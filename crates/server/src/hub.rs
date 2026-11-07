//! Çevrimiçi oturumlar ve olay dağıtımı.
//!
//! Her bağlantının, kendisine giden mesajlar için sınırlı bir kuyruğu var. Bir istemci
//! kuyruğunu boşaltamayacak kadar takılırsa (256 mesaj) bağlantısı kesilir; böylece tek bir
//! takılan istemci sunucunun belleğini şişiremez.
//!
//! Kilit sırası her zaman `inner` → `db`; tersi yapılmaz.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::{Mutex, MutexGuard};

use anyhow::Result;
use base64::Engine;
use corddis_proto::control::{ChannelId, ChannelKind, ClientMsg, ServerMsg, User, UserId};
use corddis_proto::voice::{
    ClientHeader, PacketType, ReplayWindow, ServerHeader, VoiceCipher, SERVER_HEADER_LEN, TAG_LEN, VOICE_SEQ_LEN,
};
use tokio::sync::mpsc;

use crate::db::{Db, UserRow};
use crate::log;

pub const MAX_TEXT_CHARS: usize = 2000;
pub const MAX_HISTORY: u32 = 100;
const QUEUE: usize = 256;

struct Session {
    conn: u64,
    tx: mpsc::Sender<ServerMsg>,
    muted: bool,
    deafened: bool,
    voice: Option<ChannelId>,
    // --- UDP ses (MVP.md §7.2) ---
    udp_session: u32,
    cipher: VoiceCipher,
    /// İlk geçerli UDP paketinden öğrenilir; NAT yenilenirse güncellenir.
    udp_addr: Option<SocketAddr>,
    replay: ReplayWindow,
    /// Bu alıcıya giden paketlerin nonce sayacı.
    out_counter: u64,
}

#[derive(Default)]
struct Inner {
    sessions: HashMap<UserId, Session>,
    /// UDP session id → kullanıcı.
    udp_index: HashMap<u32, UserId>,
    next_conn: u64,
}

/// Sayaçlar (sunucu logu için).
#[derive(Default, Debug, Clone, Copy)]
pub struct RelayStats {
    pub received: u64,
    pub forwarded: u64,
    pub rejected: u64,
}

pub struct Hub {
    db: Mutex<Db>,
    inner: Mutex<Inner>,
    udp_port: u16,
    relay_stats: Mutex<RelayStats>,
}

pub struct Registered {
    pub conn: u64,
    pub rx: mpsc::Receiver<ServerMsg>,
    pub auth_ok: ServerMsg,
    pub snapshot: ServerMsg,
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("rastgele sayı üretilemedi: {e}"))?;
    Ok(b)
}

fn err(code: &str) -> ServerMsg {
    ServerMsg::Error { code: code.to_string() }
}

impl Hub {
    pub fn new(db: Db, udp_port: u16) -> Self {
        Self {
            db: Mutex::new(db),
            inner: Mutex::new(Inner::default()),
            udp_port,
            relay_stats: Mutex::new(RelayStats::default()),
        }
    }

    /// Son çağrıdan beri biriken UDP sayaçlarını verir ve sıfırlar.
    pub fn take_relay_stats(&self) -> RelayStats {
        std::mem::take(&mut *self.relay_stats.lock().unwrap_or_else(|p| p.into_inner()))
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn db(&self) -> MutexGuard<'_, Db> {
        self.db.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn authenticate(&self, token: &str) -> Result<Option<UserRow>> {
        self.db().user_by_token(token)
    }

    /// Oturumu kaydeder. Aynı kullanıcı zaten bağlıysa eski bağlantı düşürülür.
    pub fn register(&self, user: &UserRow) -> Result<Registered> {
        let (tx, rx) = mpsc::channel(QUEUE);
        let udp_key = random::<32>()?;

        let mut inner = self.inner();
        let udp_session = loop {
            let id = u32::from_be_bytes(random::<4>()?);
            if id != 0 && !inner.udp_index.contains_key(&id) {
                break id;
            }
        };
        let conn = inner.next_conn;
        inner.next_conn += 1;
        let old = remove_session(&mut inner, user.id);
        inner.udp_index.insert(udp_session, user.id);
        inner.sessions.insert(
            user.id,
            Session {
                conn,
                tx,
                muted: false,
                deafened: false,
                voice: None,
                udp_session,
                cipher: VoiceCipher::new(&udp_key),
                udp_addr: None,
                replay: ReplayWindow::default(),
                out_counter: 0,
            },
        );
        if let Some(old) = old {
            log!("{} yeniden bağlandı, eski bağlantı kapatılıyor", user.name);
            if let Some(ch) = old.voice {
                broadcast(&mut inner, ServerMsg::VoiceLeave { channel_id: ch, user_id: user.id }, None);
            }
        } else {
            broadcast(&mut inner, ServerMsg::UserOnline { user_id: user.id, online: true }, Some(user.id));
        }
        let snapshot = self.snapshot(&inner)?;
        Ok(Registered {
            conn,
            rx,
            auth_ok: ServerMsg::AuthOk {
                user_id: user.id,
                udp_session,
                udp_key: base64::engine::general_purpose::STANDARD.encode(udp_key),
                udp_port: self.udp_port,
            },
            snapshot,
        })
    }

    /// Bağlantı kapandığında çağrılır. Bu bağlantı yerine yenisi geldiyse hiçbir şey yapmaz.
    pub fn unregister(&self, user: UserId, conn: u64) {
        let mut inner = self.inner();
        if inner.sessions.get(&user).is_some_and(|s| s.conn == conn) {
            let s = remove_session(&mut inner, user).unwrap();
            announce_gone(&mut inner, user, s.voice);
        }
    }

    /// Bir UDP paketini işler (MVP.md §7.2). Gönderilecek paketleri `out`'a ekler.
    /// Kimliği doğrulanamayan paketler sessizce atılır (saldırgana cevap verilmez).
    pub fn relay(&self, packet: &mut [u8], from: SocketAddr, out: &mut Vec<(SocketAddr, Vec<u8>)>) {
        let before = out.len();
        let ok = self.relay_inner(packet, from, out);
        let mut st = self.relay_stats.lock().unwrap_or_else(|p| p.into_inner());
        st.received += 1;
        st.forwarded += (out.len() - before) as u64;
        st.rejected += (!ok) as u64;
    }

    fn relay_inner(&self, packet: &mut [u8], from: SocketAddr, out: &mut Vec<(SocketAddr, Vec<u8>)>) -> bool {
        let Ok(sid) = ClientHeader::peek_session(packet) else { return false };
        let mut inner = self.inner();
        let Some(&uid) = inner.udp_index.get(&sid) else { return false };
        let Some(s) = inner.sessions.get_mut(&uid) else { return false };
        let Ok((h, body)) = s.cipher.open_client(packet) else { return false };
        if !s.replay.accept(h.seq as u64) {
            return false;
        }
        s.udp_addr = Some(from);

        match h.ty {
            PacketType::Keepalive => {
                let reply = ServerHeader { ty: PacketType::Keepalive, speaker: uid, seq: h.seq, counter: s.out_counter };
                s.out_counter += 1;
                let mut buf = vec![0u8; SERVER_HEADER_LEN + TAG_LEN];
                if s.cipher.seal_server(&reply, &[], &mut buf).is_ok() {
                    out.push((from, buf));
                }
                true
            }
            PacketType::Voice | PacketType::EndOfSpeech => {
                if body.len() < VOICE_SEQ_LEN {
                    return false;
                }
                let voice_seq = u32::from_be_bytes(body[..VOICE_SEQ_LEN].try_into().unwrap());
                let opus = &body[VOICE_SEQ_LEN..];
                let Some(channel) = s.voice else { return true };
                // Mute'lu kullanıcının sesi sunucu tarafında da kesilir.
                if h.ty == PacketType::Voice && (s.muted || opus.is_empty()) {
                    return true;
                }
                for (&rid, r) in inner.sessions.iter_mut() {
                    if rid == uid || r.voice != Some(channel) || r.deafened {
                        continue;
                    }
                    let Some(addr) = r.udp_addr else { continue };
                    let hdr = ServerHeader { ty: h.ty, speaker: uid, seq: voice_seq, counter: r.out_counter };
                    r.out_counter += 1;
                    let mut buf = vec![0u8; SERVER_HEADER_LEN + opus.len() + TAG_LEN];
                    if r.cipher.seal_server(&hdr, opus, &mut buf).is_ok() {
                        out.push((addr, buf));
                    }
                }
                true
            }
        }
    }

    fn snapshot(&self, inner: &Inner) -> Result<ServerMsg> {
        let db = self.db();
        let users = db
            .users()?
            .into_iter()
            .map(|u| {
                let s = inner.sessions.get(&u.id);
                User {
                    id: u.id,
                    name: u.name,
                    online: s.is_some(),
                    muted: s.is_some_and(|s| s.muted),
                    deafened: s.is_some_and(|s| s.deafened),
                }
            })
            .collect();
        let mut voice: BTreeMap<ChannelId, Vec<UserId>> = BTreeMap::new();
        for (&id, s) in &inner.sessions {
            if let Some(ch) = s.voice {
                voice.entry(ch).or_default().push(id);
            }
        }
        voice.values_mut().for_each(|v| v.sort());
        Ok(ServerMsg::Snapshot { channels: db.channels()?, users, voice_members: voice.into_iter().collect() })
    }

    /// İstemciden gelen bir mesajı işler. Yanıtlar ve olaylar kuyruklar üzerinden gider.
    pub fn handle(&self, user: UserId, conn: u64, msg: ClientMsg) -> Result<()> {
        match msg {
            ClientMsg::Auth { .. } => self.reply(user, conn, err("already_authenticated")),

            ClientMsg::SendMsg { channel_id, text } => {
                let text = text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
                if text.is_empty() || text.chars().count() > MAX_TEXT_CHARS {
                    return self.reply(user, conn, err("bad_text"));
                }
                let msg = {
                    let db = self.db();
                    if !db.channel(channel_id)?.is_some_and(|c| c.kind == ChannelKind::Text) {
                        drop(db);
                        return self.reply(user, conn, err("bad_channel"));
                    }
                    db.insert_message(channel_id, user, text)?
                };
                broadcast(&mut self.inner(), ServerMsg::Msg(msg), None);
                Ok(())
            }

            ClientMsg::History { channel_id, before_id, limit } => {
                let reply = {
                    let db = self.db();
                    if db.channel(channel_id)?.is_some_and(|c| c.kind == ChannelKind::Text) {
                        let messages = db.history(channel_id, before_id, limit.clamp(1, MAX_HISTORY))?;
                        ServerMsg::HistoryResult { channel_id, messages }
                    } else {
                        err("bad_channel")
                    }
                };
                self.reply(user, conn, reply)
            }

            ClientMsg::JoinVoice { channel_id } => {
                let ok = self.db().channel(channel_id)?.is_some_and(|c| c.kind == ChannelKind::Voice);
                let mut inner = self.inner();
                if !ok {
                    send_to(&mut inner, user, conn, err("bad_channel"));
                    return Ok(());
                }
                let Some(s) = own(&mut inner, user, conn) else { return Ok(()) };
                let prev = s.voice.replace(channel_id);
                if prev == Some(channel_id) {
                    return Ok(());
                }
                if let Some(p) = prev {
                    broadcast(&mut inner, ServerMsg::VoiceLeave { channel_id: p, user_id: user }, None);
                }
                broadcast(&mut inner, ServerMsg::VoiceJoin { channel_id, user_id: user }, None);
                Ok(())
            }

            ClientMsg::LeaveVoice => {
                let mut inner = self.inner();
                let Some(s) = own(&mut inner, user, conn) else { return Ok(()) };
                if let Some(p) = s.voice.take() {
                    broadcast(&mut inner, ServerMsg::VoiceLeave { channel_id: p, user_id: user }, None);
                }
                Ok(())
            }

            ClientMsg::SetState { muted, deafened } => {
                let mut inner = self.inner();
                let Some(s) = own(&mut inner, user, conn) else { return Ok(()) };
                // Sağır olan biri konuşamaz.
                let muted = muted || deafened;
                if (s.muted, s.deafened) == (muted, deafened) {
                    return Ok(());
                }
                (s.muted, s.deafened) = (muted, deafened);
                broadcast(&mut inner, ServerMsg::UserState { user_id: user, muted, deafened }, None);
                Ok(())
            }
        }
    }

    fn reply(&self, user: UserId, conn: u64, msg: ServerMsg) -> Result<()> {
        send_to(&mut self.inner(), user, conn, msg);
        Ok(())
    }
}

/// Kullanıcının bu bağlantıya ait oturumu (araya yeni bir bağlantı girdiyse `None`).
fn own(inner: &mut Inner, user: UserId, conn: u64) -> Option<&mut Session> {
    inner.sessions.get_mut(&user).filter(|s| s.conn == conn)
}

fn send_to(inner: &mut Inner, user: UserId, conn: u64, msg: ServerMsg) {
    if let Some(s) = own(inner, user, conn) {
        if s.tx.try_send(msg).is_err() {
            drop_session(inner, user);
        }
    }
}

fn broadcast(inner: &mut Inner, msg: ServerMsg, except: Option<UserId>) {
    let mut stuck = Vec::new();
    for (&id, s) in &inner.sessions {
        if Some(id) == except {
            continue;
        }
        if let Err(mpsc::error::TrySendError::Full(_)) = s.tx.try_send(msg.clone()) {
            stuck.push(id);
        }
        // Closed: bağlantı zaten kapanıyor, kendi unregister'ı temizleyecek.
    }
    for id in stuck {
        drop_session(inner, id);
    }
}

fn remove_session(inner: &mut Inner, user: UserId) -> Option<Session> {
    let s = inner.sessions.remove(&user)?;
    inner.udp_index.remove(&s.udp_session);
    Some(s)
}

fn drop_session(inner: &mut Inner, user: UserId) {
    if let Some(s) = remove_session(inner, user) {
        log!("kullanıcı {user}: kuyruk doldu, bağlantı kesiliyor");
        announce_gone(inner, user, s.voice);
    }
}

fn announce_gone(inner: &mut Inner, user: UserId, voice: Option<ChannelId>) {
    if let Some(ch) = voice {
        broadcast(inner, ServerMsg::VoiceLeave { channel_id: ch, user_id: user }, None);
    }
    broadcast(inner, ServerMsg::UserOnline { user_id: user, online: false }, None);
}

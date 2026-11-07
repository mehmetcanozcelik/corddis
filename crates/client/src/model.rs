//! Arayüzün gösterdiği durum. Ağ thread'i sunucu mesajlarını buraya uygular,
//! arayüz her karede okur. Saf veri yapısı: ağ ve cihazdan bağımsız, test edilebilir.

use std::collections::{BTreeMap, HashMap, HashSet};

use corddis_proto::control::{Channel, ChannelId, ChannelKind, ChatMessage, MessageId, ServerMsg, User, UserId};

/// Kanal başına bellekte tutulan en fazla mesaj (MVP.md §8: RAM sınırı).
pub const MAX_MESSAGES_PER_CHANNEL: usize = 500;
pub const HISTORY_PAGE: u32 = 50;

#[derive(Debug, Clone, PartialEq)]
pub enum Conn {
    Connecting,
    Online,
    Offline { reason: String },
    /// Davet bağlantısı geçersiz: yeniden denemenin anlamı yok.
    BadInvite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpInfo {
    pub session: u32,
    pub key: [u8; 32],
    pub port: u16,
}

/// Kullanıcının istediği durum. Bağlantı koparsa yeniden bağlanınca geri yüklenir.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Desired {
    pub voice: Option<ChannelId>,
    pub muted: bool,
    pub deafened: bool,
}

#[derive(Debug)]
pub struct Model {
    pub conn: Conn,
    pub me: Option<UserId>,
    pub udp: Option<UdpInfo>,
    pub users: BTreeMap<UserId, User>,
    pub channels: Vec<Channel>,
    pub voice: BTreeMap<ChannelId, Vec<UserId>>,
    pub messages: HashMap<ChannelId, Vec<ChatMessage>>,
    /// Bu kanalın en eski mesajına kadar inildi.
    pub history_done: HashSet<ChannelId>,
    pub unread: HashMap<ChannelId, u32>,
    /// Arayüzde açık olan yazı kanalı (okunmamış sayacı buna göre).
    pub viewing: Option<ChannelId>,
    pub desired: Desired,
    pub last_error: Option<String>,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            conn: Conn::Connecting,
            me: None,
            udp: None,
            users: BTreeMap::new(),
            channels: Vec::new(),
            voice: BTreeMap::new(),
            messages: HashMap::new(),
            history_done: HashSet::new(),
            unread: HashMap::new(),
            viewing: None,
            desired: Desired::default(),
            last_error: None,
        }
    }
}

/// `apply` sonrası ağ thread'inin yapması gerekenler.
#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    None,
    /// Yeni oturum: geçmişi iste, istenen sesli kanal/mute durumunu geri yükle.
    Resync,
    BadInvite,
}

impl Model {
    pub fn my_voice(&self) -> Option<ChannelId> {
        let me = self.me?;
        self.voice.iter().find(|(_, m)| m.contains(&me)).map(|(&c, _)| c)
    }

    pub fn name(&self, id: UserId) -> &str {
        self.users.get(&id).map(|u| u.name.as_str()).unwrap_or("?")
    }

    pub fn text_channels(&self) -> impl Iterator<Item = &Channel> {
        self.channels.iter().filter(|c| c.kind == ChannelKind::Text)
    }

    pub fn voice_channels(&self) -> impl Iterator<Item = &Channel> {
        self.channels.iter().filter(|c| c.kind == ChannelKind::Voice)
    }

    pub fn oldest_message(&self, ch: ChannelId) -> Option<MessageId> {
        self.messages.get(&ch).and_then(|v| v.first()).map(|m| m.id)
    }

    pub fn set_viewing(&mut self, ch: Option<ChannelId>) {
        self.viewing = ch;
        if let Some(c) = ch {
            self.unread.remove(&c);
        }
    }

    pub fn apply(&mut self, m: ServerMsg) -> Effect {
        match m {
            ServerMsg::AuthOk { user_id, udp_session, udp_key, udp_port } => {
                use base64::Engine;
                self.me = Some(user_id);
                self.udp = base64::engine::general_purpose::STANDARD
                    .decode(udp_key)
                    .ok()
                    .and_then(|k| <[u8; 32]>::try_from(k).ok())
                    .map(|key| UdpInfo { session: udp_session, key, port: udp_port });
            }
            ServerMsg::Snapshot { channels, users, voice_members } => {
                self.users = users.into_iter().map(|u| (u.id, u)).collect();
                self.voice = voice_members.into_iter().collect();
                // Mesajlar yeniden bağlanınca baştan yüklenir (arada kaçanlar dahil).
                self.messages.clear();
                self.history_done.clear();
                self.unread.clear();
                if !self.viewing.is_some_and(|v| channels.iter().any(|c| c.id == v && c.kind == ChannelKind::Text)) {
                    self.viewing = channels.iter().find(|c| c.kind == ChannelKind::Text).map(|c| c.id);
                }
                self.channels = channels;
                self.conn = Conn::Online;
                return Effect::Resync;
            }
            ServerMsg::Msg(msg) => {
                let ch = msg.channel_id;
                let list = self.messages.entry(ch).or_default();
                if !list.iter().any(|m| m.id == msg.id) {
                    list.push(msg);
                    if list.len() > MAX_MESSAGES_PER_CHANNEL {
                        list.remove(0);
                        self.history_done.remove(&ch);
                    }
                    if self.viewing != Some(ch) {
                        *self.unread.entry(ch).or_default() += 1;
                    }
                }
            }
            ServerMsg::HistoryResult { channel_id, messages } => {
                if (messages.len() as u32) < HISTORY_PAGE {
                    self.history_done.insert(channel_id);
                }
                let list = self.messages.entry(channel_id).or_default();
                let mut merged: Vec<ChatMessage> =
                    messages.into_iter().filter(|m| !list.iter().any(|x| x.id == m.id)).collect();
                merged.append(list);
                merged.sort_by_key(|m| m.id);
                *list = merged;
            }
            ServerMsg::VoiceJoin { channel_id, user_id } => {
                for members in self.voice.values_mut() {
                    members.retain(|&u| u != user_id);
                }
                let members = self.voice.entry(channel_id).or_default();
                members.push(user_id);
                members.sort();
            }
            ServerMsg::VoiceLeave { channel_id, user_id } => {
                if let Some(members) = self.voice.get_mut(&channel_id) {
                    members.retain(|&u| u != user_id);
                }
            }
            ServerMsg::UserState { user_id, muted, deafened } => {
                if let Some(u) = self.users.get_mut(&user_id) {
                    (u.muted, u.deafened) = (muted, deafened);
                }
            }
            ServerMsg::UserOnline { user_id, online } => {
                if let Some(u) = self.users.get_mut(&user_id) {
                    u.online = online;
                    if !online {
                        (u.muted, u.deafened) = (false, false);
                    }
                }
            }
            ServerMsg::Error { code } => {
                if code == "bad_token" {
                    self.conn = Conn::BadInvite;
                    return Effect::BadInvite;
                }
                self.last_error = Some(match code.as_str() {
                    "bad_text" => "Mesaj boş ya da çok uzun (en fazla 2000 karakter).".into(),
                    "bad_channel" => "Kanal bulunamadı.".into(),
                    _ => format!("Sunucu hatası: {code}"),
                });
            }
        }
        Effect::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: UserId, name: &str) -> User {
        User { id, name: name.into(), online: true, muted: false, deafened: false }
    }

    fn msg(id: i64, ch: ChannelId, text: &str) -> ChatMessage {
        ChatMessage { id, channel_id: ch, user_id: 1, text: text.into(), ts: 0 }
    }

    fn connected() -> Model {
        let mut m = Model::default();
        m.apply(ServerMsg::AuthOk { user_id: 1, udp_session: 9, udp_key: "A".repeat(43) + "=", udp_port: 7777 });
        let e = m.apply(ServerMsg::Snapshot {
            channels: vec![
                Channel { id: 1, name: "genel".into(), kind: ChannelKind::Text },
                Channel { id: 2, name: "Oyun".into(), kind: ChannelKind::Voice },
                Channel { id: 3, name: "duyuru".into(), kind: ChannelKind::Text },
            ],
            users: vec![user(1, "MoonDust"), user(2, "Yamakasi")],
            voice_members: vec![(2, vec![2])],
        });
        assert_eq!(e, Effect::Resync);
        m
    }

    #[test]
    fn snapshot_sets_state_and_defaults_to_first_text_channel() {
        let m = connected();
        assert_eq!(m.conn, Conn::Online);
        assert_eq!(m.viewing, Some(1));
        assert_eq!(m.udp.as_ref().unwrap().port, 7777);
        assert_eq!(m.my_voice(), None);
        assert_eq!(m.name(2), "Yamakasi");
    }

    #[test]
    fn voice_join_moves_user_between_channels() {
        let mut m = connected();
        m.apply(ServerMsg::VoiceJoin { channel_id: 2, user_id: 1 });
        assert_eq!(m.my_voice(), Some(2));
        assert_eq!(m.voice[&2], vec![1, 2]);
        m.apply(ServerMsg::VoiceLeave { channel_id: 2, user_id: 1 });
        assert_eq!(m.my_voice(), None);
    }

    #[test]
    fn unread_counts_only_other_channels_and_dedupes() {
        let mut m = connected();
        m.apply(ServerMsg::Msg(msg(10, 1, "a")));
        m.apply(ServerMsg::Msg(msg(11, 3, "b")));
        m.apply(ServerMsg::Msg(msg(11, 3, "b")));
        assert_eq!(m.unread.get(&1), None);
        assert_eq!(m.unread[&3], 1);
        assert_eq!(m.messages[&3].len(), 1);
        m.set_viewing(Some(3));
        assert!(m.unread.is_empty());
    }

    #[test]
    fn history_merges_in_order_and_marks_done() {
        let mut m = connected();
        m.apply(ServerMsg::Msg(msg(20, 1, "yeni")));
        m.apply(ServerMsg::HistoryResult { channel_id: 1, messages: vec![msg(5, 1, "eski"), msg(20, 1, "yeni")] });
        let texts: Vec<_> = m.messages[&1].iter().map(|x| x.text.as_str()).collect();
        assert_eq!(texts, ["eski", "yeni"]);
        assert!(m.history_done.contains(&1));
        assert_eq!(m.oldest_message(1), Some(5));
    }

    #[test]
    fn bad_token_stops_reconnecting() {
        let mut m = Model::default();
        assert_eq!(m.apply(ServerMsg::Error { code: "bad_token".into() }), Effect::BadInvite);
        assert_eq!(m.conn, Conn::BadInvite);
    }
}

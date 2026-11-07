//! Komut satırından sohbet + sesli kanal. Sunucuyu ve protokolü arayüz olmadan denemek için;
//! arayüz (M6) aynı `net::connect`, aynı mesajlar ve aynı `VoiceEngine` ile çalışacak.

use std::collections::HashMap;
use std::net::ToSocketAddrs;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use base64::Engine;
use corddis_proto::control::{Channel, ChannelId, ChannelKind, ChatMessage, ClientMsg, ServerMsg, UserId};
use corddis_proto::invite::Invite;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::voice::{VoiceConfig, VoiceEngine};
use crate::{net, AudioOptions};

const PING_EVERY: Duration = Duration::from_secs(30);
const SPEAKING_POLL: Duration = Duration::from_millis(200);

const COMMANDS: &str = "\
Komutlar:
  <metin>               Seçili yazı kanalına mesaj gönder
  /kanal <ad>           Yazı kanalını değiştir
  /gecmis               Seçili kanalın son 20 mesajı
  /katil <ad>           Sesli kanala katıl (ses başlar)
  /ayril                Sesli kanaldan ayrıl
  /mute, /unmute        Mikrofonu kapat/aç
  /sagir                Sağır modunu aç/kapat (hiçbir şey duymaz, konuşamazsın)
  /seviye <ad> <0-200>  Bir kişinin ses düzeyi (yüzde)
  /ses                  Ses bağlantısı durumu (ping, paketler, kayıplar)
  /kim                  Kullanıcılar ve sesli kanallar
  /cik                  Çıkış";

pub fn run(invite: &str, audio: AudioOptions) -> Result<()> {
    let inv: Invite = invite.parse()?;
    tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(chat(inv, audio))
}

struct UdpInfo {
    session: u32,
    key: [u8; 32],
    port: u16,
}

struct View {
    host: String,
    audio: AudioOptions,
    me: UserId,
    names: HashMap<UserId, String>,
    online: HashMap<UserId, bool>,
    channels: Vec<Channel>,
    voice: HashMap<ChannelId, Vec<UserId>>,
    text_channel: Option<ChannelId>,
    udp: Option<UdpInfo>,
    engine: Option<VoiceEngine>,
    muted: bool,
    deafened: bool,
    volumes: HashMap<UserId, f32>,
    last_speaking: (bool, Vec<u16>),
    proc_stats: crate::platform::ProcStats,
}

impl View {
    fn name(&self, id: UserId) -> &str {
        self.names.get(&id).map(String::as_str).unwrap_or("?")
    }
    fn user_by_name(&self, name: &str) -> Option<UserId> {
        self.names.iter().find(|(_, n)| n.eq_ignore_ascii_case(name)).map(|(&id, _)| id)
    }
    fn channel_name(&self, id: ChannelId) -> &str {
        self.channels.iter().find(|c| c.id == id).map(|c| c.name.as_str()).unwrap_or("?")
    }
    fn find(&self, name: &str, kind: ChannelKind) -> Option<ChannelId> {
        self.channels.iter().find(|c| c.kind == kind && c.name.eq_ignore_ascii_case(name)).map(|c| c.id)
    }
    fn print_msg(&self, m: &ChatMessage) {
        // Türkiye yıl boyu UTC+3 (yaz saati yok).
        let t = m.ts + 3 * 3600;
        println!("[{:02}:{:02}] #{} {}: {}", (t / 3600) % 24, (t / 60) % 60, self.channel_name(m.channel_id), self.name(m.user_id), m.text);
    }
    fn print_who(&self) {
        let on: Vec<&str> = self.names.keys().filter(|id| self.online.get(id) == Some(&true)).map(|&id| self.name(id)).collect();
        println!("Çevrimiçi: {}", if on.is_empty() { "-".into() } else { on.join(", ") });
        for c in self.channels.iter().filter(|c| c.kind == ChannelKind::Voice) {
            let members: Vec<&str> = self.voice.get(&c.id).into_iter().flatten().map(|&id| self.name(id)).collect();
            println!("  🔊 {}: {}", c.name, if members.is_empty() { "-".into() } else { members.join(", ") });
        }
    }

    fn start_voice(&mut self) {
        if self.engine.is_some() {
            return;
        }
        let Some(udp) = &self.udp else { return };
        let server = match (self.host.as_str(), udp.port).to_socket_addrs().ok().and_then(|mut a| a.next()) {
            Some(a) => a,
            None => {
                println!("(ses: sunucu adresi çözülemedi)");
                return;
            }
        };
        let cfg = VoiceConfig {
            server,
            session: udp.session,
            key: udp.key,
            input: self.audio.input.clone(),
            output: self.audio.output.clone(),
            capture: self.audio.capture.clone(),
        };
        match VoiceEngine::start(cfg) {
            Ok(e) => {
                e.set_muted(self.muted);
                e.set_deafened(self.deafened);
                for (&u, &v) in &self.volumes {
                    e.set_volume(u, v);
                }
                println!("🎙 Ses açık — mikrofon: {}, çıkış: {}", e.input_name, e.output_name);
                self.engine = Some(e);
            }
            Err(e) => println!("(ses başlatılamadı: {e:#})"),
        }
    }

    fn stop_voice(&mut self) {
        if self.engine.take().is_some() {
            println!("🎙 Ses kapandı");
        }
        self.last_speaking = (false, Vec::new());
    }

    fn print_voice_status(&mut self) {
        let p = self.proc_stats.sample();
        let Some(e) = &self.engine else {
            println!("Sesli kanalda değilsin. CPU %{:.2} (son /ses'ten beri, tek çekirdek) | RAM {:.1} MB", p.cpu_pct, p.working_set_mb);
            return;
        };
        let s = e.status();
        println!(
            "CPU %{:.2} (son /ses'ten beri, tek çekirdek) | RAM {:.1} MB (private {:.1})",
            p.cpu_pct, p.working_set_mb, p.private_mb
        );
        println!(
            "Ping: {} | gönderilen {} pkt, alınan {} pkt, reddedilen {} | çalma boşalması {}{}{}",
            s.rtt_ms.map(|r| format!("{r} ms")).unwrap_or_else(|| "ölçülmedi (UDP ulaşmıyor olabilir)".into()),
            s.tx_packets,
            s.rx_packets,
            s.rx_rejected,
            s.underruns,
            if self.muted { " | mikrofon kapalı" } else { "" },
            if self.deafened { " | sağır" } else { "" },
        );
        for (id, st) in s.speakers {
            println!(
                "  {}: {} çerçeve, kayıp → FEC {} / PLC {}, geç {} | jitter {:.1} ms → tampon {} ms",
                self.name(id),
                st.frames,
                st.fec,
                st.plc,
                st.late,
                st.jitter_ms,
                st.target_frames * 20
            );
        }
    }

    /// Konuşanlar değiştiyse tek satır yazar.
    fn poll_speaking(&mut self) {
        let Some(e) = &self.engine else { return };
        let s = e.status();
        let now = (s.self_talking, s.speaking);
        if now == self.last_speaking {
            return;
        }
        let mut names: Vec<String> = now.1.iter().map(|&id| self.name(id).to_string()).collect();
        if now.0 {
            names.insert(0, format!("{} (sen)", self.name(self.me)));
        }
        println!("🗣 {}", if names.is_empty() { "(sessiz)".into() } else { names.join(", ") });
        self.last_speaking = now;
    }
}

async fn chat(inv: Invite, audio: AudioOptions) -> Result<()> {
    println!("{}:{} adresine bağlanılıyor...", inv.host, inv.port);
    let ws = net::connect(&inv).await?;
    let (mut sink, mut stream) = ws.split();
    let send = |m: &ClientMsg| Message::text(serde_json::to_string(m).unwrap());
    sink.send(send(&ClientMsg::Auth { token: inv.token.clone() })).await?;

    let mut view = View {
        host: inv.host.clone(),
        audio,
        me: 0,
        names: HashMap::new(),
        online: HashMap::new(),
        channels: Vec::new(),
        voice: HashMap::new(),
        text_channel: None,
        udp: None,
        engine: None,
        muted: false,
        deafened: false,
        volumes: HashMap::new(),
        last_speaking: (false, Vec::new()),
        proc_stats: crate::platform::ProcStats::new(),
    };

    let (line_tx, mut lines) = mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines().map_while(Result::ok) {
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });

    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    let mut poll = tokio::time::interval(SPEAKING_POLL);

    loop {
        tokio::select! {
            _ = ping.tick() => sink.send(Message::Ping(Default::default())).await?,
            _ = poll.tick() => view.poll_speaking(),

            line = lines.recv() => {
                let Some(line) = line else { break };
                // Windows'ta yönlendirilen girdinin başında görünmez UTF-8 BOM olabilir.
                let line = line.trim_start_matches('\u{feff}').trim();
                if line.is_empty() { continue; }
                let (cmd, arg) = line.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((line, ""));
                let msg = match cmd {
                    "/cik" => break,
                    "/kim" => { view.print_who(); None }
                    "/ses" => { view.print_voice_status(); None }
                    "/yardim" | "/?" => { println!("{COMMANDS}"); None }
                    "/kanal" => match view.find(arg, ChannelKind::Text) {
                        Some(id) => { view.text_channel = Some(id); println!("Yazı kanalı: #{}", view.channel_name(id)); None }
                        None => { println!("'{arg}' adında yazı kanalı yok"); None }
                    },
                    "/gecmis" => view.text_channel.map(|c| ClientMsg::History { channel_id: c, before_id: None, limit: 20 }),
                    "/katil" => match view.find(arg, ChannelKind::Voice) {
                        Some(id) => Some(ClientMsg::JoinVoice { channel_id: id }),
                        None => { println!("'{arg}' adında sesli kanal yok"); None }
                    },
                    "/ayril" => Some(ClientMsg::LeaveVoice),
                    "/mute" | "/unmute" | "/sagir" => {
                        match cmd {
                            "/mute" => view.muted = true,
                            "/unmute" => { view.muted = false; view.deafened = false; }
                            _ => view.deafened = !view.deafened,
                        }
                        if let Some(e) = &view.engine {
                            e.set_muted(view.muted);
                            e.set_deafened(view.deafened);
                        }
                        Some(ClientMsg::SetState { muted: view.muted, deafened: view.deafened })
                    }
                    "/seviye" => {
                        let parsed = arg.rsplit_once(' ').and_then(|(n, p)| Some((view.user_by_name(n.trim())?, p.parse::<f32>().ok()?)));
                        match parsed {
                            Some((id, pct)) => {
                                let v = (pct / 100.0).clamp(0.0, 2.0);
                                view.volumes.insert(id, v);
                                if let Some(e) = &view.engine { e.set_volume(id, v); }
                                println!("{}: %{:.0}", view.name(id), v * 100.0);
                            }
                            None => println!("Kullanım: /seviye <ad> <0-200>"),
                        }
                        None
                    }
                    c if c.starts_with('/') => { println!("Bilinmeyen komut. /yardim yaz."); None }
                    _ => match view.text_channel {
                        Some(c) => Some(ClientMsg::SendMsg { channel_id: c, text: line.to_string() }),
                        None => { println!("Yazı kanalı yok"); None }
                    },
                };
                if let Some(m) = msg {
                    sink.send(send(&m)).await?;
                }
            }

            incoming = stream.next() => match incoming {
                None => { println!("Bağlantı kapandı."); break }
                Some(Err(e)) => bail!("bağlantı hatası: {e}"),
                Some(Ok(Message::Text(t))) => match serde_json::from_str::<ServerMsg>(&t) {
                    Ok(m) => on_server_msg(&mut view, m)?,
                    Err(e) => println!("(anlaşılamayan sunucu mesajı: {e})"),
                },
                Some(Ok(_)) => {}
            },
        }
    }
    view.stop_voice();
    sink.close().await.ok();
    Ok(())
}

fn on_server_msg(v: &mut View, m: ServerMsg) -> Result<()> {
    match m {
        ServerMsg::AuthOk { user_id, udp_session, udp_key, udp_port } => {
            v.me = user_id;
            let key = base64::engine::general_purpose::STANDARD.decode(udp_key).context("udp_key")?;
            let key: [u8; 32] = key.try_into().map_err(|_| anyhow::anyhow!("udp_key 32 byte değil"))?;
            v.udp = Some(UdpInfo { session: udp_session, key, port: udp_port });
        }
        ServerMsg::Snapshot { channels, users, voice_members } => {
            v.names = users.iter().map(|u| (u.id, u.name.clone())).collect();
            v.online = users.iter().map(|u| (u.id, u.online)).collect();
            v.voice = voice_members.into_iter().collect();
            v.text_channel = channels.iter().find(|c| c.kind == ChannelKind::Text).map(|c| c.id);
            v.channels = channels;
            println!("Bağlandın: {}. Yazı kanalı: #{}", v.name(v.me), v.text_channel.map(|c| v.channel_name(c)).unwrap_or("-"));
            v.print_who();
            println!("{COMMANDS}\n");
        }
        ServerMsg::Msg(msg) => v.print_msg(&msg),
        ServerMsg::HistoryResult { messages, .. } => {
            if messages.is_empty() {
                println!("(mesaj yok)");
            }
            messages.iter().for_each(|m| v.print_msg(m));
        }
        ServerMsg::VoiceJoin { channel_id, user_id } => {
            for members in v.voice.values_mut() {
                members.retain(|&u| u != user_id);
            }
            v.voice.entry(channel_id).or_default().push(user_id);
            println!("🔊 {} → {}", v.name(user_id), v.channel_name(channel_id));
            if user_id == v.me {
                v.start_voice();
            }
        }
        ServerMsg::VoiceLeave { channel_id, user_id } => {
            if let Some(members) = v.voice.get_mut(&channel_id) {
                members.retain(|&u| u != user_id);
            }
            println!("🔇 {} ← {}", v.name(user_id), v.channel_name(channel_id));
            if user_id == v.me && !v.voice.values().any(|m| m.contains(&v.me)) {
                v.stop_voice();
            }
        }
        ServerMsg::UserState { user_id, muted, deafened } => {
            let s = if deafened { "sağır" } else if muted { "mikrofon kapalı" } else { "mikrofon açık" };
            println!("• {}: {s}", v.name(user_id));
        }
        ServerMsg::UserOnline { user_id, online } => {
            v.online.insert(user_id, online);
            println!("• {} {}", v.name(user_id), if online { "çevrimiçi" } else { "çevrimdışı" });
        }
        ServerMsg::Error { code } => {
            if code == "bad_token" {
                bail!("davet bağlantısı geçersiz (token yanlış ya da yenilenmiş)");
            }
            println!("(sunucu hatası: {code})");
        }
    }
    Ok(())
}

//! Arka planda çalışan kontrol bağlantısı: bağlanır, sunucu mesajlarını modele uygular,
//! koparsa artan bekleme süreleriyle (1, 2, 4 … 30 sn) yeniden bağlanır ve kullanıcının
//! sesli kanal / mute durumunu geri yükler (MVP.md §7.1).

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::Result;
use corddis_proto::control::{ClientMsg, ServerMsg};
use corddis_proto::invite::Invite;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::model::{Conn, Effect, Model, HISTORY_PAGE};
use crate::net;

const PING_EVERY: Duration = Duration::from_secs(30);
const MAX_BACKOFF_SECS: u64 = 30;

enum Cmd {
    Send(ClientMsg),
    Quit,
}

pub struct NetHandle {
    tx: mpsc::UnboundedSender<Cmd>,
    pub model: Arc<Mutex<Model>>,
}

impl NetHandle {
    /// Ayrı bir thread'de bağlantıyı başlatır. `repaint`, model her değiştiğinde çağrılır.
    pub fn start(invite: Invite, repaint: impl Fn() + Send + Sync + 'static) -> Self {
        let model = Arc::new(Mutex::new(Model::default()));
        let (tx, rx) = mpsc::unbounded_channel();
        let m = model.clone();
        let _ = std::thread::Builder::new().name("corddis-net".into()).spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build();
            match rt {
                Ok(rt) => rt.block_on(run(invite, m, rx, repaint)),
                Err(e) => lock(&m).conn = Conn::Offline { reason: format!("{e}") },
            }
        });
        Self { tx, model }
    }

    pub fn send(&self, m: ClientMsg) {
        let _ = self.tx.send(Cmd::Send(m));
    }

    pub fn model(&self) -> MutexGuard<'_, Model> {
        lock(&self.model)
    }
}

impl Drop for NetHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Quit);
    }
}

fn lock(m: &Mutex<Model>) -> MutexGuard<'_, Model> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

enum End {
    Quit,
    BadInvite,
    Lost(String),
}

async fn run(
    inv: Invite,
    model: Arc<Mutex<Model>>,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
    repaint: impl Fn() + Send + Sync + 'static,
) {
    let mut backoff = 1u64;
    loop {
        lock(&model).conn = Conn::Connecting;
        repaint();
        let mut reached_online = false;
        let end = match session(&inv, &model, &mut rx, &repaint, &mut reached_online).await {
            Ok(e) => e,
            Err(e) => End::Lost(format!("{e:#}")),
        };
        match end {
            End::Quit => return,
            End::BadInvite => {
                lock(&model).conn = Conn::BadInvite;
                repaint();
                // Arayüz yeni davet isteyecek; kapatılmayı bekle.
                while let Some(c) = rx.recv().await {
                    if matches!(c, Cmd::Quit) {
                        return;
                    }
                }
                return;
            }
            End::Lost(reason) => {
                if reached_online {
                    backoff = 1;
                }
                lock(&model).conn = Conn::Offline { reason };
                repaint();
            }
        }
        // Beklerken gelen komutlar atılır (bağlantı yok); kapatma hemen işlenir.
        let wait = tokio::time::sleep(Duration::from_secs(backoff));
        tokio::pin!(wait);
        loop {
            tokio::select! {
                _ = &mut wait => break,
                c = rx.recv() => if matches!(c, None | Some(Cmd::Quit)) { return },
            }
        }
        backoff = (backoff * 2).min(MAX_BACKOFF_SECS);
    }
}

async fn session(
    inv: &Invite,
    model: &Arc<Mutex<Model>>,
    rx: &mut mpsc::UnboundedReceiver<Cmd>,
    repaint: &(impl Fn() + Send + Sync),
    reached_online: &mut bool,
) -> Result<End> {
    // Bağlanma en fazla ~10 sn sürer; bu arada gelen kapatma isteği hemen sonra işlenir.
    let ws = net::connect(inv).await?;
    let (mut sink, mut stream) = ws.split();
    let text = |m: &ClientMsg| Message::text(serde_json::to_string(m).expect("serileştirme"));
    sink.send(text(&ClientMsg::Auth { token: inv.token.clone() })).await?;

    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    loop {
        tokio::select! {
            _ = ping.tick() => sink.send(Message::Ping(Default::default())).await?,
            c = rx.recv() => match c {
                None | Some(Cmd::Quit) => {
                    sink.close().await.ok();
                    return Ok(End::Quit);
                }
                Some(Cmd::Send(m)) => sink.send(text(&m)).await?,
            },
            incoming = stream.next() => {
                let t = match incoming {
                    None => return Ok(End::Lost("sunucu bağlantıyı kapattı".into())),
                    Some(Err(e)) => return Ok(End::Lost(format!("{e}"))),
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(_)) => continue,
                };
                let Ok(msg) = serde_json::from_str::<ServerMsg>(&t) else { continue };
                let (effect, resync) = {
                    let mut m = lock(model);
                    let effect = m.apply(msg);
                    let resync: Vec<ClientMsg> = if effect == Effect::Resync {
                        let mut out: Vec<ClientMsg> = m
                            .text_channels()
                            .map(|c| ClientMsg::History { channel_id: c.id, before_id: None, limit: HISTORY_PAGE })
                            .collect();
                        let d = m.desired.clone();
                        if d.muted || d.deafened {
                            out.push(ClientMsg::SetState { muted: d.muted, deafened: d.deafened });
                        }
                        if let Some(ch) = d.voice {
                            out.push(ClientMsg::JoinVoice { channel_id: ch });
                        }
                        out
                    } else {
                        Vec::new()
                    };
                    (effect, resync)
                };
                repaint();
                match effect {
                    Effect::BadInvite => return Ok(End::BadInvite),
                    Effect::Resync => {
                        *reached_online = true;
                        for m in &resync {
                            sink.send(text(m)).await?;
                        }
                    }
                    Effect::None => {}
                }
            }
        }
    }
}

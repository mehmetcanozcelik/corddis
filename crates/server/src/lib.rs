//! Corddis sunucusu: TLS üzerinden WebSocket kontrol kanalı (MVP.md §4, §7.1)
//! ve UDP ses relay'i (§7.2).

pub mod db;
pub mod hub;
pub mod tls;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use corddis_proto::control::{ClientMsg, ServerMsg};
use futures_util::{Sink, SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

use crate::hub::Hub;

/// Zaman damgalı stderr logu (systemd journal'a gider).
#[macro_export]
macro_rules! log {
    ($($t:tt)*) => {{
        let s = $crate::db::now_secs();
        eprintln!("[{:02}:{:02}:{:02} UTC] {}",(s / 3600) % 24, (s / 60) % 60, s % 60, format!($($t)*));
    }};
}

/// TLS + WebSocket el sıkışması ve ilk `auth` mesajı için süre.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// İstemci 30 sn'de bir ping atar; bu sürede hiçbir şey gelmezse bağlantı ölü sayılır.
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);
const MAX_WS_MESSAGE: usize = 64 * 1024;

fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default().max_message_size(Some(MAX_WS_MESSAGE)).max_frame_size(Some(MAX_WS_MESSAGE))
}

/// Bağlantıları kabul eder; asla dönmez (yalnızca listener hatasında).
pub async fn serve(listener: TcpListener, acceptor: TlsAcceptor, hub: Arc<Hub>) -> Result<()> {
    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                // Örn. dosya tanıtıcı sınırı: kısa bekleyip devam et.
                log!("accept hatası: {e}");
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
        };
        let _ = tcp.set_nodelay(true);
        let (acceptor, hub) = (acceptor.clone(), hub.clone());
        tokio::spawn(async move {
            if let Err(e) = handle_conn(tcp, acceptor, hub).await {
                let disconnect =
                    e.downcast_ref::<tokio_tungstenite::tungstenite::Error>().is_some_and(is_disconnect);
                if !disconnect {
                    log!("{peer}: {e:#}");
                }
            }
        });
    }
}

/// UDP ses relay'i: paketi al, doğrula, aynı sesli kanaldaki diğerlerine yolla.
pub async fn serve_voice(sock: UdpSocket, hub: Arc<Hub>) -> Result<()> {
    let mut buf = vec![0u8; corddis_proto::voice::MAX_PACKET];
    let mut out = Vec::new();
    let mut report = tokio::time::interval(Duration::from_secs(600));
    report.tick().await;
    loop {
        tokio::select! {
            r = sock.recv_from(&mut buf) => {
                let (n, from) = match r {
                    Ok(x) => x,
                    // Windows'ta ICMP "port ulaşılamaz" bir sonraki recv'de hata olarak döner; önemsiz.
                    Err(_) => continue,
                };
                hub.relay(&mut buf[..n], from, &mut out);
                for (addr, pkt) in out.drain(..) {
                    let _ = sock.send_to(&pkt, addr).await;
                }
            }
            _ = report.tick() => {
                let s = hub.take_relay_stats();
                if s.received > 0 {
                    log!("UDP son 10 dk: alınan {}, iletilen {}, reddedilen {}", s.received, s.forwarded, s.rejected);
                }
            }
        }
    }
}

/// İstemcinin düzgün kapatmadan gitmesi (uygulama kapandı, internet koptu): hata değil, ayrılma.
fn is_disconnect(e: &tokio_tungstenite::tungstenite::Error) -> bool {
    use std::io::ErrorKind::*;
    use tokio_tungstenite::tungstenite::error::{Error, ProtocolError};
    match e {
        Error::ConnectionClosed | Error::AlreadyClosed => true,
        Error::Protocol(ProtocolError::ResetWithoutClosingHandshake) => true,
        Error::Io(io) => matches!(io.kind(), ConnectionReset | ConnectionAborted | BrokenPipe | UnexpectedEof),
        _ => false,
    }
}

async fn send<S>(sink: &mut S, msg: &ServerMsg) -> Result<()>
where
    S: Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    sink.send(Message::text(serde_json::to_string(msg)?)).await?;
    Ok(())
}

async fn handle_conn(tcp: TcpStream, acceptor: TlsAcceptor, hub: Arc<Hub>) -> Result<()> {
    let tls = timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await.context("TLS zaman aşımı")??;
    let ws = timeout(HANDSHAKE_TIMEOUT, tokio_tungstenite::accept_async_with_config(tls, Some(ws_config())))
        .await
        .context("WebSocket zaman aşımı")??;
    let (mut sink, mut stream) = ws.split();

    let token = match timeout(HANDSHAKE_TIMEOUT, stream.next()).await {
        Ok(Some(Ok(Message::Text(t)))) => match serde_json::from_str::<ClientMsg>(&t) {
            Ok(ClientMsg::Auth { token }) => token,
            _ => {
                send(&mut sink, &ServerMsg::Error { code: "auth_required".into() }).await.ok();
                return Ok(());
            }
        },
        _ => return Ok(()),
    };
    let Some(user) = hub.authenticate(&token)? else {
        send(&mut sink, &ServerMsg::Error { code: "bad_token".into() }).await.ok();
        sink.close().await.ok();
        anyhow::bail!("geçersiz token");
    };

    let reg = hub.register(&user)?;
    let conn = reg.conn;
    log!("{} bağlandı", user.name);
    let result = session(&hub, user.id, reg, &mut sink, &mut stream).await;
    hub.unregister(user.id, conn);
    sink.close().await.ok();
    log!("{} ayrıldı", user.name);
    result
}

async fn session<S, R>(
    hub: &Hub,
    user: corddis_proto::control::UserId,
    reg: hub::Registered,
    sink: &mut S,
    stream: &mut R,
) -> Result<()>
where
    S: Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
    R: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let hub::Registered { conn, mut rx, auth_ok, snapshot } = reg;
    send(sink, &auth_ok).await?;
    send(sink, &snapshot).await?;

    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(m) => send(sink, &m).await?,
                // Kuyruk kapandı: aynı kullanıcı başka yerden bağlandı ya da takıldığı için atıldı.
                None => return Ok(()),
            },
            incoming = timeout(IDLE_TIMEOUT, stream.next()) => match incoming {
                Err(_) => anyhow::bail!("zaman aşımı (ping gelmedi)"),
                Ok(None) | Ok(Some(Ok(Message::Close(_)))) => return Ok(()),
                Ok(Some(Err(e))) if is_disconnect(&e) => return Ok(()),
                Ok(Some(Err(e))) => return Err(e.into()),
                Ok(Some(Ok(Message::Text(t)))) => match serde_json::from_str::<ClientMsg>(&t) {
                    Ok(m) => hub.handle(user, conn, m)?,
                    Err(_) => send(sink, &ServerMsg::Error { code: "bad_message".into() }).await?,
                },
                // Ping'lere tungstenite kendisi pong ile cevap verir.
                Ok(Some(Ok(_))) => {}
            },
        }
    }
}

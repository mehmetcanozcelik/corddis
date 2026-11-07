//! Sunucuya kontrol bağlantısı: TCP → TLS (parmak izi sabitli) → WebSocket (MVP.md §7.1).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use corddis_proto::invite::Invite;
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::WebSocketStream;

pub type Ws = WebSocketStream<TlsStream<TcpStream>>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_WS_MESSAGE: usize = 1024 * 1024;

pub async fn connect(inv: &Invite) -> Result<Ws> {
    let tcp = timeout(CONNECT_TIMEOUT, TcpStream::connect((inv.host.as_str(), inv.port)))
        .await
        .context("sunucuya bağlanırken zaman aşımı")?
        .with_context(|| format!("{}:{} adresine bağlanılamadı", inv.host, inv.port))?;
    tcp.set_nodelay(true)?;

    let connector = TlsConnector::from(Arc::new(corddis_proto::tls::pinned_client_config(inv.fingerprint)));
    let name = ServerName::try_from(inv.host.clone()).context("sunucu adresi geçersiz")?;
    let tls = timeout(CONNECT_TIMEOUT, connector.connect(name, tcp))
        .await
        .context("TLS zaman aşımı")?
        .context("güvenli bağlantı kurulamadı")?;

    let url = format!("wss://{}:{}/", inv.url_host(), inv.port);
    let cfg = WebSocketConfig::default().max_message_size(Some(MAX_WS_MESSAGE));
    let (ws, _) = timeout(CONNECT_TIMEOUT, tokio_tungstenite::client_async_with_config(url, tls, Some(cfg)))
        .await
        .context("WebSocket zaman aşımı")??;
    Ok(ws)
}

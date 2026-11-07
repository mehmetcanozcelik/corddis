//! Uçtan uca testler için ortak yardımcılar: geçici veri klasöründe gerçek sunucu,
//! gerçek TCP + TLS (parmak izi sabitli) + WebSocket istemcisi.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use corddis_proto::control::{ChannelKind, ClientMsg, ServerMsg};
use corddis_server::db::Db;
use corddis_server::hub::Hub;
use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::ServerName;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsConnector;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

pub type Ws = WebSocketStream<tokio_rustls::client::TlsStream<TcpStream>>;

pub struct TestServer {
    pub addr: SocketAddr,
    pub udp: SocketAddr,
    pub fp: [u8; 32],
    pub tokens: Vec<String>,
    pub text: u32,
    pub voice: u32,
    _dir: TempDir,
}

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub async fn start(users: &[&str]) -> TestServer {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "corddis-e2e-{}-{}-{}",
        std::process::id(),
        module_path!().replace("::", "-"),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Db::open(&dir.join("t.db")).unwrap();
    let tokens = users.iter().map(|u| db.add_user(u).unwrap().1).collect();
    let chans = db.channels().unwrap();
    let text = chans.iter().find(|c| c.kind == ChannelKind::Text).unwrap().id;
    let voice = chans.iter().find(|c| c.kind == ChannelKind::Voice).unwrap().id;
    let (id, _) = corddis_server::tls::load_or_create(&dir).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let acceptor = id.acceptor().unwrap();
    let udp_sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let udp = udp_sock.local_addr().unwrap();
    let hub = Arc::new(Hub::new(db, udp.port()));
    tokio::spawn(corddis_server::serve(listener, acceptor, hub.clone()));
    tokio::spawn(corddis_server::serve_voice(udp_sock, hub));
    TestServer { addr, udp, fp: id.fingerprint(), tokens, text, voice, _dir: TempDir(dir) }
}

pub async fn connect(addr: SocketAddr, fp: [u8; 32]) -> anyhow::Result<Ws> {
    let tcp = TcpStream::connect(addr).await?;
    let connector = TlsConnector::from(Arc::new(corddis_proto::tls::pinned_client_config(fp)));
    let tls = connector.connect(ServerName::try_from("127.0.0.1").unwrap().to_owned(), tcp).await?;
    let (ws, _) = tokio_tungstenite::client_async(format!("wss://{addr}/"), tls).await?;
    Ok(ws)
}

pub async fn send(ws: &mut Ws, m: &ClientMsg) {
    ws.send(Message::text(serde_json::to_string(m).unwrap())).await.unwrap();
}

pub async fn recv(ws: &mut Ws) -> ServerMsg {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("sunucudan mesaj gelmedi")
            .expect("bağlantı kapandı")
            .unwrap();
        if let Message::Text(t) = m {
            return serde_json::from_str(&t).unwrap();
        }
    }
}

/// Giriş yapar; AuthOk ve Snapshot'ı döner.
pub async fn login(s: &TestServer, i: usize) -> (Ws, ServerMsg, ServerMsg) {
    let mut ws = connect(s.addr, s.fp).await.unwrap();
    send(&mut ws, &ClientMsg::Auth { token: s.tokens[i].clone() }).await;
    let ok = recv(&mut ws).await;
    let snap = recv(&mut ws).await;
    (ws, ok, snap)
}

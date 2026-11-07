//! corddis-server komut satırı: sunucuyu çalıştırma ve yönetim (MVP.md §5.2, §10).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use corddis_proto::control::ChannelKind;
use corddis_proto::invite::{fingerprint_hex, Invite};
use corddis_server::db::{Db, UserRow};
use corddis_server::hub::Hub;
use corddis_server::{log, tls};

const HELP: &str = "\
corddis-server

Kullanım:
  corddis-server run [--bind 0.0.0.0:443] [--udp-port 7777]
  corddis-server user add <isim> [--host <ip/alan adı>] [--port 443]
  corddis-server user reset <isim> [--host <ip/alan adı>] [--port 443]
  corddis-server user remove <isim>
  corddis-server user list
  corddis-server channel add <isim> text|voice
  corddis-server channel remove <isim>
  corddis-server channel list
  corddis-server fingerprint

Ortak seçenek:
  --data <klasör>   Veri klasörü (veritabanı + sertifika). Varsayılan: $CORDDIS_DATA, yoksa ./data

`user add` ve `user reset` kişiye özel bir davet bağlantısı yazdırır. Bu bağlantı şifre gibidir:
yalnızca o kişiye, birebir mesajla gönderin.
";

struct Args {
    data: PathBuf,
    rest: Vec<String>,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut data = std::env::var_os("CORDDIS_DATA").map(PathBuf::from).unwrap_or_else(|| "data".into());
        let mut rest = Vec::new();
        let mut it = std::env::args().skip(1);
        while let Some(a) = it.next() {
            if a == "--data" {
                data = it.next().context("--data bir klasör bekliyor")?.into();
            } else {
                rest.push(a);
            }
        }
        Ok(Self { data, rest })
    }

    /// `--isim değer` seçeneğini çıkarır.
    fn take(&mut self, flag: &str) -> Result<Option<String>> {
        match self.rest.iter().position(|a| a == flag) {
            None => Ok(None),
            Some(i) => {
                if i + 1 >= self.rest.len() {
                    bail!("{flag} bir değer bekliyor");
                }
                let v = self.rest.remove(i + 1);
                self.rest.remove(i);
                Ok(Some(v))
            }
        }
    }
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("hata: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let mut a = Args::parse()?;
    let bind = a.take("--bind")?;
    let udp_port = a.take("--udp-port")?;
    let host = a.take("--host")?;
    let port: u16 = a.take("--port")?.map(|p| p.parse()).transpose().context("--port")?.unwrap_or(443);
    let words: Vec<&str> = a.rest.iter().map(String::as_str).collect();

    match words.as_slice() {
        ["run"] => {
            let bind: SocketAddr = bind.as_deref().unwrap_or("0.0.0.0:443").parse().context("--bind")?;
            let udp_port: u16 = udp_port.as_deref().unwrap_or("7777").parse().context("--udp-port")?;
            run(&a.data, bind, udp_port)
        }
        ["user", "add", name] => {
            let (u, token) = open(&a.data)?.add_user(name)?;
            print_invite(&a.data, &u, &token, host, port)
        }
        ["user", "reset", name] => {
            let (u, token) = open(&a.data)?.reset_token(name)?;
            println!("Eski davet bağlantısı artık geçersiz.");
            print_invite(&a.data, &u, &token, host, port)
        }
        ["user", "remove", name] => {
            if open(&a.data)?.remove_user(name)? {
                println!("'{name}' ve mesajları silindi. Bağlıysa en geç yeniden bağlanırken düşer.");
            } else {
                bail!("'{name}' adında kullanıcı yok");
            }
            Ok(())
        }
        ["user", "list"] => {
            for u in open(&a.data)?.users()? {
                println!("{:>4}  {}", u.id, u.name);
            }
            Ok(())
        }
        ["channel", "add", name, kind] => {
            let kind = match *kind {
                "text" => ChannelKind::Text,
                "voice" => ChannelKind::Voice,
                _ => bail!("kanal türü text veya voice olmalı"),
            };
            let c = open(&a.data)?.add_channel(name, kind)?;
            println!("Eklendi: {} ({:?}). Bağlı istemciler yeniden bağlanınca görecek.", c.name, c.kind);
            Ok(())
        }
        ["channel", "remove", name] => {
            if !open(&a.data)?.remove_channel(name)? {
                bail!("'{name}' adında kanal yok");
            }
            println!("Silindi.");
            Ok(())
        }
        ["channel", "list"] => {
            for c in open(&a.data)?.channels()? {
                println!("{:>4}  {:<6} {}", c.id, if c.kind == ChannelKind::Text { "yazı" } else { "ses" }, c.name);
            }
            Ok(())
        }
        ["fingerprint"] => {
            println!("{}", fingerprint_hex(&identity(&a.data)?.fingerprint()));
            Ok(())
        }
        _ => {
            print!("{HELP}");
            Ok(())
        }
    }
}

fn open(data: &PathBuf) -> Result<Db> {
    std::fs::create_dir_all(data).with_context(|| format!("{} oluşturulamadı", data.display()))?;
    Db::open(&data.join("corddis.db"))
}

fn identity(data: &PathBuf) -> Result<tls::Identity> {
    std::fs::create_dir_all(data)?;
    let (id, created) = tls::load_or_create(data)?;
    if created {
        eprintln!("Yeni sunucu sertifikası üretildi ({}). Bu dosyaları yedekleyin: değişirse tüm davetler geçersiz olur.", data.display());
    }
    Ok(id)
}

fn print_invite(data: &PathBuf, u: &UserRow, token: &str, host: Option<String>, port: u16) -> Result<()> {
    let fp = identity(data)?.fingerprint();
    match host {
        Some(host) => {
            let inv = Invite { host, port, fingerprint: fp, token: token.to_string() };
            println!("{} için davet bağlantısı (yalnızca ona, birebir gönderin):\n\n{inv}\n", u.name);
        }
        None => {
            let inv = Invite { host: "SUNUCU_IP".into(), port, fingerprint: fp, token: token.to_string() };
            println!(
                "{} eklendi. --host verilmediği için SUNUCU_IP kısmını kendiniz değiştirin:\n\n{inv}\n",
                u.name
            );
        }
    }
    Ok(())
}

fn run(data: &PathBuf, bind: SocketAddr, udp_port: u16) -> Result<()> {
    let db = open(data)?;
    let id = identity(data)?;
    let acceptor = id.acceptor()?;
    let hub = Arc::new(Hub::new(db, udp_port));

    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(bind).await.with_context(|| format!("{bind} dinlenemedi"))?;
        let udp_addr = SocketAddr::new(bind.ip(), udp_port);
        let udp = tokio::net::UdpSocket::bind(udp_addr).await.with_context(|| format!("UDP {udp_addr} dinlenemedi"))?;
        log!("corddis-server {} dinliyor: TCP {bind}, UDP {udp_addr}", env!("CARGO_PKG_VERSION"));
        log!("sertifika parmak izi: {}", fingerprint_hex(&id.fingerprint()));
        tokio::select! {
            r = corddis_server::serve(listener, acceptor, hub.clone()) => r,
            r = corddis_server::serve_voice(udp, hub) => r,
            _ = tokio::signal::ctrl_c() => { log!("kapatılıyor"); Ok(()) }
        }
    })
}

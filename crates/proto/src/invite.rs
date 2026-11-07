//! Davet bağlantısı (MVP.md §10.6):
//! `corddis://HOST:PORT?fp=<SHA-256 HEX>&token=<TOKEN>`
//!
//! Sunucu adresi, sunucu sertifikasının parmak izi ve kişiye özel giriş token'ı tek satırda.
//! Parmak izi sayesinde domain/CA gerekmez ve araya giren biri sunucuyu taklit edemez.

use std::fmt;
use std::str::FromStr;

pub const SCHEME: &str = "corddis://";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    /// IP veya alan adı (IPv6 köşeli parantezsiz).
    pub host: String,
    pub port: u16,
    pub fingerprint: [u8; 32],
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub &'static str);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "geçersiz davet bağlantısı: {}", self.0)
    }
}
impl std::error::Error for ParseError {}

pub fn fingerprint_hex(fp: &[u8; 32]) -> String {
    fp.iter().map(|b| format!("{b:02X}")).collect()
}

pub fn parse_fingerprint(s: &str) -> Option<[u8; 32]> {
    let s: String = s.chars().filter(|c| *c != ':').collect();
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

impl Invite {
    /// URL'de kullanılacak host (IPv6 ise köşeli parantezli).
    pub fn url_host(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        }
    }
}

impl fmt::Display for Invite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{SCHEME}{}:{}?fp={}&token={}",
            self.url_host(),
            self.port,
            fingerprint_hex(&self.fingerprint),
            self.token
        )
    }
}

impl FromStr for Invite {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, ParseError> {
        let rest = s.trim().strip_prefix(SCHEME).ok_or(ParseError("corddis:// ile başlamıyor"))?;
        let (addr, query) = rest.split_once('?').ok_or(ParseError("parametreler eksik"))?;
        let addr = addr.trim_end_matches('/');
        let (host, port) = addr.rsplit_once(':').ok_or(ParseError("port eksik"))?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        if host.is_empty() {
            return Err(ParseError("sunucu adresi boş"));
        }
        let port: u16 = port.parse().map_err(|_| ParseError("port geçersiz"))?;

        let (mut fp, mut token) = (None, None);
        for pair in query.split('&') {
            match pair.split_once('=') {
                Some(("fp", v)) => fp = parse_fingerprint(v),
                Some(("token", v)) if !v.is_empty() => token = Some(v.to_string()),
                _ => {}
            }
        }
        Ok(Invite {
            host: host.to_string(),
            port,
            fingerprint: fp.ok_or(ParseError("parmak izi (fp) eksik veya bozuk"))?,
            token: token.ok_or(ParseError("token eksik"))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(host: &str) -> Invite {
        let mut fp = [0u8; 32];
        fp.iter_mut().enumerate().for_each(|(i, b)| *b = i as u8 * 7);
        Invite { host: host.into(), port: 443, fingerprint: fp, token: "abc-DEF_123".into() }
    }

    #[test]
    fn roundtrip_ipv4_and_domain() {
        for h in ["1.2.3.4", "corddis.example.com"] {
            let i = sample(h);
            assert_eq!(i.to_string().parse::<Invite>().unwrap(), i);
        }
    }

    #[test]
    fn roundtrip_ipv6() {
        let i = sample("2001:db8::1");
        let s = i.to_string();
        assert!(s.starts_with("corddis://[2001:db8::1]:443?"));
        assert_eq!(s.parse::<Invite>().unwrap(), i);
    }

    #[test]
    fn rejects_bad_links() {
        assert!("https://x:1?fp=00&token=a".parse::<Invite>().is_err());
        assert!("corddis://x?fp=00&token=a".parse::<Invite>().is_err());
        assert!("corddis://x:1?fp=00&token=a".parse::<Invite>().is_err()); // kısa parmak izi
        let no_token = format!("corddis://x:1?fp={}", "AB".repeat(32));
        assert!(no_token.parse::<Invite>().is_err());
    }

    #[test]
    fn fingerprint_accepts_colons() {
        let colon = vec!["AB"; 32].join(":");
        assert_eq!(parse_fingerprint(&colon), Some([0xAB; 32]));
    }
}

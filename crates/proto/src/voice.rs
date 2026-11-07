//! UDP ses paketleri (MVP.md §7.2).
//!
//! İstemci → Sunucu: `ver | type | session u32 | seq u32 | şifreli içerik | tag`
//! Sunucu → İstemci: `ver | type | speaker u16 | seq u32 | counter u64 | şifreli içerik | tag`
//!
//! Başlık şifrelenmez ama AEAD "associated data" olarak doğrulanır.
//!
//! İki ayrı sayı var:
//! - **Paket sayacı** (istemci başlığındaki `seq`): her pakette (ses, keepalive, konuşma-bitti)
//!   artar. Nonce ve replay koruması bununla yapılır.
//! - **Ses sıra numarası** (şifreli içeriğin ilk 4 byte'ı): yalnızca ses çerçevelerinde artar.
//!   Alıcının jitter buffer'ı bununla çalışır; keepalive'lar araya boşluk sokmaz.
//!   Sunucu bunu okuyup alıcıya giden başlığın `seq` alanına koyar.
//!
//! İçerik:
//! - Voice:        `ses_seq u32 | opus çerçevesi`
//! - EndOfSpeech:  `ses_seq u32` (son ses çerçevesinden sonraki numara)
//! - Keepalive:    boş. Sunucu aynı paket sayacıyla geri yollar (istemci ping ölçer).
//!
//! Nonce:
//! - İstemci → Sunucu: yön ‖ 0 ‖ paket sayacı. Anahtar bağlantıya özel ve sayaç bağlantı
//!   boyunca sıfırlanmaz, bu yüzden nonce tekrar etmez.
//! - Sunucu → İstemci: yön ‖ 0 ‖ counter. Sunucu her alıcı için ayrı, sürekli artan bir
//!   sayaç tutar (konuşmacıların seq'leri çakışabileceği için onlar kullanılamaz).

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce, Tag};

pub const VERSION: u8 = 1;
pub const TAG_LEN: usize = 16;
pub const KEY_LEN: usize = 32;
pub const CLIENT_HEADER_LEN: usize = 10;
pub const SERVER_HEADER_LEN: usize = 16;
/// Voice/EndOfSpeech içeriğinin başındaki ses sıra numarası.
pub const VOICE_SEQ_LEN: usize = 4;
/// Bir UDP paketinin olabileceği en büyük boyut (alım tamponu için).
pub const MAX_PACKET: usize = 1500;

/// Tekrar gönderilmiş (replay) paketleri reddeder; son 64 numara içinde sıra dışı
/// gelişlere izin verir (IPsec tarzı kayan pencere). Yalnızca kimliği doğrulanmış
/// (şifresi başarıyla çözülmüş) paketlerle çağrılmalı.
#[derive(Debug, Default, Clone)]
pub struct ReplayWindow {
    top: u64,
    bits: u64,
    seen_any: bool,
}

impl ReplayWindow {
    pub fn accept(&mut self, n: u64) -> bool {
        if !self.seen_any {
            (self.seen_any, self.top, self.bits) = (true, n, 1);
            return true;
        }
        if n > self.top {
            let shift = n - self.top;
            self.bits = if shift >= 64 { 0 } else { self.bits << shift };
            self.bits |= 1;
            self.top = n;
            return true;
        }
        let off = self.top - n;
        if off >= 64 {
            return false;
        }
        let mask = 1u64 << off;
        if self.bits & mask != 0 {
            return false;
        }
        self.bits |= mask;
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketType {
    /// İçinde bir Opus çerçevesi var.
    Voice = 1,
    /// NAT eşlemesini canlı tutar; içerik boş.
    Keepalive = 2,
    /// Konuşma bitti; `seq` son ses paketinden sonraki numaradır. İçerik boş.
    EndOfSpeech = 3,
}

impl PacketType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Voice),
            2 => Some(Self::Keepalive),
            3 => Some(Self::EndOfSpeech),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Direction {
    ClientToServer = 0,
    ServerToClient = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    TooShort,
    BadVersion,
    BadType,
    BufferTooSmall,
    /// Şifre çözme/doğrulama başarısız (yanlış anahtar, bozulmuş veya sahte paket).
    Auth,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}
impl std::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientHeader {
    pub ty: PacketType,
    pub session: u32,
    pub seq: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerHeader {
    pub ty: PacketType,
    pub speaker: u16,
    pub seq: u32,
    pub counter: u64,
}

impl ClientHeader {
    fn write(&self, b: &mut [u8]) {
        b[0] = VERSION;
        b[1] = self.ty as u8;
        b[2..6].copy_from_slice(&self.session.to_be_bytes());
        b[6..10].copy_from_slice(&self.seq.to_be_bytes());
    }

    fn read(b: &[u8]) -> Result<Self, Error> {
        if b.len() < CLIENT_HEADER_LEN + TAG_LEN {
            return Err(Error::TooShort);
        }
        if b[0] != VERSION {
            return Err(Error::BadVersion);
        }
        Ok(Self {
            ty: PacketType::from_u8(b[1]).ok_or(Error::BadType)?,
            session: u32::from_be_bytes(b[2..6].try_into().unwrap()),
            seq: u32::from_be_bytes(b[6..10].try_into().unwrap()),
        })
    }

    /// Şifrelemeden önce, oturum anahtarı aranmadan sadece session id'yi okumak için.
    pub fn peek_session(packet: &[u8]) -> Result<u32, Error> {
        Ok(Self::read(packet)?.session)
    }
}

impl ServerHeader {
    fn write(&self, b: &mut [u8]) {
        b[0] = VERSION;
        b[1] = self.ty as u8;
        b[2..4].copy_from_slice(&self.speaker.to_be_bytes());
        b[4..8].copy_from_slice(&self.seq.to_be_bytes());
        b[8..16].copy_from_slice(&self.counter.to_be_bytes());
    }

    fn read(b: &[u8]) -> Result<Self, Error> {
        if b.len() < SERVER_HEADER_LEN + TAG_LEN {
            return Err(Error::TooShort);
        }
        if b[0] != VERSION {
            return Err(Error::BadVersion);
        }
        Ok(Self {
            ty: PacketType::from_u8(b[1]).ok_or(Error::BadType)?,
            speaker: u16::from_be_bytes(b[2..4].try_into().unwrap()),
            seq: u32::from_be_bytes(b[4..8].try_into().unwrap()),
            counter: u64::from_be_bytes(b[8..16].try_into().unwrap()),
        })
    }
}

fn nonce(dir: Direction, counter: u64) -> Nonce {
    let mut n = [0u8; 12];
    n[0] = dir as u8;
    n[4..12].copy_from_slice(&counter.to_be_bytes());
    Nonce::from(n)
}

/// Bir bağlantıya ait UDP şifreleme anahtarı.
pub struct VoiceCipher {
    aead: ChaCha20Poly1305,
}

impl VoiceCipher {
    pub fn new(key: &[u8; KEY_LEN]) -> Self {
        Self {
            aead: ChaCha20Poly1305::new(Key::from_slice(key)),
        }
    }

    /// İstemci tarafı: paketi `out`'a yazar, yazılan uzunluğu döner.
    pub fn seal_client(
        &self,
        h: &ClientHeader,
        payload: &[u8],
        out: &mut [u8],
    ) -> Result<usize, Error> {
        let total = CLIENT_HEADER_LEN + payload.len() + TAG_LEN;
        if out.len() < total {
            return Err(Error::BufferTooSmall);
        }
        let (head, rest) = out.split_at_mut(CLIENT_HEADER_LEN);
        h.write(head);
        let body = &mut rest[..payload.len()];
        body.copy_from_slice(payload);
        let tag = self
            .aead
            .encrypt_in_place_detached(&nonce(Direction::ClientToServer, h.seq as u64), head, body)
            .map_err(|_| Error::Auth)?;
        rest[payload.len()..payload.len() + TAG_LEN].copy_from_slice(&tag);
        Ok(total)
    }

    /// Sunucu tarafı: paketi yerinde çözer, başlığı ve çözülmüş içeriği döner.
    pub fn open_client<'a>(&self, packet: &'a mut [u8]) -> Result<(ClientHeader, &'a [u8]), Error> {
        let h = ClientHeader::read(packet)?;
        let (head, rest) = packet.split_at_mut(CLIENT_HEADER_LEN);
        let body_len = rest.len() - TAG_LEN;
        let (body, tag) = rest.split_at_mut(body_len);
        self.aead
            .decrypt_in_place_detached(
                &nonce(Direction::ClientToServer, h.seq as u64),
                head,
                body,
                Tag::from_slice(tag),
            )
            .map_err(|_| Error::Auth)?;
        Ok((h, body))
    }

    /// Sunucu tarafı: alıcıya özel anahtarla paketi `out`'a yazar.
    pub fn seal_server(
        &self,
        h: &ServerHeader,
        payload: &[u8],
        out: &mut [u8],
    ) -> Result<usize, Error> {
        let total = SERVER_HEADER_LEN + payload.len() + TAG_LEN;
        if out.len() < total {
            return Err(Error::BufferTooSmall);
        }
        let (head, rest) = out.split_at_mut(SERVER_HEADER_LEN);
        h.write(head);
        let body = &mut rest[..payload.len()];
        body.copy_from_slice(payload);
        let tag = self
            .aead
            .encrypt_in_place_detached(&nonce(Direction::ServerToClient, h.counter), head, body)
            .map_err(|_| Error::Auth)?;
        rest[payload.len()..payload.len() + TAG_LEN].copy_from_slice(&tag);
        Ok(total)
    }

    /// İstemci tarafı: sunucudan gelen paketi yerinde çözer.
    pub fn open_server<'a>(&self, packet: &'a mut [u8]) -> Result<(ServerHeader, &'a [u8]), Error> {
        let h = ServerHeader::read(packet)?;
        let (head, rest) = packet.split_at_mut(SERVER_HEADER_LEN);
        let body_len = rest.len() - TAG_LEN;
        let (body, tag) = rest.split_at_mut(body_len);
        self.aead
            .decrypt_in_place_detached(
                &nonce(Direction::ServerToClient, h.counter),
                head,
                body,
                Tag::from_slice(tag),
            )
            .map_err(|_| Error::Auth)?;
        Ok((h, body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7; 32];

    #[test]
    fn client_roundtrip() {
        let c = VoiceCipher::new(&KEY);
        let h = ClientHeader { ty: PacketType::Voice, session: 42, seq: 9 };
        let mut buf = [0u8; 128];
        let n = c.seal_client(&h, b"opus-frame", &mut buf).unwrap();
        assert_eq!(n, CLIENT_HEADER_LEN + 10 + TAG_LEN);
        assert_eq!(ClientHeader::peek_session(&buf[..n]).unwrap(), 42);
        let (h2, body) = c.open_client(&mut buf[..n]).unwrap();
        assert_eq!(h2, h);
        assert_eq!(body, b"opus-frame");
    }

    #[test]
    fn server_roundtrip() {
        let c = VoiceCipher::new(&KEY);
        let h = ServerHeader { ty: PacketType::Voice, speaker: 3, seq: 100, counter: 5 };
        let mut buf = [0u8; 128];
        let n = c.seal_server(&h, b"abc", &mut buf).unwrap();
        let (h2, body) = c.open_server(&mut buf[..n]).unwrap();
        assert_eq!(h2, h);
        assert_eq!(body, b"abc");
    }

    #[test]
    fn empty_payload_roundtrip() {
        let c = VoiceCipher::new(&KEY);
        let h = ClientHeader { ty: PacketType::Keepalive, session: 1, seq: 2 };
        let mut buf = [0u8; 64];
        let n = c.seal_client(&h, &[], &mut buf).unwrap();
        let (h2, body) = c.open_client(&mut buf[..n]).unwrap();
        assert_eq!(h2.ty, PacketType::Keepalive);
        assert!(body.is_empty());
    }

    #[test]
    fn tampered_header_rejected() {
        let c = VoiceCipher::new(&KEY);
        let h = ClientHeader { ty: PacketType::Voice, session: 42, seq: 9 };
        let mut buf = [0u8; 128];
        let n = c.seal_client(&h, b"x", &mut buf).unwrap();
        buf[2] ^= 1; // session değiştirildi
        assert_eq!(c.open_client(&mut buf[..n]).unwrap_err(), Error::Auth);
    }

    #[test]
    fn wrong_key_rejected() {
        let a = VoiceCipher::new(&KEY);
        let b = VoiceCipher::new(&[8; 32]);
        let h = ServerHeader { ty: PacketType::Voice, speaker: 1, seq: 1, counter: 1 };
        let mut buf = [0u8; 64];
        let n = a.seal_server(&h, b"x", &mut buf).unwrap();
        assert_eq!(b.open_server(&mut buf[..n]).unwrap_err(), Error::Auth);
    }

    #[test]
    fn replay_window() {
        let mut w = ReplayWindow::default();
        assert!(w.accept(10));
        assert!(!w.accept(10)); // tekrar
        assert!(w.accept(12));
        assert!(w.accept(11)); // sıra dışı ama yeni
        assert!(!w.accept(11));
        assert!(w.accept(100));
        assert!(!w.accept(12)); // pencerenin dışında kaldı
        assert!(w.accept(40)); // pencere içinde, hiç görülmemiş
        assert!(w.accept(1000));
    }

    #[test]
    fn short_packet_rejected() {
        let c = VoiceCipher::new(&KEY);
        let mut buf = [VERSION, 1, 0, 0];
        assert_eq!(c.open_client(&mut buf).unwrap_err(), Error::TooShort);
    }
}

//! UDP ses relay'i, gerçek soketler üzerinden (MVP.md §7.2).

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine;
use common::*;
use corddis_proto::control::{ClientMsg, ServerMsg, UserId};
use corddis_proto::voice::{
    ClientHeader, PacketType, ServerHeader, VoiceCipher, CLIENT_HEADER_LEN, MAX_PACKET, TAG_LEN, VOICE_SEQ_LEN,
};
use tokio::net::UdpSocket;

/// WebSocket ile giriş yapmış ve UDP adresini sunucuya tanıtmış test istemcisi.
struct Peer {
    ws: Ws,
    id: UserId,
    udp: UdpSocket,
    session: u32,
    cipher: VoiceCipher,
    counter: u32,
}

impl Peer {
    /// AuthOk'tan UDP bilgilerini alır, ilk keepalive'ı yollar ve yankısını doğrular.
    async fn from_auth(ws: Ws, ok: ServerMsg, server_udp: SocketAddr) -> Peer {
        let ServerMsg::AuthOk { user_id, udp_session, udp_key, .. } = ok else { panic!("{ok:?}") };
        let key: [u8; 32] = base64::engine::general_purpose::STANDARD.decode(udp_key).unwrap().try_into().unwrap();
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        udp.connect(server_udp).await.unwrap();
        let mut p = Peer { ws, id: user_id, udp, session: udp_session, cipher: VoiceCipher::new(&key), counter: 0 };
        let seq = p.send(PacketType::Keepalive, &[]).await;
        let (h, body) = p.recv().await.expect("keepalive yankısı gelmedi");
        assert_eq!((h.ty, h.seq, h.speaker), (PacketType::Keepalive, seq, user_id));
        assert!(body.is_empty());
        p
    }

    /// Giriş yapar ve sesli kanala katılır; diğerlerinin olay kuyruklarını da tüketir.
    async fn join(s: &TestServer, i: usize, others: &mut [&mut Peer]) -> Peer {
        let (mut ws, ok, _) = login(s, i).await;
        for o in others.iter_mut() {
            assert!(matches!(recv(&mut o.ws).await, ServerMsg::UserOnline { .. }));
        }
        send(&mut ws, &ClientMsg::JoinVoice { channel_id: s.voice }).await;
        assert!(matches!(recv(&mut ws).await, ServerMsg::VoiceJoin { .. }));
        for o in others.iter_mut() {
            assert!(matches!(recv(&mut o.ws).await, ServerMsg::VoiceJoin { .. }));
        }
        Peer::from_auth(ws, ok, s.udp).await
    }

    fn packet(&mut self, ty: PacketType, payload: &[u8]) -> Vec<u8> {
        let h = ClientHeader { ty, session: self.session, seq: self.counter };
        self.counter += 1;
        let mut buf = vec![0u8; CLIENT_HEADER_LEN + payload.len() + TAG_LEN];
        self.cipher.seal_client(&h, payload, &mut buf).unwrap();
        buf
    }

    async fn send(&mut self, ty: PacketType, payload: &[u8]) -> u32 {
        let pkt = self.packet(ty, payload);
        self.udp.send(&pkt).await.unwrap();
        self.counter - 1
    }

    async fn send_voice(&mut self, voice_seq: u32, opus: &[u8]) -> Vec<u8> {
        let mut payload = voice_seq.to_be_bytes().to_vec();
        payload.extend_from_slice(opus);
        let pkt = self.packet(PacketType::Voice, &payload);
        self.udp.send(&pkt).await.unwrap();
        pkt
    }

    async fn recv(&mut self) -> Option<(ServerHeader, Vec<u8>)> {
        let mut buf = vec![0u8; MAX_PACKET];
        let n = tokio::time::timeout(Duration::from_millis(400), self.udp.recv(&mut buf)).await.ok()?.ok()?;
        let (h, body) = self.cipher.open_server(&mut buf[..n]).expect("sunucu paketi çözülemedi");
        Some((h, body.to_vec()))
    }
}

#[tokio::test]
async fn voice_is_relayed_to_channel_members_only() {
    let s = start(&["Ali", "Veli", "Can"]).await;
    let mut a = Peer::join(&s, 0, &mut []).await;
    let mut b = Peer::join(&s, 1, &mut [&mut a]).await;

    // Can çevrimiçi ve UDP'si açık ama sesli kanalda değil.
    let (c_ws, ok, _) = login(&s, 2).await;
    recv(&mut a.ws).await;
    recv(&mut b.ws).await;
    let mut c = Peer::from_auth(c_ws, ok, s.udp).await;

    a.send_voice(7, b"opus-1").await;
    let (h, body) = b.recv().await.expect("Veli sesi almadı");
    assert_eq!((h.ty, h.speaker, h.seq, body.as_slice()), (PacketType::Voice, a.id, 7, &b"opus-1"[..]));
    assert!(a.recv().await.is_none(), "konuşan kendi sesini almamalı");
    assert!(c.recv().await.is_none(), "kanal dışındaki ses almamalı");

    // Konuşma bitti işareti de iletilir; keepalive'lar ses sırasına boşluk sokmaz.
    a.send(PacketType::Keepalive, &[]).await;
    assert!(a.recv().await.is_some());
    a.send(PacketType::EndOfSpeech, &8u32.to_be_bytes()).await;
    let (h, body) = b.recv().await.unwrap();
    assert_eq!((h.ty, h.seq), (PacketType::EndOfSpeech, 8));
    assert!(body.is_empty());
}

#[tokio::test]
async fn replayed_and_forged_packets_are_dropped() {
    let s = start(&["Ali", "Veli"]).await;
    let mut a = Peer::join(&s, 0, &mut []).await;
    let mut b = Peer::join(&s, 1, &mut [&mut a]).await;

    let pkt = a.send_voice(0, b"x").await;
    assert!(b.recv().await.is_some());
    a.udp.send(&pkt).await.unwrap(); // aynı paket tekrar
    assert!(b.recv().await.is_none(), "tekrar edilen paket iletilmemeli");

    // Doğru session id, yanlış anahtar.
    let forger = VoiceCipher::new(&[9; 32]);
    let mut payload = 1u32.to_be_bytes().to_vec();
    payload.push(b'y');
    let mut buf = vec![0u8; CLIENT_HEADER_LEN + VOICE_SEQ_LEN + 1 + TAG_LEN];
    forger
        .seal_client(&ClientHeader { ty: PacketType::Voice, session: a.session, seq: 999 }, &payload, &mut buf)
        .unwrap();
    a.udp.send(&buf).await.unwrap();
    assert!(b.recv().await.is_none(), "sahte paket iletilmemeli");

    // Asıl istemci hâlâ çalışıyor (sahte paket replay penceresini bozmadı).
    a.send_voice(1, b"z").await;
    assert_eq!(b.recv().await.unwrap().1, b"z");
}

#[tokio::test]
async fn muted_sender_and_deafened_receiver_get_nothing() {
    let s = start(&["Ali", "Veli"]).await;
    let mut a = Peer::join(&s, 0, &mut []).await;
    let mut b = Peer::join(&s, 1, &mut [&mut a]).await;

    send(&mut b.ws, &ClientMsg::SetState { muted: false, deafened: true }).await;
    recv(&mut a.ws).await;
    recv(&mut b.ws).await;
    a.send_voice(0, b"x").await;
    assert!(b.recv().await.is_none(), "sağır kullanıcıya ses gitmemeli");

    send(&mut b.ws, &ClientMsg::SetState { muted: false, deafened: false }).await;
    send(&mut a.ws, &ClientMsg::SetState { muted: true, deafened: false }).await;
    for _ in 0..2 {
        recv(&mut a.ws).await;
        recv(&mut b.ws).await;
    }
    a.send_voice(1, b"x").await;
    assert!(b.recv().await.is_none(), "mute'lu kullanıcının sesi iletilmemeli");
}

#[tokio::test]
async fn leaving_channel_stops_relay() {
    let s = start(&["Ali", "Veli"]).await;
    let mut a = Peer::join(&s, 0, &mut []).await;
    let mut b = Peer::join(&s, 1, &mut [&mut a]).await;
    send(&mut b.ws, &ClientMsg::LeaveVoice).await;
    recv(&mut a.ws).await;
    a.send_voice(0, b"x").await;
    assert!(b.recv().await.is_none());
}

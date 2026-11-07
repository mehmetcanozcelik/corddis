//! Uçtan uca: gerçek TCP + TLS (parmak izi sabitli) + WebSocket üzerinden sunucu davranışı.

mod common;

use std::time::Duration;

use common::*;
use corddis_proto::control::{ClientMsg, ServerMsg};
use futures_util::StreamExt;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn wrong_fingerprint_is_rejected_by_client() {
    let s = start(&["a"]).await;
    let mut bad = s.fp;
    bad[0] ^= 0xFF;
    let err = connect(s.addr, bad).await.unwrap_err();
    assert!(format!("{err:#}").contains("parmak izi"), "{err:#}");
}

#[tokio::test]
async fn bad_token_is_rejected() {
    let s = start(&["a"]).await;
    let mut ws = connect(s.addr, s.fp).await.unwrap();
    send(&mut ws, &ClientMsg::Auth { token: "yanlis".into() }).await;
    assert_eq!(recv(&mut ws).await, ServerMsg::Error { code: "bad_token".into() });
}

#[tokio::test]
async fn first_message_must_be_auth() {
    let s = start(&["a"]).await;
    let mut ws = connect(s.addr, s.fp).await.unwrap();
    send(&mut ws, &ClientMsg::LeaveVoice).await;
    assert_eq!(recv(&mut ws).await, ServerMsg::Error { code: "auth_required".into() });
}

#[tokio::test]
async fn full_chat_and_voice_presence_flow() {
    let s = start(&["Ali", "Veli"]).await;

    let (mut a, ok, snap) = login(&s, 0).await;
    let ServerMsg::AuthOk { user_id: a_id, udp_key, udp_port, .. } = ok else { panic!("{ok:?}") };
    assert_eq!(udp_port, s.udp.port());
    assert_eq!(udp_key.len(), 44); // 32 byte, base64
    let ServerMsg::Snapshot { channels, users, .. } = snap else { panic!() };
    assert_eq!(channels.len(), 3);
    assert!(users.iter().any(|u| u.id == a_id && u.online));
    assert!(users.iter().any(|u| u.name == "Veli" && !u.online));

    let (mut b, ok, snap) = login(&s, 1).await;
    let ServerMsg::AuthOk { user_id: b_id, .. } = ok else { panic!() };
    let ServerMsg::Snapshot { users, .. } = snap else { panic!() };
    assert!(users.iter().all(|u| u.online));
    assert_eq!(recv(&mut a).await, ServerMsg::UserOnline { user_id: b_id, online: true });

    // Mesaj iki tarafa da gider.
    send(&mut a, &ClientMsg::SendMsg { channel_id: s.text, text: "\u{feff}  selam  ".into() }).await;
    for ws in [&mut a, &mut b] {
        let ServerMsg::Msg(m) = recv(ws).await else { panic!() };
        assert_eq!((m.user_id, m.text.as_str()), (a_id, "selam"));
    }

    // Geçmiş yalnızca isteyene gider.
    send(&mut b, &ClientMsg::History { channel_id: s.text, before_id: None, limit: 500 }).await;
    let ServerMsg::HistoryResult { messages, .. } = recv(&mut b).await else { panic!() };
    assert_eq!(messages.len(), 1);

    // Hatalı istekler yalnızca gönderene hata döner.
    send(&mut a, &ClientMsg::SendMsg { channel_id: s.voice, text: "x".into() }).await;
    assert_eq!(recv(&mut a).await, ServerMsg::Error { code: "bad_channel".into() });
    send(&mut a, &ClientMsg::SendMsg { channel_id: s.text, text: "x".repeat(2001) }).await;
    assert_eq!(recv(&mut a).await, ServerMsg::Error { code: "bad_text".into() });
    send(&mut a, &ClientMsg::JoinVoice { channel_id: s.text }).await;
    assert_eq!(recv(&mut a).await, ServerMsg::Error { code: "bad_channel".into() });

    // Sesli kanal ve durum değişiklikleri herkese yayınlanır.
    send(&mut a, &ClientMsg::JoinVoice { channel_id: s.voice }).await;
    for ws in [&mut a, &mut b] {
        assert_eq!(recv(ws).await, ServerMsg::VoiceJoin { channel_id: s.voice, user_id: a_id });
    }
    send(&mut b, &ClientMsg::SetState { muted: false, deafened: true }).await;
    for ws in [&mut a, &mut b] {
        // Sağır = otomatik olarak mikrofon da kapalı.
        assert_eq!(recv(ws).await, ServerMsg::UserState { user_id: b_id, muted: true, deafened: true });
    }

    // A ayrılınca B, A'nın sesli kanaldan çıktığını ve çevrimdışı olduğunu görür.
    a.close(None).await.unwrap();
    assert_eq!(recv(&mut b).await, ServerMsg::VoiceLeave { channel_id: s.voice, user_id: a_id });
    assert_eq!(recv(&mut b).await, ServerMsg::UserOnline { user_id: a_id, online: false });
}

#[tokio::test]
async fn second_login_replaces_first_connection() {
    let s = start(&["Ali", "Veli"]).await;
    let (mut b, _, _) = login(&s, 1).await;
    let (mut a1, _, _) = login(&s, 0).await;
    let ServerMsg::UserOnline { online: true, .. } = recv(&mut b).await else { panic!() };
    send(&mut a1, &ClientMsg::JoinVoice { channel_id: s.voice }).await;
    recv(&mut a1).await;
    recv(&mut b).await;

    let (_a2, _, snap) = login(&s, 0).await;
    // Yeni bağlantının gördüğü durumda Ali sesli kanalda değil.
    let ServerMsg::Snapshot { voice_members, .. } = snap else { panic!() };
    assert!(voice_members.iter().all(|(_, m)| m.is_empty()));
    // B, Ali'nin eski bağlantısının sesli kanaldan düştüğünü görür; "çevrimdışı" görmez.
    assert!(matches!(recv(&mut b).await, ServerMsg::VoiceLeave { .. }));
    // Eski bağlantı kapanır.
    let end = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match a1.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                _ => {}
            }
        }
    })
    .await;
    assert!(end.is_ok(), "eski bağlantı kapanmadı");
}

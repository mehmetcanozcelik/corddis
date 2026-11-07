//! WebSocket kontrol mesajları (MVP.md §7.1). JSON, `type` alanıyla etiketli.

use serde::{Deserialize, Serialize};

pub type UserId = u16;
pub type ChannelId = u32;
pub type MessageId = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Text,
    Voice,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    pub id: ChannelId,
    pub name: String,
    pub kind: ChannelKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: UserId,
    pub name: String,
    pub online: bool,
    pub muted: bool,
    pub deafened: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: MessageId,
    pub channel_id: ChannelId,
    pub user_id: UserId,
    pub text: String,
    /// Unix zamanı (saniye).
    pub ts: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Auth { token: String },
    JoinVoice { channel_id: ChannelId },
    LeaveVoice,
    SendMsg { channel_id: ChannelId, text: String },
    History { channel_id: ChannelId, before_id: Option<MessageId>, limit: u32 },
    SetState { muted: bool, deafened: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    AuthOk {
        user_id: UserId,
        udp_session: u32,
        /// 32 byte, base64.
        udp_key: String,
        udp_port: u16,
    },
    Snapshot {
        channels: Vec<Channel>,
        users: Vec<User>,
        /// (kanal, o kanaldaki kullanıcılar)
        voice_members: Vec<(ChannelId, Vec<UserId>)>,
    },
    Msg(ChatMessage),
    HistoryResult { channel_id: ChannelId, messages: Vec<ChatMessage> },
    VoiceJoin { channel_id: ChannelId, user_id: UserId },
    VoiceLeave { channel_id: ChannelId, user_id: UserId },
    UserState { user_id: UserId, muted: bool, deafened: bool },
    UserOnline { user_id: UserId, online: bool },
    Error { code: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_shape_matches_doc() {
        let m = ClientMsg::SendMsg { channel_id: 2, text: "gg".into() };
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"type":"send_msg","channel_id":2,"text":"gg"}"#
        );
        let s: ServerMsg =
            serde_json::from_str(r#"{"type":"voice_join","channel_id":1,"user_id":4}"#).unwrap();
        assert_eq!(s, ServerMsg::VoiceJoin { channel_id: 1, user_id: 4 });
    }
}

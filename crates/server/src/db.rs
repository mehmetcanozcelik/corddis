//! SQLite: kullanıcılar, kanallar, mesaj geçmişi (MVP.md §5.2).
//!
//! Token'lar veritabanında yalnızca SHA-256 özeti olarak saklanır; veritabanı sızsa bile
//! token'lar ele geçmez.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use base64::Engine;
use corddis_proto::control::{Channel, ChannelId, ChannelKind, ChatMessage, MessageId, UserId};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

pub const MAX_NAME_CHARS: usize = 32;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS users (
    id          INTEGER PRIMARY KEY,
    name        TEXT    NOT NULL UNIQUE COLLATE NOCASE,
    token_hash  BLOB    NOT NULL UNIQUE,
    created_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS channels (
    id        INTEGER PRIMARY KEY,
    name      TEXT    NOT NULL UNIQUE COLLATE NOCASE,
    kind      TEXT    NOT NULL CHECK (kind IN ('text', 'voice')),
    position  INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS messages (
    id          INTEGER PRIMARY KEY,
    channel_id  INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    user_id     INTEGER NOT NULL REFERENCES users(id)    ON DELETE CASCADE,
    text        TEXT    NOT NULL,
    ts          INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS messages_by_channel ON messages (channel_id, id);
";

const DEFAULT_CHANNELS: &[(&str, ChannelKind)] =
    &[("genel", ChannelKind::Text), ("Oyun", ChannelKind::Voice), ("AFK", ChannelKind::Voice)];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRow {
    pub id: UserId,
    pub name: String,
}

pub struct Db {
    conn: Connection,
}

pub fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

pub fn new_token() -> Result<String> {
    let mut b = [0u8; 24];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("rastgele sayı üretilemedi: {e}"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b))
}

fn kind_str(k: ChannelKind) -> &'static str {
    match k {
        ChannelKind::Text => "text",
        ChannelKind::Voice => "voice",
    }
}

fn parse_kind(s: &str) -> rusqlite::Result<ChannelKind> {
    match s {
        "text" => Ok(ChannelKind::Text),
        "voice" => Ok(ChannelKind::Voice),
        _ => Err(rusqlite::Error::InvalidColumnType(0, s.into(), rusqlite::types::Type::Text)),
    }
}

fn validate_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        bail!("isim 1–{MAX_NAME_CHARS} karakter olmalı");
    }
    if name.chars().any(|c| c.is_control()) {
        bail!("isimde kontrol karakteri olamaz");
    }
    Ok(name)
}

fn to_user_id(id: i64) -> Result<UserId> {
    UserId::try_from(id).context("kullanıcı id'si 65535'i aştı")
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("{} açılamadı", path.display()))?;
        conn.busy_timeout(Duration::from_secs(2))?;
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;")?;
        conn.execute_batch(SCHEMA)?;
        let db = Self { conn };
        let n: i64 = db.conn.query_row("SELECT COUNT(*) FROM channels", [], |r| r.get(0))?;
        if n == 0 {
            for (name, kind) in DEFAULT_CHANNELS {
                db.add_channel(name, *kind)?;
            }
        }
        Ok(db)
    }

    // ---------- kullanıcılar ----------

    /// Yeni kullanıcı ekler, davet token'ını döner (yalnızca bir kez gösterilir).
    pub fn add_user(&self, name: &str) -> Result<(UserRow, String)> {
        let name = validate_name(name)?;
        let token = new_token()?;
        self.conn
            .execute(
                "INSERT INTO users (name, token_hash, created_at) VALUES (?1, ?2, ?3)",
                params![name, hash_token(&token), now_secs()],
            )
            .with_context(|| format!("'{name}' eklenemedi (bu isim zaten var olabilir)"))?;
        let id = to_user_id(self.conn.last_insert_rowid())?;
        Ok((UserRow { id, name: name.to_string() }, token))
    }

    /// Token'ı yeniler (kaybolduysa veya sızdıysa). Eski token hemen geçersiz olur.
    pub fn reset_token(&self, name: &str) -> Result<(UserRow, String)> {
        let user = self.user_by_name(name)?.with_context(|| format!("'{name}' adında kullanıcı yok"))?;
        let token = new_token()?;
        self.conn.execute(
            "UPDATE users SET token_hash = ?1 WHERE id = ?2",
            params![hash_token(&token), user.id],
        )?;
        Ok((user, token))
    }

    /// Kullanıcıyı ve mesajlarını siler.
    pub fn remove_user(&self, name: &str) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM users WHERE name = ?1", params![name.trim()])? > 0)
    }

    pub fn user_by_name(&self, name: &str) -> Result<Option<UserRow>> {
        self.conn
            .query_row("SELECT id, name FROM users WHERE name = ?1", params![name.trim()], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })
            .optional()?
            .map(|(id, name)| Ok(UserRow { id: to_user_id(id)?, name }))
            .transpose()
    }

    pub fn user_by_token(&self, token: &str) -> Result<Option<UserRow>> {
        self.conn
            .query_row("SELECT id, name FROM users WHERE token_hash = ?1", params![hash_token(token)], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })
            .optional()?
            .map(|(id, name)| Ok(UserRow { id: to_user_id(id)?, name }))
            .transpose()
    }

    pub fn users(&self) -> Result<Vec<UserRow>> {
        let mut st = self.conn.prepare("SELECT id, name FROM users ORDER BY name")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        rows.map(|r| {
            let (id, name) = r?;
            Ok(UserRow { id: to_user_id(id)?, name })
        })
        .collect()
    }

    // ---------- kanallar ----------

    pub fn add_channel(&self, name: &str, kind: ChannelKind) -> Result<Channel> {
        let name = validate_name(name)?;
        let pos: i64 =
            self.conn.query_row("SELECT COALESCE(MAX(position), -1) + 1 FROM channels", [], |r| r.get(0))?;
        self.conn
            .execute(
                "INSERT INTO channels (name, kind, position) VALUES (?1, ?2, ?3)",
                params![name, kind_str(kind), pos],
            )
            .with_context(|| format!("'{name}' kanalı eklenemedi (bu isim zaten var olabilir)"))?;
        Ok(Channel { id: self.conn.last_insert_rowid() as ChannelId, name: name.to_string(), kind })
    }

    pub fn remove_channel(&self, name: &str) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM channels WHERE name = ?1", params![name.trim()])? > 0)
    }

    pub fn channels(&self) -> Result<Vec<Channel>> {
        let mut st = self.conn.prepare("SELECT id, name, kind FROM channels ORDER BY position, id")?;
        let rows = st.query_map([], |r| {
            Ok(Channel { id: r.get(0)?, name: r.get(1)?, kind: parse_kind(&r.get::<_, String>(2)?)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn channel(&self, id: ChannelId) -> Result<Option<Channel>> {
        Ok(self
            .conn
            .query_row("SELECT id, name, kind FROM channels WHERE id = ?1", params![id], |r| {
                Ok(Channel { id: r.get(0)?, name: r.get(1)?, kind: parse_kind(&r.get::<_, String>(2)?)? })
            })
            .optional()?)
    }

    // ---------- mesajlar ----------

    pub fn insert_message(&self, channel_id: ChannelId, user_id: UserId, text: &str) -> Result<ChatMessage> {
        let ts = now_secs();
        self.conn.execute(
            "INSERT INTO messages (channel_id, user_id, text, ts) VALUES (?1, ?2, ?3, ?4)",
            params![channel_id, user_id, text, ts],
        )?;
        Ok(ChatMessage { id: self.conn.last_insert_rowid(), channel_id, user_id, text: text.to_string(), ts })
    }

    /// `before_id`'den eski en fazla `limit` mesaj, eskiden yeniye sıralı.
    pub fn history(&self, channel_id: ChannelId, before_id: Option<MessageId>, limit: u32) -> Result<Vec<ChatMessage>> {
        let mut st = self.conn.prepare(
            "SELECT id, channel_id, user_id, text, ts FROM messages
             WHERE channel_id = ?1 AND id < ?2 ORDER BY id DESC LIMIT ?3",
        )?;
        let rows = st.query_map(params![channel_id, before_id.unwrap_or(i64::MAX), limit], |r| {
            Ok(ChatMessage { id: r.get(0)?, channel_id: r.get(1)?, user_id: r.get(2)?, text: r.get(3)?, ts: r.get(4)? })
        })?;
        let mut v: Vec<ChatMessage> = rows.collect::<rusqlite::Result<_>>()?;
        v.reverse();
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Db {
        Db::open(Path::new(":memory:")).unwrap()
    }

    #[test]
    fn default_channels_seeded_once() {
        let db = mem();
        let names: Vec<_> = db.channels().unwrap().into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["genel", "Oyun", "AFK"]);
    }

    #[test]
    fn token_login_and_reset() {
        let db = mem();
        let (u, t1) = db.add_user("Ali").unwrap();
        assert_eq!(db.user_by_token(&t1).unwrap(), Some(u.clone()));
        assert!(db.user_by_token("yanlis").unwrap().is_none());
        let (_, t2) = db.reset_token("ali").unwrap(); // büyük/küçük harf duyarsız
        assert!(db.user_by_token(&t1).unwrap().is_none());
        assert_eq!(db.user_by_token(&t2).unwrap(), Some(u));
    }

    #[test]
    fn duplicate_and_invalid_names_rejected() {
        let db = mem();
        db.add_user("Veli").unwrap();
        assert!(db.add_user("veli").is_err());
        assert!(db.add_user("   ").is_err());
        assert!(db.add_user(&"x".repeat(MAX_NAME_CHARS + 1)).is_err());
    }

    #[test]
    fn history_paging() {
        let db = mem();
        let (u, _) = db.add_user("A").unwrap();
        let ch = db.channels().unwrap()[0].id;
        let ids: Vec<_> = (0..5).map(|i| db.insert_message(ch, u.id, &format!("m{i}")).unwrap().id).collect();
        let last2 = db.history(ch, None, 2).unwrap();
        assert_eq!(last2.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(), ["m3", "m4"]);
        let before = db.history(ch, Some(ids[3]), 10).unwrap();
        assert_eq!(before.len(), 3);
        assert_eq!(before[0].text, "m0");
    }

    #[test]
    fn removing_user_removes_messages() {
        let db = mem();
        let (u, _) = db.add_user("A").unwrap();
        let ch = db.channels().unwrap()[0].id;
        db.insert_message(ch, u.id, "x").unwrap();
        assert!(db.remove_user("A").unwrap());
        assert!(db.history(ch, None, 10).unwrap().is_empty());
    }
}

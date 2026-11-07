//! Kullanıcı ayarları: `%APPDATA%\Corddis\config.json`.
//! Davet bağlantısı (içinde giriş token'ı var) DPAPI ile şifreli saklanır.

use std::collections::HashMap;
use std::path::PathBuf;

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::audio::capture::CaptureSettings;
use crate::platform;
use crate::AudioOptions;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// DPAPI ile şifrelenmiş davet bağlantısı (base64).
    invite: Option<String>,
    pub input: Option<String>,
    pub output: Option<String>,
    pub denoise: bool,
    /// Bu seviyenin altındaki ses konuşma sayılmaz (dBFS). Düşük = daha hassas.
    pub vad_min_db: f32,
    pub vad_prob: f32,
    /// Otomatik ses seviyesi (kısık mikrofonu yükseltir).
    pub agc: bool,
    /// Kanala giriş/çıkış ve mikrofon aç/kapat sesleri.
    pub sounds: bool,
    /// Kişi başına ses düzeyi (kullanıcı adı → 0.0–3.0).
    pub volumes: HashMap<String, f32>,
}

impl Default for Config {
    fn default() -> Self {
        let c = CaptureSettings::default();
        Self {
            invite: None,
            input: None,
            output: None,
            denoise: c.denoise,
            vad_min_db: c.vad_min_db,
            vad_prob: c.vad_prob,
            agc: c.agc,
            sounds: true,
            volumes: HashMap::new(),
        }
    }
}

/// `CORDDIS_CONFIG_DIR` verilirse o klasör kullanılır (aynı bilgisayarda ikinci hesapla test için).
fn path() -> Option<PathBuf> {
    let dir = match std::env::var_os("CORDDIS_CONFIG_DIR") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(std::env::var_os("APPDATA")?).join("Corddis"),
    };
    Some(dir.join("config.json"))
}

impl Config {
    /// Dosya yoksa veya bozuksa varsayılan ayarlar.
    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            // Önce geçici dosyaya yaz, sonra taşı: yazarken kapanırsa ayarlar bozulmaz.
            let tmp = p.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, &p);
            }
        }
    }

    pub fn invite(&self) -> Option<String> {
        let enc = base64::engine::general_purpose::STANDARD.decode(self.invite.as_ref()?).ok()?;
        String::from_utf8(platform::unprotect(&enc)?).ok()
    }

    pub fn set_invite(&mut self, invite: Option<&str>) {
        self.invite = invite
            .and_then(|i| platform::protect(i.as_bytes()))
            .map(|b| base64::engine::general_purpose::STANDARD.encode(b));
    }

    pub fn audio_options(&self) -> AudioOptions {
        AudioOptions {
            input: self.input.clone(),
            output: self.output.clone(),
            capture: CaptureSettings {
                denoise: self.denoise,
                vad_prob: self.vad_prob,
                vad_min_db: self.vad_min_db,
                agc: self.agc,
                ..CaptureSettings::default()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_is_not_stored_in_plain_text() {
        let mut c = Config::default();
        c.set_invite(Some("corddis://h:443?fp=AA&token=gizli-token"));
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("gizli-token"));
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back.invite().as_deref(), Some("corddis://h:443?fp=AA&token=gizli-token"));
    }

    #[test]
    fn missing_fields_use_defaults() {
        let c: Config = serde_json::from_str(r#"{"output":"Kulaklık"}"#).unwrap();
        assert_eq!(c.output.as_deref(), Some("Kulaklık"));
        assert!(c.denoise);
        assert_eq!(c.vad_min_db, -50.0);
    }
}

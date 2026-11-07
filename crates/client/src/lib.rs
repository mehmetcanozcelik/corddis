//! Corddis istemcisi: ses hattı, ağ, arayüz.
//!
//! İki çalıştırılabilir dosya bu kütüphaneyi kullanır:
//! - `corddis.exe` — arayüz (konsol penceresi açmaz)
//! - `corddis-cli.exe` — komut satırı araçları (`devices`, `loopback`, `chat`)

pub mod audio;
pub mod chat;
pub mod config;
pub mod gui;
pub mod hotkey;
pub mod loopback;
pub mod model;
pub mod net;
pub mod netloop;
pub mod platform;
pub mod tray;
pub mod voice;

use audio::capture::CaptureSettings;

/// Ses cihazı ve mikrofon hattı ayarları.
#[derive(Clone, Default)]
pub struct AudioOptions {
    pub input: Option<String>,
    pub output: Option<String>,
    pub capture: CaptureSettings,
}

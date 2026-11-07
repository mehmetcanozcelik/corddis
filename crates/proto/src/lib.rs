//! İstemci ve sunucunun ortak kullandığı tanımlar (MVP.md §6–7, §10).

pub mod audio;
pub mod control;
pub mod invite;
#[cfg(feature = "tls")]
pub mod tls;
pub mod voice;

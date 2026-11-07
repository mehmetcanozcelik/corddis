//! Ses hattı sabitleri (MVP.md §6).

/// Tüm iç işlemler bu örnekleme hızında yapılır; cihaz farklıysa istemci dönüştürür.
pub const SAMPLE_RATE: u32 = 48_000;
/// Bir Opus çerçevesinin süresi.
pub const FRAME_MS: u32 = 20;
/// Bir Opus çerçevesindeki örnek sayısı (mono).
pub const FRAME_SAMPLES: usize = (SAMPLE_RATE / 1000 * FRAME_MS) as usize; // 960
/// RNNoise/VAD'nin işlediği alt çerçeve (10 ms).
pub const SUBFRAME_SAMPLES: usize = FRAME_SAMPLES / 2; // 480
/// Varsayılan Opus bitrate'i.
pub const DEFAULT_BITRATE: i32 = 32_000;
/// Opus paketinin olabileceği en büyük boyut (20 ms, 64 kbps'de ~160 byte; geniş pay).
pub const MAX_OPUS_PACKET: usize = 400;

//! Mikrofon tarafı ses hattı (MVP.md §6):
//! cihaz hızı → 48 kHz → RNNoise (10 ms) → ses algılama → 20 ms çerçeve → Opus.
//!
//! Hem `loopback` hem gerçek ses motoru bunu kullanır.

use std::time::Instant;

use anyhow::Result;
use corddis_proto::audio::{FRAME_SAMPLES, SAMPLE_RATE, SUBFRAME_SAMPLES};
use nnnoiseless::DenoiseState;

use super::codec::Encoder;
use super::resample::Resampler;
use super::vad::{rms_dbfs, VadGate};

/// Ses algılama kapandıktan sonra açık kalma süresi: 30 × 10 ms = 300 ms.
const VAD_HANGOVER_SUBFRAMES: u32 = 30;

#[derive(Clone, Debug)]
pub struct CaptureSettings {
    pub denoise: bool,
    pub vad_prob: f32,
    pub vad_min_db: f32,
    pub bitrate: i32,
}

impl Default for CaptureSettings {
    fn default() -> Self {
        Self { denoise: true, vad_prob: 0.5, vad_min_db: -50.0, bitrate: corddis_proto::audio::DEFAULT_BITRATE }
    }
}

pub enum Event<'a> {
    /// Konuşma başladı (ilk `Voice`'tan hemen önce).
    Started { level_db: f32, prob: f32 },
    /// Kodlanmış 20 ms'lik Opus çerçevesi.
    Voice(&'a [u8]),
    /// Konuşma bitti.
    Ended,
    /// Her 20 ms'de bir (ses olsun olmasın), `Voice`/`Ended`'dan sonra.
    Tick,
}

#[derive(Debug, Clone)]
pub struct CaptureStats {
    pub subframes: u64,
    pub subframes_open: u64,
    pub peak_db: f32,
    pub denoise_ns: u128,
    pub enc_ns: u128,
    pub frames_sent: u64,
    pub payload_bytes: u64,
}

impl Default for CaptureStats {
    fn default() -> Self {
        Self { subframes: 0, subframes_open: 0, peak_db: -100.0, denoise_ns: 0, enc_ns: 0, frames_sent: 0, payload_bytes: 0 }
    }
}

pub struct CaptureProcessor {
    denoise_enabled: bool,
    denoise: Box<DenoiseState<'static>>,
    gate: VadGate,
    enc: Encoder,
    rs: Resampler,
    pending: Vec<f32>,
    frame: [f32; FRAME_SAMPLES],
    frame_len: usize,
    frame_active: bool,
    talking: bool,
    scaled: [f32; SUBFRAME_SAMPLES],
    denoised: [f32; SUBFRAME_SAMPLES],
    stats: CaptureStats,
    level_db: f32,
}

impl CaptureProcessor {
    pub fn new(in_rate: u32, s: &CaptureSettings) -> Result<Self> {
        Ok(Self {
            denoise_enabled: s.denoise,
            denoise: DenoiseState::new(),
            gate: VadGate::new(s.vad_prob, s.vad_min_db, VAD_HANGOVER_SUBFRAMES),
            enc: Encoder::new(s.bitrate)?,
            rs: Resampler::new(in_rate, SAMPLE_RATE),
            pending: Vec::with_capacity(8192),
            frame: [0.0; FRAME_SAMPLES],
            frame_len: 0,
            frame_active: false,
            talking: false,
            scaled: [0.0; SUBFRAME_SAMPLES],
            denoised: [0.0; SUBFRAME_SAMPLES],
            stats: CaptureStats::default(),
            level_db: -100.0,
        })
    }

    pub fn talking(&self) -> bool {
        self.talking
    }

    /// Son 10 ms'nin mikrofon seviyesi (dBFS); mute'tayken -100.
    pub fn level_db(&self) -> f32 {
        self.level_db
    }

    /// Ses algılama ayarlarını çalışırken değiştirir.
    pub fn set_vad(&mut self, prob: f32, min_db: f32) {
        self.gate.prob_threshold = prob;
        self.gate.min_db = min_db;
    }

    pub fn set_denoise(&mut self, on: bool) {
        self.denoise_enabled = on;
    }

    pub fn take_stats(&mut self) -> CaptureStats {
        std::mem::take(&mut self.stats)
    }

    /// Cihazdan gelen mono örnekleri işler. Mute'tayken hiçbir işlem yapılmaz (CPU ~0);
    /// konuşma sürerken mute'a basıldıysa yalnızca `Ended` üretilir.
    pub fn process(&mut self, raw: &[f32], muted: bool, mut emit: impl FnMut(Event<'_>) -> Result<()>) -> Result<()> {
        if muted {
            self.level_db = -100.0;
            self.pending.clear();
            self.frame_len = 0;
            self.frame_active = false;
            if self.talking {
                self.talking = false;
                emit(Event::Ended)?;
            }
            return Ok(());
        }

        self.rs.process(raw, &mut self.pending);
        let mut off = 0;
        while self.pending.len() - off >= SUBFRAME_SAMPLES {
            let chunk = &self.pending[off..off + SUBFRAME_SAMPLES];
            off += SUBFRAME_SAMPLES;

            // --- 10 ms: gürültü engelleme + ses algılama ---
            let level = rms_dbfs(chunk);
            self.level_db = level;
            for (d, s) in self.scaled.iter_mut().zip(chunk) {
                *d = s * 32768.0; // RNNoise 16-bit ölçeğinde çalışır
            }
            let t = Instant::now();
            let prob = self.denoise.process_frame(&mut self.denoised, &self.scaled);
            self.stats.denoise_ns += t.elapsed().as_nanos();

            let open = self.gate.update(prob, level);
            self.stats.subframes += 1;
            self.stats.subframes_open += open as u64;
            self.stats.peak_db = self.stats.peak_db.max(level);

            let dst = &mut self.frame[self.frame_len..self.frame_len + SUBFRAME_SAMPLES];
            if self.denoise_enabled {
                for (d, s) in dst.iter_mut().zip(&self.denoised) {
                    *d = s / 32768.0;
                }
            } else {
                dst.copy_from_slice(chunk);
            }
            self.frame_len += SUBFRAME_SAMPLES;
            self.frame_active |= open;
            if self.frame_len < FRAME_SAMPLES {
                continue;
            }

            // --- 20 ms: kodla ---
            self.frame_len = 0;
            if self.frame_active {
                if !self.talking {
                    self.talking = true;
                    emit(Event::Started { level_db: level, prob })?;
                }
                let t = Instant::now();
                let pkt = self.enc.encode(&self.frame)?;
                self.stats.enc_ns += t.elapsed().as_nanos();
                self.stats.frames_sent += 1;
                self.stats.payload_bytes += pkt.len() as u64;
                emit(Event::Voice(pkt))?;
            } else if self.talking {
                self.talking = false;
                emit(Event::Ended)?;
            }
            self.frame_active = false;
            emit(Event::Tick)?;
        }
        self.pending.drain(..off);
        Ok(())
    }
}

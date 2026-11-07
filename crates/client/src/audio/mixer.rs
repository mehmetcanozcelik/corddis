//! Gelen sesler: konuşmacı başına jitter buffer + Opus çözücü, hepsinin karışımı.
//!
//! Jitter buffer hedefi ağdaki oynamaya göre uyarlanır (RFC 3550 tarzı jitter tahmini):
//! sakin ağda 40 ms, oynak ağda en fazla 120 ms (bkz. `target_for`). Hedef yalnızca konuşma başlarında
//! değişir, konuşma ortasında ses bozulmaz.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Result;
use corddis_proto::audio::{FRAME_MS, FRAME_SAMPLES};

use super::agc::Limiter;
use super::codec::Decoder;
use super::jitter::{JitterBuffer, Playout};

const MIN_TARGET_FRAMES: usize = 2;
const MAX_TARGET_FRAMES: usize = 6;
/// Bu kadar süredir paket gelmeyen konuşmacının çözücüsü bellekten atılır.
const IDLE_FORGET: Duration = Duration::from_secs(30);
/// Kişi başı ses düzeyi üst sınırı (%300); karışım sınırlayıcıdan geçtiği için cızırdamaz.
pub const MAX_VOLUME: f32 = 3.0;

#[derive(Debug, Clone, Copy, Default)]
pub struct SpeakerStats {
    pub frames: u64,
    pub fec: u64,
    pub plc: u64,
    pub late: u64,
    pub jitter_ms: f32,
    pub target_frames: usize,
}

struct Speaker {
    jb: JitterBuffer,
    dec: Decoder,
    last_packet: Instant,
    prev_transit: Option<f64>,
    jitter_ms: f64,
    stats: SpeakerStats,
}

pub struct Mixer {
    epoch: Instant,
    speakers: HashMap<u16, Speaker>,
    volumes: HashMap<u16, f32>,
    decoded: [f32; FRAME_SAMPLES],
    limiter: Limiter,
}

impl Default for Mixer {
    fn default() -> Self {
        Self { epoch: Instant::now(), speakers: HashMap::new(), volumes: HashMap::new(), decoded: [0.0; FRAME_SAMPLES], limiter: Limiter::default() }
    }
}

/// Tampon ≈ bir çerçeve + 3 × jitter, en az 40 ms. Sakin ağda (jitter < ~7 ms) 40 ms'de kalır.
fn target_for(jitter_ms: f64) -> usize {
    let ms = FRAME_MS as f64 + 3.0 * jitter_ms;
    ((ms / FRAME_MS as f64).ceil() as usize).clamp(MIN_TARGET_FRAMES, MAX_TARGET_FRAMES)
}

impl Mixer {
    fn speaker(&mut self, id: u16) -> Result<&mut Speaker> {
        if !self.speakers.contains_key(&id) {
            let s = Speaker {
                jb: JitterBuffer::new(MIN_TARGET_FRAMES),
                dec: Decoder::new()?,
                last_packet: Instant::now(),
                prev_transit: None,
                jitter_ms: 0.0,
                stats: SpeakerStats { target_frames: MIN_TARGET_FRAMES, ..Default::default() },
            };
            self.speakers.insert(id, s);
        }
        Ok(self.speakers.get_mut(&id).unwrap())
    }

    pub fn push(&mut self, id: u16, seq: u32, data: Vec<u8>) -> Result<()> {
        let now_ms = self.epoch.elapsed().as_secs_f64() * 1000.0;
        let s = self.speaker(id)?;
        s.last_packet = Instant::now();
        let transit = now_ms - seq as f64 * FRAME_MS as f64;
        if let Some(prev) = s.prev_transit {
            let d = (transit - prev).abs();
            // Konuşmalar arası uzun sessizlik jitter sayılmaz.
            if d < 1000.0 {
                s.jitter_ms += (d - s.jitter_ms) / 16.0;
            }
        }
        s.prev_transit = Some(transit);
        let late_before = s.jb.late_drops;
        s.jb.push(seq, data);
        s.stats.late += s.jb.late_drops - late_before;
        Ok(())
    }

    pub fn end(&mut self, id: u16, seq: u32) {
        if let Some(s) = self.speakers.get_mut(&id) {
            s.jb.mark_end(seq);
        }
    }

    pub fn set_volume(&mut self, id: u16, volume: f32) {
        self.volumes.insert(id, volume.clamp(0.0, MAX_VOLUME));
    }

    /// 20 ms'lik karışım üretir. Kimse konuşmuyorsa `false` döner (çıktı sıfırlanır).
    pub fn mix(&mut self, out: &mut [f32; FRAME_SAMPLES]) -> Result<bool> {
        out.fill(0.0);
        let mut any = false;
        let mut forget = Vec::new();
        for (&id, s) in self.speakers.iter_mut() {
            if !s.jb.is_playing() {
                let t = target_for(s.jitter_ms);
                s.jb.set_target(t);
                s.stats.target_frames = t;
            }
            match s.jb.pop() {
                Playout::Frame(d) => s.dec.decode(&d, &mut self.decoded)?,
                Playout::Conceal { fec: Some(next) } => {
                    s.stats.fec += 1;
                    s.dec.decode_fec(&next, &mut self.decoded)?
                }
                Playout::Conceal { fec: None } => {
                    s.stats.plc += 1;
                    s.dec.conceal(&mut self.decoded)?
                }
                Playout::Silence => {
                    if s.last_packet.elapsed() > IDLE_FORGET {
                        forget.push(id);
                    }
                    continue;
                }
            }
            s.stats.frames += 1;
            let vol = self.volumes.get(&id).copied().unwrap_or(1.0);
            for (o, d) in out.iter_mut().zip(&self.decoded) {
                *o += d * vol;
            }
            any = true;
        }
        for id in forget {
            self.speakers.remove(&id);
        }
        if any {
            self.limiter.process(out);
        }
        Ok(any)
    }

    /// Şu an sesi çalınan konuşmacılar.
    pub fn speaking(&self) -> Vec<u16> {
        let mut v: Vec<u16> = self.speakers.iter().filter(|(_, s)| s.jb.is_playing()).map(|(&id, _)| id).collect();
        v.sort();
        v
    }

    pub fn stats(&self) -> Vec<(u16, SpeakerStats)> {
        let mut v: Vec<_> = self
            .speakers
            .iter()
            .map(|(&id, s)| (id, SpeakerStats { jitter_ms: s.jitter_ms as f32, ..s.stats }))
            .collect();
        v.sort_by_key(|(id, _)| *id);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::codec::Encoder;

    fn tone(seq: u32) -> [f32; FRAME_SAMPLES] {
        let mut f = [0f32; FRAME_SAMPLES];
        for (i, s) in f.iter_mut().enumerate() {
            let t = (seq as usize * FRAME_SAMPLES + i) as f32 / 48_000.0;
            *s = 0.3 * (2.0 * std::f32::consts::PI * 440.0 * t).sin();
        }
        f
    }

    #[test]
    fn target_grows_with_jitter() {
        assert_eq!(target_for(0.0), 2);
        assert_eq!(target_for(3.0), 2); // localhost'ta ölçülen ~3 ms
        assert_eq!(target_for(10.0), 3);
        assert_eq!(target_for(30.0), 6);
        assert_eq!(target_for(500.0), MAX_TARGET_FRAMES);
    }

    #[test]
    fn mixes_two_speakers_and_reports_speaking() {
        let mut enc = Encoder::new(32_000).unwrap();
        let mut m = Mixer::default();
        for seq in 0..3 {
            let p = enc.encode(&tone(seq)).unwrap().to_vec();
            m.push(1, seq, p.clone()).unwrap();
            m.push(2, seq, p).unwrap();
        }
        let mut out = [0f32; FRAME_SAMPLES];
        assert!(m.mix(&mut out).unwrap());
        assert_eq!(m.speaking(), vec![1, 2]);
        m.set_volume(2, 0.0);
        assert!(m.mix(&mut out).unwrap());
        // Konuşma bitince sessizlik.
        m.end(1, 3);
        m.end(2, 3);
        assert!(m.mix(&mut out).unwrap());
        assert!(!m.mix(&mut out).unwrap());
        assert!(m.speaking().is_empty());
        assert!(out.iter().all(|&s| s == 0.0));
    }
}

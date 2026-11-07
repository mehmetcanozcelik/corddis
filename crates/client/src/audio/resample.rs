//! Basit, akışlı doğrusal (linear) yeniden örnekleyici.
//!
//! Çoğu Windows cihazı zaten 48 kHz çalıştığı için çoğu zaman devre dışıdır (passthrough).
//! Konuşma için yeterli kalitededir; gerekirse ileride `rubato` ile değiştirilebilir.

pub struct Resampler {
    /// Girdi örneği başına çıktı adımı (in_rate / out_rate).
    step: f64,
    /// Bir sonraki çıktı örneğinin, mevcut girdi bloğuna göre konumu (-1 = önceki bloğun son örneği).
    pos: f64,
    prev: f32,
    passthrough: bool,
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        Self {
            step: in_rate as f64 / out_rate as f64,
            pos: 0.0,
            prev: 0.0,
            passthrough: in_rate == out_rate,
        }
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.passthrough {
            out.extend_from_slice(input);
            return;
        }
        let n = input.len();
        if n == 0 {
            return;
        }
        let last = n as f64 - 1.0;
        while self.pos < last {
            let i = self.pos.floor();
            let frac = (self.pos - i) as f32;
            let i = i as isize;
            let a = if i < 0 { self.prev } else { input[i as usize] };
            let b = input[(i + 1) as usize];
            out.push(a + (b - a) * frac);
            self.pos += self.step;
        }
        self.pos -= n as f64;
        self.prev = input[n - 1];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_when_equal() {
        let mut r = Resampler::new(48_000, 48_000);
        let mut out = Vec::new();
        r.process(&[1.0, 2.0, 3.0], &mut out);
        assert_eq!(out, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn output_length_tracks_ratio_across_chunks() {
        let mut r = Resampler::new(44_100, 48_000);
        let mut out = Vec::new();
        let chunk = vec![0.5f32; 441];
        for _ in 0..100 {
            r.process(&chunk, &mut out);
        }
        // 44100 girdi → ~48000 çıktı (sınırda en fazla bir örnek fark).
        assert!((out.len() as i64 - 48_000).abs() <= 2, "len = {}", out.len());
        // İlk örnek, başlangıçtaki 0 ile 0.5 arasında enterpole edilir; gerisi tam 0.5.
        assert!(out.iter().all(|&s| (0.0..=0.5).contains(&s)));
        assert!(out[1..].iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn downsample_ramp_is_monotonic() {
        let mut r = Resampler::new(96_000, 48_000);
        let input: Vec<f32> = (0..960).map(|i| i as f32).collect();
        let mut out = Vec::new();
        r.process(&input[..500], &mut out);
        r.process(&input[500..], &mut out);
        assert!(out.windows(2).all(|w| w[1] > w[0]));
        assert!((out.len() as i64 - 480).abs() <= 1);
    }
}

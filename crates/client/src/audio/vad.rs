//! Ses algılama kapısı (MVP.md §6).
//!
//! Mikrofon sürekli açık olduğu için "şu an konuşuyor mu?" kararını verir.
//! Konuşma = RNNoise konuşma olasılığı eşiği geçti **ve** ses seviyesi eşiği geçti.
//! Böylece tek başına bir klavye tıklaması (yüksek ses, düşük olasılık) kapıyı açmaz.
//! Açılma anında, kapanma `hangover` kadar gecikmeli (kelime sonları kesilmesin).

pub struct VadGate {
    pub prob_threshold: f32,
    pub min_db: f32,
    hangover: u32,
    remaining: u32,
}

impl VadGate {
    /// `hangover_frames`: 10 ms'lik alt çerçeve sayısı (30 = 300 ms).
    pub fn new(prob_threshold: f32, min_db: f32, hangover_frames: u32) -> Self {
        Self { prob_threshold, min_db, hangover: hangover_frames, remaining: 0 }
    }

    /// Her 10 ms'lik alt çerçeve için çağrılır; kapı açıksa `true`.
    pub fn update(&mut self, speech_prob: f32, level_db: f32) -> bool {
        if speech_prob >= self.prob_threshold && level_db >= self.min_db {
            self.remaining = self.hangover;
            true
        } else if self.remaining > 0 {
            self.remaining -= 1;
            true
        } else {
            false
        }
    }
}

/// [-1, 1] aralığındaki örneklerin RMS seviyesi, dBFS.
pub fn rms_dbfs(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return -100.0;
    }
    let ms = samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32;
    10.0 * ms.max(1e-10).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_immediately_and_holds_for_hangover() {
        let mut v = VadGate::new(0.5, -50.0, 3);
        assert!(!v.update(0.1, -20.0));
        assert!(v.update(0.9, -20.0));
        assert!(v.update(0.0, -90.0));
        assert!(v.update(0.0, -90.0));
        assert!(v.update(0.0, -90.0));
        assert!(!v.update(0.0, -90.0));
    }

    #[test]
    fn loud_non_speech_does_not_open() {
        let mut v = VadGate::new(0.5, -50.0, 3);
        assert!(!v.update(0.2, -10.0)); // klavye: yüksek ses, düşük olasılık
    }

    #[test]
    fn quiet_speech_below_floor_does_not_open() {
        let mut v = VadGate::new(0.5, -50.0, 3);
        assert!(!v.update(0.9, -65.0)); // uzaktan gelen konuşma
    }

    #[test]
    fn rms_levels() {
        assert!((rms_dbfs(&[1.0; 480]) - 0.0).abs() < 0.01);
        assert!((rms_dbfs(&[0.1; 480]) + 20.0).abs() < 0.01);
        assert_eq!(rms_dbfs(&[0.0; 480]), -100.0);
    }
}

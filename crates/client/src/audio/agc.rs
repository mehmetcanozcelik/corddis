//! Ses seviyesi işleme.
//!
//! - [`Agc`]: otomatik kazanç (gönderen tarafta). Kısık mikrofonu hedef seviyeye yükseltir,
//!   yüksek olanı kısar. Yalnızca konuşma sırasında uyarlanır; sessizlikte kazanç sabit kalır,
//!   böylece oda gürültüsü yükseltilmez.
//! - [`Limiter`]: tepe sınırlayıcı (dinleyen tarafta). Kişi sesleri %300'e kadar yükseltilip
//!   karıştırıldığında cızırtı/kırpma olmasın diye.

/// Konuşmanın "zarf" seviyesi bu değere çekilir (10 ms RMS'lerin yavaş sönen tepesi).
const TARGET_DB: f32 = -18.0;
const MAX_GAIN_DB: f32 = 24.0;
const MIN_GAIN_DB: f32 = -12.0;
/// Kazanç artışı en fazla 10 ms başına bu kadar (30 dB/sn): konuşma ortasında "pompalama" olmasın.
const RISE_DB_PER_STEP: f32 = 0.3;
/// Kazanç düşüşü hızlı: hedefin üstüne çıkınca farkın yarısı kadar her 10 ms'de.
const FALL_FRACTION: f32 = 0.5;
/// Bu seviyenin altı konuşma sayılsa bile uyarlamaya katılmaz.
const MIN_LEVEL_DB: f32 = -70.0;
const PEAK_CEILING: f32 = 0.9;

pub fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub fn lin_to_db(x: f32) -> f32 {
    20.0 * x.max(1e-6).log10()
}

pub fn rms_db(x: &[f32]) -> f32 {
    if x.is_empty() {
        return -120.0;
    }
    let ms = x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32;
    10.0 * ms.max(1e-12).log10()
}

pub struct Agc {
    gain_db: f32,
    /// Konuşma zarfı (dBFS): yükselince hemen, düşünce yavaş takip eder.
    envelope_db: Option<f32>,
    prev_gain: f32,
}

impl Default for Agc {
    fn default() -> Self {
        Self { gain_db: 0.0, envelope_db: None, prev_gain: 1.0 }
    }
}

impl Agc {
    pub fn gain_db(&self) -> f32 {
        self.gain_db
    }

    /// 10 ms'lik bloğu yerinde işler. `speech`: ses algılama kapısı açık mı.
    pub fn process(&mut self, x: &mut [f32], speech: bool) {
        let level = rms_db(x);
        if speech && level > MIN_LEVEL_DB {
            let env = match self.envelope_db {
                // Yükselişte hemen, düşüşte ~0.5 sn zaman sabitiyle.
                Some(e) if level < e => e + (level - e) * 0.02,
                _ => level,
            };
            self.envelope_db = Some(env);
            let desired = (TARGET_DB - env).clamp(MIN_GAIN_DB, MAX_GAIN_DB);
            let diff = desired - self.gain_db;
            self.gain_db += if diff > 0.0 { diff.min(RISE_DB_PER_STEP) } else { diff * FALL_FRACTION };
        }

        // Blok boyunca eski kazançtan yenisine doğrusal geçiş (tıkırtı olmasın).
        let target = db_to_lin(self.gain_db);
        let n = x.len().max(1) as f32;
        let mut peak = 0f32;
        for (i, s) in x.iter_mut().enumerate() {
            let g = self.prev_gain + (target - self.prev_gain) * (i as f32 + 1.0) / n;
            *s *= g;
            peak = peak.max(s.abs());
        }
        let mut applied = target;
        if peak > PEAK_CEILING {
            // Kırpılacaktı: bloğu tavana indir, kazancı da o kadar düşür.
            let k = PEAK_CEILING / peak;
            x.iter_mut().for_each(|s| *s *= k);
            applied *= k;
            self.gain_db = lin_to_db(applied).max(MIN_GAIN_DB);
        }
        self.prev_gain = applied;
    }
}

/// Karışımın tepesi 1.0'ı aşmasın: anında kısar, yavaşça (~13 dB/sn) geri açar.
pub struct Limiter {
    gain: f32,
}

impl Default for Limiter {
    fn default() -> Self {
        Self { gain: 1.0 }
    }
}

const LIMIT: f32 = 0.95;
const LIMITER_RELEASE: f32 = 1.035; // 20 ms başına (~+0.3 dB)

impl Limiter {
    pub fn process(&mut self, x: &mut [f32]) {
        let peak = x.iter().fold(0f32, |m, s| m.max(s.abs()));
        let needed = if peak * self.gain > LIMIT { LIMIT / peak } else { 1.0 };
        let start = self.gain;
        let end = if needed < self.gain { needed } else { (self.gain * LIMITER_RELEASE).min(needed).min(1.0) };
        let n = x.len().max(1) as f32;
        for (i, s) in x.iter_mut().enumerate() {
            // Kısarken hemen (ilk örnekten itibaren), açarken blok boyunca kademeli.
            let g = if end < start { end } else { start + (end - start) * (i as f32 + 1.0) / n };
            *s = (*s * g).clamp(-1.0, 1.0);
        }
        self.gain = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(amp: f32, n: usize, phase: &mut f32) -> Vec<f32> {
        (0..n)
            .map(|_| {
                *phase += 2.0 * std::f32::consts::PI * 220.0 / 48_000.0;
                amp * phase.sin()
            })
            .collect()
    }

    #[test]
    fn quiet_speech_is_raised_close_to_target() {
        let mut agc = Agc::default();
        let mut ph = 0.0;
        let amp = db_to_lin(-40.0) * std::f32::consts::SQRT_2; // RMS -40 dBFS
        let mut last = -120.0;
        for _ in 0..300 {
            let mut b = tone(amp, 480, &mut ph);
            agc.process(&mut b, true);
            last = rms_db(&b);
        }
        assert!((last - TARGET_DB).abs() < 2.0, "çıkış {last:.1} dBFS");
        assert!(agc.gain_db() > 18.0);
    }

    #[test]
    fn gain_never_exceeds_limits_and_loud_input_is_not_clipped() {
        let mut agc = Agc::default();
        let mut ph = 0.0;
        for _ in 0..100 {
            let mut b = tone(0.99, 480, &mut ph);
            agc.process(&mut b, true);
            assert!(b.iter().all(|s| s.abs() <= PEAK_CEILING + 1e-4));
        }
        assert!(agc.gain_db() < 0.0);

        let mut agc = Agc::default();
        for _ in 0..1000 {
            let mut b = tone(db_to_lin(-65.0), 480, &mut ph);
            agc.process(&mut b, true);
        }
        assert!(agc.gain_db() <= MAX_GAIN_DB + 1e-3);
    }

    #[test]
    fn silence_does_not_change_gain() {
        let mut agc = Agc::default();
        let mut ph = 0.0;
        for _ in 0..200 {
            let mut b = tone(db_to_lin(-50.0), 480, &mut ph);
            agc.process(&mut b, false); // konuşma değil (ör. oda gürültüsü)
        }
        assert_eq!(agc.gain_db(), 0.0);
    }

    #[test]
    fn limiter_keeps_mix_below_full_scale() {
        let mut lim = Limiter::default();
        let mut ph = 0.0;
        for _ in 0..50 {
            let mut b = tone(2.5, 960, &mut ph); // %300'le yükseltilmiş iki kişi gibi
            lim.process(&mut b);
            assert!(b.iter().all(|s| s.abs() <= 1.0));
        }
        let mut quiet = tone(0.3, 960, &mut ph);
        let before = quiet.clone();
        let mut lim = Limiter::default();
        lim.process(&mut quiet);
        assert_eq!(quiet, before, "eşiğin altındaki ses değişmemeli");
    }
}

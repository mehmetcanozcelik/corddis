//! Bildirim sesleri: kanala giriş/çıkış, mikrofon kapat/aç.
//!
//! Ses dosyası yok: kısa iki notalı "ding"ler kodda üretilir (exe'ye 0 bayt, lisans derdi yok).
//! Her ses kendi kısa ömürlü çıkış akışını açar, çalar, kapatır; ses motoru çalışmıyorken de
//! (ör. kanaldan çıkınca) duyulur.

use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use ringbuf::traits::{Producer, Split};
use ringbuf::HeapRb;

use super::device;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    /// Sen ya da biri sesli kanala girdi: yükselen iki nota.
    Join,
    /// Sen ya da biri sesli kanaldan çıktı: alçalan iki nota.
    Leave,
    /// Mikrofon kapandı: pes, kısa.
    Mute,
    /// Mikrofon açıldı: tiz, kısa.
    Unmute,
}

const VOLUME: f32 = 0.22;

/// (frekans Hz, süre sn)
fn notes(s: Sound) -> &'static [(f32, f32)] {
    match s {
        Sound::Join => &[(660.0, 0.075), (990.0, 0.14)],
        Sound::Leave => &[(990.0, 0.075), (587.0, 0.14)],
        Sound::Mute => &[(392.0, 0.06), (294.0, 0.09)],
        Sound::Unmute => &[(523.0, 0.06), (784.0, 0.09)],
    }
}

/// Mono örnekler, `rate` hızında.
pub fn synth(s: Sound, rate: u32) -> Vec<f32> {
    let r = rate as f32;
    let mut out = Vec::new();
    for &(freq, dur) in notes(s) {
        let n = (dur * r) as usize;
        let attack = (0.004 * r) as usize;
        for i in 0..n {
            let t = i as f32 / r;
            let env = if i < attack { i as f32 / attack as f32 } else { (-(t - 0.004) * 28.0).exp() };
            let w = 2.0 * std::f32::consts::PI * freq * t;
            // Biraz ikinci harmonik: kuru sinüsten daha yumuşak, "çan" gibi.
            out.push(VOLUME * env * (w.sin() + 0.18 * (2.0 * w).sin()));
        }
        out.extend(std::iter::repeat_n(0.0, (0.012 * r) as usize));
    }
    out
}

/// Arka planda çalar; çağıranı bekletmez. Cihaz açılamazsa sessizce vazgeçer.
pub fn play(s: Sound, output: Option<String>) {
    let _ = std::thread::Builder::new().name("corddis-sound".into()).spawn(move || {
        let Ok(dev) = device::open_output(output.as_deref()) else { return };
        let samples = synth(s, dev.rate());
        let (mut prod, cons) = HeapRb::<f32>::new(samples.len() + 1).split();
        prod.push_slice(&samples);
        let dur = Duration::from_secs_f32(samples.len() as f32 / dev.rate() as f32);
        // Tampon kırpması bu kısa ses için devre dışı (en fazla 5 sn).
        let Ok(stream) = device::start_playback(&dev, cons, 5000, 5000, Arc::new(AtomicU64::new(0)), None) else {
            return;
        };
        std::thread::sleep(dur + Duration::from_millis(120));
        drop(stream);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sounds_are_short_quiet_and_distinct() {
        for s in [Sound::Join, Sound::Leave, Sound::Mute, Sound::Unmute] {
            let v = synth(s, 48_000);
            let secs = v.len() as f32 / 48_000.0;
            assert!(secs > 0.1 && secs < 0.35, "{s:?}: {secs} sn");
            assert!(v.iter().all(|x| x.abs() <= 0.3), "{s:?} çok yüksek");
        }
        assert_ne!(synth(Sound::Join, 48_000), synth(Sound::Leave, 48_000));
    }
}

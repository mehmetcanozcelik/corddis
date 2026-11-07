//! M1: yerel ses döngüsü.
//!
//! Gerçek istemcideki mikrofon hattını (`CaptureProcessor`) ağ yerine bellekte çalışan sahte
//! bir ağla çalıştırır. Böylece gürültü engelleme, ses algılama eşiği, Opus kalitesi,
//! FEC/PLC ve kaynak kullanımı sunucu olmadan test edilebilir.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use corddis_proto::audio::{FRAME_MS, FRAME_SAMPLES, SAMPLE_RATE};
use corddis_proto::voice::{CLIENT_HEADER_LEN, TAG_LEN, VOICE_SEQ_LEN};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use crate::audio::capture::{CaptureProcessor, Event};
use crate::audio::codec::Decoder;
use crate::audio::device;
use crate::audio::jitter::{JitterBuffer, Playout};
use crate::audio::resample::Resampler;
use crate::platform;
use crate::AudioOptions;

/// Çalma tamponu bu kadarı aşarsa (cihaz saatleri arasındaki kayma) kırpılır.
const PLAYBACK_MAX_MS: u32 = 60;
const PLAYBACK_TRIM_TO_MS: u32 = 30;
/// IPv4 (20) + UDP (8) başlıkları.
const IP_UDP_OVERHEAD: usize = 28;
/// Durum satırı her 100 çerçevede (2 sn) bir yazılır.
const STATS_EVERY_FRAMES: u64 = 100;

#[derive(Clone)]
pub struct Options {
    pub audio: AudioOptions,
    pub loss_pct: f32,
    pub jitter_ms: u32,
    pub jitter_target: usize,
    /// Verilirse Enter beklemeden bu kadar saniye sonra kapanır (ölçüm/otomasyon için).
    pub seconds: Option<u64>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            audio: AudioOptions::default(),
            loss_pct: 0.0,
            jitter_ms: 0,
            jitter_target: 2,
            seconds: None,
        }
    }
}

pub fn run(o: Options) -> Result<()> {
    let input = device::open_input(o.audio.input.as_deref())?;
    let output = device::open_output(o.audio.output.as_deref())?;
    println!("Mikrofon : {} ({} Hz, {} kanal)", input.name, input.rate(), input.channels());
    println!("Çıkış    : {} ({} Hz, {} kanal)", output.name, output.rate(), output.channels());
    let c = &o.audio.capture;
    println!(
        "Ayarlar  : gürültü engelleme {}, VAD olasılık ≥ {}, seviye ≥ {} dBFS, {} kbps, \
         sahte ağ kayıp %{} / jitter {} ms, jitter buffer {} çerçeve",
        if c.denoise { "açık" } else { "kapalı" },
        c.vad_prob,
        c.vad_min_db,
        c.bitrate / 1000,
        o.loss_pct,
        o.jitter_ms,
        o.jitter_target
    );

    let (cap_prod, cap_cons) = HeapRb::<f32>::new(input.rate() as usize).split();
    let (play_prod, play_cons) = HeapRb::<f32>::new((output.rate() / 2) as usize).split();
    let stop = Arc::new(AtomicBool::new(false));
    let overruns = Arc::new(AtomicU64::new(0));
    let underruns = Arc::new(AtomicU64::new(0));

    let worker = {
        let (o, stop, overruns, underruns) = (o.clone(), stop.clone(), overruns.clone(), underruns.clone());
        let rates = (input.rate(), output.rate());
        thread::Builder::new()
            .name("corddis-audio".into())
            .spawn(move || process(o, rates, cap_cons, play_prod, stop, overruns, underruns))?
    };

    let _capture = device::start_capture(&input, cap_prod, worker.thread().clone(), overruns)?;
    let _playback =
        device::start_playback(&output, play_cons, PLAYBACK_MAX_MS, PLAYBACK_TRIM_TO_MS, underruns, None)?;

    match o.seconds {
        Some(s) => {
            println!("\nKonuş ve kendi sesini dinle. {s} sn sonra kapanacak.\n");
            thread::sleep(Duration::from_secs(s));
        }
        None => {
            println!("\nKonuş ve kendi sesini dinle. Çıkmak için Enter.\n");
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
        }
    }

    stop.store(true, Ordering::Relaxed);
    worker.thread().unpark();
    worker.join().map_err(|_| anyhow!("ses thread'i çöktü"))?
}

#[derive(Default)]
struct PlayStats {
    fec: u64,
    plc: u64,
    dec_ns: u128,
    decoded: u64,
}

fn process(
    o: Options,
    (in_rate, out_rate): (u32, u32),
    mut cap: HeapCons<f32>,
    mut play: HeapProd<f32>,
    stop: Arc<AtomicBool>,
    overruns: Arc<AtomicU64>,
    underruns: Arc<AtomicU64>,
) -> Result<()> {
    let boosted = platform::boost_audio_thread();
    println!("Ses thread'i MMCSS \"Pro Audio\" önceliği: {}", if boosted { "açık" } else { "alınamadı" });

    let mut capture = CaptureProcessor::new(in_rate, &o.audio.capture)?;
    let mut dec = Decoder::new()?;
    let mut jb = JitterBuffer::new(o.jitter_target);
    let mut net = SimNet::new(o.loss_pct, o.jitter_ms);
    let mut rs_out = Resampler::new(SAMPLE_RATE, out_rate);

    let mut raw = vec![0f32; 4096];
    let mut decoded = [0f32; FRAME_SAMPLES];
    let mut out_buf: Vec<f32> = Vec::with_capacity(4096);
    let mut seq: u32 = 0;
    let mut tick: u64 = 0;
    let mut ps = PlayStats::default();
    let mut proc_stats = platform::ProcStats::new();

    while !stop.load(Ordering::Relaxed) {
        let n = cap.pop_slice(&mut raw);
        if n == 0 {
            // Mikrofon callback'i bizi uyandırır; zaman aşımı sadece durdurma kontrolü için.
            thread::park_timeout(Duration::from_millis(50));
            continue;
        }

        capture.process(&raw[..n], false, |ev| {
            match ev {
                Event::Started { level_db, prob } => {
                    println!("[vad] konuşma başladı (seviye {level_db:.0} dBFS, olasılık {prob:.2})")
                }
                Event::Voice(pkt) => {
                    net.send(tick * FRAME_MS as u64, SimPacket::Voice { seq, data: pkt.to_vec() });
                    seq += 1;
                }
                Event::Ended => {
                    println!("[vad] konuşma bitti");
                    net.send(tick * FRAME_MS as u64, SimPacket::End { seq });
                }
                Event::Tick => {
                    // 20 ms: sahte ağdan al, çöz, çal.
                    for p in net.deliver(tick * FRAME_MS as u64) {
                        match p {
                            SimPacket::Voice { seq, data } => jb.push(seq, data),
                            SimPacket::End { seq } => jb.mark_end(seq),
                        }
                    }
                    tick += 1;
                    let t = Instant::now();
                    match jb.pop() {
                        Playout::Frame(d) => dec.decode(&d, &mut decoded)?,
                        Playout::Conceal { fec: Some(next) } => {
                            ps.fec += 1;
                            dec.decode_fec(&next, &mut decoded)?
                        }
                        Playout::Conceal { fec: None } => {
                            ps.plc += 1;
                            dec.conceal(&mut decoded)?
                        }
                        Playout::Silence => decoded.fill(0.0),
                    }
                    if decoded.iter().any(|&s| s != 0.0) {
                        ps.dec_ns += t.elapsed().as_nanos();
                        ps.decoded += 1;
                    }
                    out_buf.clear();
                    rs_out.process(&decoded, &mut out_buf);
                    play.push_slice(&out_buf);
                }
            }
            Ok(())
        })?;

        if tick >= STATS_EVERY_FRAMES {
            tick -= STATS_EVERY_FRAMES;
            let st = capture.take_stats();
            let p = proc_stats.sample();
            let secs = (STATS_EVERY_FRAMES * FRAME_MS as u64) as f64 / 1000.0;
            let per_pkt = (IP_UDP_OVERHEAD + CLIENT_HEADER_LEN + VOICE_SEQ_LEN + TAG_LEN) as u64;
            let wire = st.payload_bytes + st.frames_sent * per_pkt;
            let avg_us = |ns: u128, n: u64| if n == 0 { 0.0 } else { ns as f64 / n as f64 / 1000.0 };
            println!(
                "[durum] VAD açık %{:>3.0} | mikrofon tepe {:>4.0} dBFS → gönderilen {:>4.0} dBFS (AGC {:+.0} dB) | upload {:>4.1} kbps ({} pkt) | \
                 kayıp {} → FEC {} / PLC {} | jitter {} çerçeve | çalma tamponu {:>3.0} ms | \
                 işlem: rnnoise {:.0}µs enc {:.0}µs dec {:.0}µs | CPU %{:.2} | RAM {:.1} MB (private {:.1}) | \
                 taşma {} boşalma {}",
                st.subframes_open as f64 * 100.0 / st.subframes.max(1) as f64,
                st.peak_db,
                st.sent_level_db,
                st.agc_gain_db,
                wire as f64 * 8.0 / secs / 1000.0,
                st.frames_sent,
                net.dropped,
                ps.fec,
                ps.plc,
                jb.depth(),
                play.occupied_len() as f64 * 1000.0 / out_rate as f64,
                avg_us(st.denoise_ns, st.subframes),
                avg_us(st.enc_ns, st.frames_sent),
                avg_us(ps.dec_ns, ps.decoded),
                p.cpu_pct,
                p.working_set_mb,
                p.private_mb,
                overruns.load(Ordering::Relaxed),
                underruns.load(Ordering::Relaxed),
            );
            net.dropped = 0;
            ps = PlayStats::default();
        }
    }
    Ok(())
}

enum SimPacket {
    Voice { seq: u32, data: Vec<u8> },
    End { seq: u32 },
}

/// Bellekte çalışan sahte ağ: paket kaybı ve rastgele gecikme (sıra karışması dahil) ekler.
struct SimNet {
    loss_pct: f32,
    jitter_ms: u32,
    rng: u64,
    queue: Vec<(u64, SimPacket)>,
    dropped: u64,
}

impl SimNet {
    fn new(loss_pct: f32, jitter_ms: u32) -> Self {
        Self { loss_pct, jitter_ms, rng: 0x9E37_79B9_7F4A_7C15, queue: Vec::new(), dropped: 0 }
    }

    /// xorshift64; [0, 1)
    fn rand(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 40) as f32 / (1u64 << 24) as f32
    }

    fn send(&mut self, now_ms: u64, p: SimPacket) {
        if matches!(p, SimPacket::Voice { .. }) && self.rand() * 100.0 < self.loss_pct {
            self.dropped += 1;
            return;
        }
        let delay = (self.rand() * (self.jitter_ms + 1) as f32) as u64;
        self.queue.push((now_ms + delay, p));
    }

    fn deliver(&mut self, now_ms: u64) -> Vec<SimPacket> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.queue.len() {
            if self.queue[i].0 <= now_ms {
                out.push(self.queue.swap_remove(i).1);
            } else {
                i += 1;
            }
        }
        out
    }
}

//! Ses motoru (MVP.md §4, §6, §7.2): sesli kanaldayken çalışır.
//!
//! Üç thread, hepsi olay tabanlı (busy-loop yok):
//! - **gönderme**: mikrofon callback'i uyandırır → gürültü engelleme/VAD/Opus → şifrele → UDP.
//!   Ayrıca 5 sn'de bir keepalive (NAT eşlemesi + ping ölçümü).
//! - **alma**: UDP → şifre çöz → konuşmacının jitter buffer'ı.
//! - **çalma**: çıkış cihazı callback'i uyandırır → jitter buffer'lardan 20 ms karışım → kulaklık.
//!   Böylece çalma çıkış cihazının saatine göre ilerler.
//!
//! `VoiceEngine` düşürüldüğünde (drop) thread'ler durur ve cihazlar kapanır.

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use corddis_proto::audio::{FRAME_SAMPLES, SAMPLE_RATE};
use corddis_proto::voice::{ClientHeader, PacketType, ReplayWindow, VoiceCipher, KEY_LEN, MAX_PACKET};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use crate::audio::capture::{CaptureProcessor, CaptureSettings, Event};
use crate::audio::device;
use crate::audio::mixer::{Mixer, SpeakerStats};
use crate::audio::resample::Resampler;
use crate::platform;

const KEEPALIVE_EVERY: Duration = Duration::from_secs(5);
/// Çalma tamponunda tutulan ses (çıkış cihazının bir sonraki callback'ine yetecek kadar).
const PLAYBACK_FILL_MS: u32 = 20;
const PLAYBACK_MAX_MS: u32 = 80;
const PLAYBACK_TRIM_TO_MS: u32 = 40;
const NO_RTT: u32 = u32::MAX;

pub struct VoiceConfig {
    pub server: SocketAddr,
    pub session: u32,
    pub key: [u8; KEY_LEN],
    pub input: Option<String>,
    pub output: Option<String>,
    pub capture: CaptureSettings,
}

#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    muted: AtomicBool,
    deafened: AtomicBool,
    self_talking: AtomicBool,
    rtt_ms: AtomicU32,
    tx_packets: AtomicU64,
    rx_packets: AtomicU64,
    rx_rejected: AtomicU64,
    /// cpal callback'leri de yazdığı için Arc.
    underruns: Arc<AtomicU64>,
    overruns: Arc<AtomicU64>,
    mixer: Mutex<Mixer>,
    /// Son yollanan keepalive (paket sayacı, zaman) — ping ölçümü için.
    ping: Mutex<Option<(u32, Instant)>>,
    /// Arayüzden canlı değişen ayarlar (f32 bitleri); `settings_gen` değişince gönderme
    /// thread'i yeniden okur.
    vad_prob: AtomicU32,
    vad_min_db: AtomicU32,
    denoise: AtomicBool,
    settings_gen: AtomicU32,
    /// Son 10 ms'nin mikrofon seviyesi (f32 bitleri, dBFS).
    level_db: AtomicU32,
}

impl Shared {
    fn mixer(&self) -> MutexGuard<'_, Mixer> {
        self.mixer.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[derive(Debug, Clone)]
pub struct VoiceStatus {
    pub rtt_ms: Option<u32>,
    pub self_talking: bool,
    pub speaking: Vec<u16>,
    pub tx_packets: u64,
    pub rx_packets: u64,
    pub rx_rejected: u64,
    pub underruns: u64,
    pub speakers: Vec<(u16, SpeakerStats)>,
}

pub struct VoiceEngine {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
    // cpal akışları; düşürülünce cihazlar kapanır.
    _capture: cpal::Stream,
    _playback: cpal::Stream,
    pub input_name: String,
    pub output_name: String,
}

impl VoiceEngine {
    pub fn start(cfg: VoiceConfig) -> Result<Self> {
        let input = device::open_input(cfg.input.as_deref())?;
        let output = device::open_output(cfg.output.as_deref())?;

        let bind: SocketAddr = if cfg.server.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }.parse().unwrap();
        let sock = UdpSocket::bind(bind).context("UDP soketi açılamadı")?;
        sock.connect(cfg.server).context("UDP hedefi ayarlanamadı")?;
        sock.set_read_timeout(Some(Duration::from_millis(100)))?;

        let shared = Arc::new(Shared {
            rtt_ms: AtomicU32::new(NO_RTT),
            vad_prob: AtomicU32::new(cfg.capture.vad_prob.to_bits()),
            vad_min_db: AtomicU32::new(cfg.capture.vad_min_db.to_bits()),
            denoise: AtomicBool::new(cfg.capture.denoise),
            level_db: AtomicU32::new((-100f32).to_bits()),
            ..Default::default()
        });
        let (cap_prod, cap_cons) = HeapRb::<f32>::new(input.rate() as usize).split();
        let (play_prod, play_cons) = HeapRb::<f32>::new((output.rate() / 2) as usize).split();

        let sender = {
            let (shared, sock, settings) = (shared.clone(), sock.try_clone()?, cfg.capture.clone());
            let (key, session, rate) = (cfg.key, cfg.session, input.rate());
            spawn("corddis-send", move || send_loop(shared, sock, key, session, rate, settings, cap_cons))?
        };
        let receiver = {
            let (shared, sock, key) = (shared.clone(), sock.try_clone()?, cfg.key);
            spawn("corddis-recv", move || recv_loop(shared, sock, key))?
        };
        let player = {
            let (shared, rate) = (shared.clone(), output.rate());
            spawn("corddis-play", move || play_loop(shared, rate, play_prod))?
        };

        let started = (|| {
            let c = device::start_capture(&input, cap_prod, sender.thread().clone(), shared.overruns.clone())?;
            let p = device::start_playback(
                &output,
                play_cons,
                PLAYBACK_MAX_MS,
                PLAYBACK_TRIM_TO_MS,
                shared.underruns.clone(),
                Some(player.thread().clone()),
            )?;
            Ok::<_, anyhow::Error>((c, p))
        })();
        let threads = vec![sender, receiver, player];
        let (capture, playback) = match started {
            Ok(x) => x,
            Err(e) => {
                stop_threads(&shared, threads);
                return Err(e);
            }
        };
        Ok(Self {
            shared,
            threads,
            _capture: capture,
            _playback: playback,
            input_name: input.name,
            output_name: output.name,
        })
    }

    pub fn set_muted(&self, muted: bool) {
        self.shared.muted.store(muted, Ordering::Relaxed);
    }

    pub fn set_deafened(&self, deafened: bool) {
        self.shared.deafened.store(deafened, Ordering::Relaxed);
    }

    pub fn set_volume(&self, user: u16, volume: f32) {
        self.shared.mixer().set_volume(user, volume);
    }

    /// Ses algılama ve gürültü engelleme ayarlarını motoru durdurmadan değiştirir.
    pub fn set_capture(&self, vad_prob: f32, vad_min_db: f32, denoise: bool) {
        let s = &self.shared;
        s.vad_prob.store(vad_prob.to_bits(), Ordering::Relaxed);
        s.vad_min_db.store(vad_min_db.to_bits(), Ordering::Relaxed);
        s.denoise.store(denoise, Ordering::Relaxed);
        s.settings_gen.fetch_add(1, Ordering::Release);
    }

    /// Mikrofonun anlık seviyesi (dBFS); ayarlardaki seviye göstergesi için.
    pub fn level_db(&self) -> f32 {
        f32::from_bits(self.shared.level_db.load(Ordering::Relaxed))
    }

    pub fn status(&self) -> VoiceStatus {
        let s = &self.shared;
        let rtt = s.rtt_ms.load(Ordering::Relaxed);
        let mixer = s.mixer();
        VoiceStatus {
            rtt_ms: (rtt != NO_RTT).then_some(rtt),
            self_talking: s.self_talking.load(Ordering::Relaxed),
            speaking: mixer.speaking(),
            tx_packets: s.tx_packets.load(Ordering::Relaxed),
            rx_packets: s.rx_packets.load(Ordering::Relaxed),
            rx_rejected: s.rx_rejected.load(Ordering::Relaxed),
            underruns: s.underruns.load(Ordering::Relaxed),
            speakers: mixer.stats(),
        }
    }
}

impl Drop for VoiceEngine {
    fn drop(&mut self) {
        stop_threads(&self.shared, std::mem::take(&mut self.threads));
    }
}

fn spawn(name: &str, f: impl FnOnce() -> Result<()> + Send + 'static) -> Result<JoinHandle<()>> {
    let n = name.to_string();
    Ok(thread::Builder::new().name(name.into()).spawn(move || {
        if let Err(e) = f() {
            eprintln!("[ses] {n} durdu: {e:#}");
        }
    })?)
}

fn stop_threads(shared: &Shared, threads: Vec<JoinHandle<()>>) {
    shared.stop.store(true, Ordering::Relaxed);
    for t in &threads {
        t.thread().unpark();
    }
    for t in threads {
        let _ = t.join();
    }
}

fn send_loop(
    shared: Arc<Shared>,
    sock: UdpSocket,
    key: [u8; KEY_LEN],
    session: u32,
    in_rate: u32,
    settings: CaptureSettings,
    mut cap: HeapCons<f32>,
) -> Result<()> {
    platform::boost_audio_thread();
    let cipher = VoiceCipher::new(&key);
    let mut capture = CaptureProcessor::new(in_rate, &settings)?;
    let mut raw = vec![0f32; 4096];
    let mut buf = [0u8; MAX_PACKET];
    let mut payload = Vec::with_capacity(MAX_PACKET);
    let mut counter: u32 = 0;
    let mut voice_seq: u32 = 0;
    let mut last_keepalive: Option<Instant> = None;
    let mut settings_seen = 0u32;

    let mut send = |ty: PacketType, payload: &[u8], counter: &mut u32| {
        let h = ClientHeader { ty, session, seq: *counter };
        *counter += 1;
        if let Ok(n) = cipher.seal_client(&h, payload, &mut buf) {
            // Ağ geçici olarak yoksa hata önemsiz; ses zaten gerçek zamanlı.
            if sock.send(&buf[..n]).is_ok() {
                shared.tx_packets.fetch_add(1, Ordering::Relaxed);
            }
        }
        h.seq
    };

    while !shared.stop.load(Ordering::Relaxed) {
        if last_keepalive.is_none_or(|t| t.elapsed() >= KEEPALIVE_EVERY) {
            let seq = send(PacketType::Keepalive, &[], &mut counter);
            *shared.ping.lock().unwrap_or_else(|p| p.into_inner()) = Some((seq, Instant::now()));
            last_keepalive = Some(Instant::now());
        }

        let n = cap.pop_slice(&mut raw);
        if n == 0 {
            thread::park_timeout(Duration::from_millis(50));
            continue;
        }
        let gen = shared.settings_gen.load(Ordering::Acquire);
        if gen != settings_seen {
            settings_seen = gen;
            capture.set_vad(
                f32::from_bits(shared.vad_prob.load(Ordering::Relaxed)),
                f32::from_bits(shared.vad_min_db.load(Ordering::Relaxed)),
            );
            capture.set_denoise(shared.denoise.load(Ordering::Relaxed));
        }
        let muted = shared.muted.load(Ordering::Relaxed) || shared.deafened.load(Ordering::Relaxed);
        capture.process(&raw[..n], muted, |ev| {
            match ev {
                Event::Voice(opus) => {
                    payload.clear();
                    payload.extend_from_slice(&voice_seq.to_be_bytes());
                    payload.extend_from_slice(opus);
                    send(PacketType::Voice, &payload, &mut counter);
                    voice_seq = voice_seq.wrapping_add(1);
                }
                Event::Ended => {
                    send(PacketType::EndOfSpeech, &voice_seq.to_be_bytes(), &mut counter);
                }
                Event::Started { .. } | Event::Tick => {}
            }
            Ok(())
        })?;
        shared.self_talking.store(capture.talking(), Ordering::Relaxed);
        shared.level_db.store(capture.level_db().to_bits(), Ordering::Relaxed);
    }
    Ok(())
}

fn recv_loop(shared: Arc<Shared>, sock: UdpSocket, key: [u8; KEY_LEN]) -> Result<()> {
    let cipher = VoiceCipher::new(&key);
    let mut replay = ReplayWindow::default();
    let mut buf = [0u8; MAX_PACKET];
    while !shared.stop.load(Ordering::Relaxed) {
        let n = match sock.recv(&mut buf) {
            Ok(n) => n,
            // Zaman aşımı (durdurma kontrolü için) veya Windows'ta ICMP "ulaşılamaz" bildirimi.
            Err(_) => continue,
        };
        let Ok((h, body)) = cipher.open_server(&mut buf[..n]) else {
            shared.rx_rejected.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        if !replay.accept(h.counter) {
            shared.rx_rejected.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        shared.rx_packets.fetch_add(1, Ordering::Relaxed);
        match h.ty {
            PacketType::Keepalive => {
                let ping = *shared.ping.lock().unwrap_or_else(|p| p.into_inner());
                if let Some((seq, at)) = ping {
                    if seq == h.seq {
                        shared.rtt_ms.store(at.elapsed().as_millis() as u32, Ordering::Relaxed);
                    }
                }
            }
            PacketType::Voice => {
                if !shared.deafened.load(Ordering::Relaxed) && !body.is_empty() {
                    shared.mixer().push(h.speaker, h.seq, body.to_vec())?;
                }
            }
            PacketType::EndOfSpeech => shared.mixer().end(h.speaker, h.seq),
        }
    }
    Ok(())
}

fn play_loop(shared: Arc<Shared>, out_rate: u32, mut play: HeapProd<f32>) -> Result<()> {
    platform::boost_audio_thread();
    let fill = (out_rate * PLAYBACK_FILL_MS / 1000) as usize;
    let mut rs = Resampler::new(SAMPLE_RATE, out_rate);
    let mut frame = [0f32; FRAME_SAMPLES];
    let mut out = Vec::with_capacity(4096);
    while !shared.stop.load(Ordering::Relaxed) {
        while play.occupied_len() < fill {
            let any = shared.mixer().mix(&mut frame)?;
            if !any || shared.deafened.load(Ordering::Relaxed) {
                frame.fill(0.0);
            }
            out.clear();
            rs.process(&frame, &mut out);
            if play.push_slice(&out) < out.len() {
                break;
            }
        }
        // Çıkış callback'i bizi her ~10 ms'de uyandırır.
        thread::park_timeout(Duration::from_millis(50));
    }
    Ok(())
}

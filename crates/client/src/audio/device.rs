//! WASAPI cihazları (cpal üzerinden).
//!
//! Callback'ler bilerek çok basit tutulur: yalnızca mono'ya çevirip halka tampona yazar /
//! halka tampondan okur. Örnekleme hızı dönüşümü ve tüm işlem ayrı bir thread'de yapılır.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::Thread;

use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, SampleFormat, SizedSample, Stream, StreamConfig};
use ringbuf::traits::{Consumer, Observer, Producer};
use ringbuf::{HeapCons, HeapProd};

pub fn list_devices() -> Result<()> {
    let host = cpal::default_host();
    let def_in = host.default_input_device().and_then(|d| d.name().ok());
    let def_out = host.default_output_device().and_then(|d| d.name().ok());

    println!("Mikrofonlar:");
    for d in host.input_devices()? {
        let name = d.name().unwrap_or_else(|_| "?".into());
        let cfg = d.default_input_config().map(|c| describe(&c)).unwrap_or_default();
        let mark = if Some(&name) == def_in.as_ref() { "*" } else { " " };
        println!(" {mark} {name}  [{cfg}]");
    }
    println!("\nÇıkış cihazları:");
    for d in host.output_devices()? {
        let name = d.name().unwrap_or_else(|_| "?".into());
        let cfg = d.default_output_config().map(|c| describe(&c)).unwrap_or_default();
        let mark = if Some(&name) == def_out.as_ref() { "*" } else { " " };
        println!(" {mark} {name}  [{cfg}]");
    }
    println!("\n* = sistem varsayılanı");
    Ok(())
}

fn describe(c: &cpal::SupportedStreamConfig) -> String {
    format!("{} Hz, {} kanal, {:?}", c.sample_rate().0, c.channels(), c.sample_format())
}

fn find(input: bool, name: Option<&str>) -> Result<Device> {
    let host = cpal::default_host();
    match name {
        None => if input { host.default_input_device() } else { host.default_output_device() }
            .ok_or_else(|| anyhow!("varsayılan {} cihazı yok", if input { "giriş" } else { "çıkış" })),
        Some(want) => {
            let want = want.to_lowercase();
            let mut devs: Box<dyn Iterator<Item = Device>> =
                if input { Box::new(host.input_devices()?) } else { Box::new(host.output_devices()?) };
            devs.find(|d| d.name().map(|n| n.to_lowercase().contains(&want)).unwrap_or(false))
                .ok_or_else(|| anyhow!("'{want}' adında cihaz bulunamadı (`corddis devices` ile listele)"))
        }
    }
}

/// Seçilmiş ama henüz başlatılmamış bir cihaz.
pub struct Opened {
    device: Device,
    config: StreamConfig,
    format: SampleFormat,
    pub name: String,
}

impl Opened {
    pub fn rate(&self) -> u32 {
        self.config.sample_rate.0
    }
    pub fn channels(&self) -> u16 {
        self.config.channels
    }
}

pub fn open_input(name: Option<&str>) -> Result<Opened> {
    let device = find(true, name)?;
    let sup = device.default_input_config().context("mikrofon ayarı okunamadı")?;
    Ok(Opened {
        name: device.name().unwrap_or_default(),
        format: sup.sample_format(),
        config: sup.config(),
        device,
    })
}

pub fn open_output(name: Option<&str>) -> Result<Opened> {
    let device = find(false, name)?;
    let sup = device.default_output_config().context("çıkış ayarı okunamadı")?;
    Ok(Opened {
        name: device.name().unwrap_or_default(),
        format: sup.sample_format(),
        config: sup.config(),
        device,
    })
}

fn err_fn(e: cpal::StreamError) {
    eprintln!("[ses] cihaz hatası: {e}");
}

/// Mikrofonu başlatır. Her callback'te mono örnekler `prod`'a yazılır ve `wake` uyandırılır.
pub fn start_capture(dev: &Opened, prod: HeapProd<f32>, wake: Thread, overruns: Arc<AtomicU64>) -> Result<Stream> {
    let stream = match dev.format {
        SampleFormat::F32 => build_input::<f32>(dev, prod, wake, overruns),
        SampleFormat::I16 => build_input::<i16>(dev, prod, wake, overruns),
        SampleFormat::I32 => build_input::<i32>(dev, prod, wake, overruns),
        SampleFormat::U16 => build_input::<u16>(dev, prod, wake, overruns),
        f => bail!("desteklenmeyen mikrofon formatı: {f:?}"),
    }?;
    stream.play().context("mikrofon başlatılamadı")?;
    Ok(stream)
}

fn build_input<T>(dev: &Opened, mut prod: HeapProd<f32>, wake: Thread, overruns: Arc<AtomicU64>) -> Result<Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let ch = dev.channels() as usize;
    let mut mono: Vec<f32> = Vec::with_capacity(8192);
    let stream = dev.device.build_input_stream(
        &dev.config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            mono.clear();
            for frame in data.chunks_exact(ch) {
                let sum: f32 = frame.iter().map(|&s| <f32 as FromSample<T>>::from_sample_(s)).sum();
                mono.push(sum / ch as f32);
            }
            if prod.push_slice(&mono) < mono.len() {
                overruns.fetch_add(1, Ordering::Relaxed);
            }
            wake.unpark();
        },
        err_fn,
        None,
    )?;
    Ok(stream)
}

/// Çıkışı başlatır. `cons`'tan cihaz hızında mono örnek okur, tüm kanallara kopyalar.
/// Tampon `max_ms`'yi aşarsa (iki cihazın saatleri farklı ilerlediğinde birikme olur)
/// `trim_to_ms`'ye kadar eski örnekler atılır; böylece gecikme zamanla büyümez.
/// `wake` verilirse her callback'te uyandırılır: çalma tamponunu dolduran thread
/// çıkış cihazının saatine göre çalışır.
pub fn start_playback(
    dev: &Opened,
    cons: HeapCons<f32>,
    max_ms: u32,
    trim_to_ms: u32,
    underruns: Arc<AtomicU64>,
    wake: Option<Thread>,
) -> Result<Stream> {
    let max = (dev.rate() * max_ms / 1000) as usize;
    let trim_to = (dev.rate() * trim_to_ms / 1000) as usize;
    let stream = match dev.format {
        SampleFormat::F32 => build_output::<f32>(dev, cons, max, trim_to, underruns, wake),
        SampleFormat::I16 => build_output::<i16>(dev, cons, max, trim_to, underruns, wake),
        SampleFormat::I32 => build_output::<i32>(dev, cons, max, trim_to, underruns, wake),
        SampleFormat::U16 => build_output::<u16>(dev, cons, max, trim_to, underruns, wake),
        f => bail!("desteklenmeyen çıkış formatı: {f:?}"),
    }?;
    stream.play().context("çıkış başlatılamadı")?;
    Ok(stream)
}

fn build_output<T>(
    dev: &Opened,
    mut cons: HeapCons<f32>,
    max: usize,
    trim_to: usize,
    underruns: Arc<AtomicU64>,
    wake: Option<Thread>,
) -> Result<Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let ch = dev.channels() as usize;
    let stream = dev.device.build_output_stream(
        &dev.config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            let occ = cons.occupied_len();
            if occ > max {
                cons.skip(occ - trim_to);
            }
            let mut starved = false;
            for frame in data.chunks_exact_mut(ch) {
                let s = cons.try_pop().unwrap_or_else(|| {
                    starved = true;
                    0.0
                });
                let v = <T as FromSample<f32>>::from_sample_(s);
                frame.iter_mut().for_each(|x| *x = v);
            }
            if starved {
                underruns.fetch_add(1, Ordering::Relaxed);
            }
            if let Some(w) = &wake {
                w.unpark();
            }
        },
        err_fn,
        None,
    )?;
    Ok(stream)
}

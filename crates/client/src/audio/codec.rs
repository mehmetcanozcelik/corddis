//! Opus kodlayıcı/çözücü (MVP.md §6 parametreleri).

use anyhow::{Context, Result};
use corddis_proto::audio::{FRAME_SAMPLES, MAX_OPUS_PACKET, SAMPLE_RATE};
use opus::{Application, Bitrate, Channels};

pub struct Encoder {
    inner: opus::Encoder,
    buf: [u8; MAX_OPUS_PACKET],
}

impl Encoder {
    pub fn new(bitrate: i32) -> Result<Self> {
        let mut e = opus::Encoder::new(SAMPLE_RATE, Channels::Mono, Application::Voip)
            .context("opus encoder")?;
        e.set_bitrate(Bitrate::Bits(bitrate))?;
        e.set_vbr(true)?;
        e.set_inband_fec(true)?;
        e.set_packet_loss_perc(5)?;
        e.set_complexity(5)?;
        e.set_signal(opus::Signal::Voice)?;
        Ok(Self { inner: e, buf: [0; MAX_OPUS_PACKET] })
    }

    /// 960 örneklik (20 ms) çerçeveyi kodlar.
    pub fn encode(&mut self, frame: &[f32]) -> Result<&[u8]> {
        debug_assert_eq!(frame.len(), FRAME_SAMPLES);
        let n = self.inner.encode_float(frame, &mut self.buf)?;
        Ok(&self.buf[..n])
    }
}

pub struct Decoder {
    inner: opus::Decoder,
}

impl Decoder {
    pub fn new() -> Result<Self> {
        Ok(Self { inner: opus::Decoder::new(SAMPLE_RATE, Channels::Mono).context("opus decoder")? })
    }

    pub fn decode(&mut self, packet: &[u8], out: &mut [f32; FRAME_SAMPLES]) -> Result<()> {
        self.inner.decode_float(packet, out, false)?;
        Ok(())
    }

    /// Kayıp çerçeveyi, bir sonraki paketin içindeki FEC verisinden kurtarır.
    pub fn decode_fec(&mut self, next_packet: &[u8], out: &mut [f32; FRAME_SAMPLES]) -> Result<()> {
        self.inner.decode_float(next_packet, out, true)?;
        Ok(())
    }

    /// Veri yok: Opus'un kayıp gizleme (PLC) algoritmasıyla tahmini ses üretir.
    pub fn conceal(&mut self, out: &mut [f32; FRAME_SAMPLES]) -> Result<()> {
        self.inner.decode_float(&[], out, false)?;
        Ok(())
    }
}

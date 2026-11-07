//! Konuşmacı başına jitter buffer (MVP.md §6).
//!
//! Ağdan düzensiz aralıklarla (ve bazen sırası karışık) gelen paketleri toplar,
//! her 20 ms'de bir çerçeve verir. Eksik çerçevede, varsa bir sonraki paketin FEC verisini
//! önerir; yoksa çağıran Opus PLC (kayıp gizleme) kullanır.
//!
//! Hedef derinliği çağıran ayarlar (`audio::mixer` ağdaki oynamaya göre uyarlar).

use std::collections::BTreeMap;

/// Üst üste bu kadar çerçeve eksikse konuşmacı durmuş kabul edilir (200 ms).
const MAX_MISSES: u32 = 10;
/// Bellek sınırı: 1 saniyelik paket.
const MAX_PACKETS: usize = 50;

#[derive(Debug, PartialEq, Eq)]
pub enum Playout {
    /// Normal çerçeve: çöz ve çal.
    Frame(Vec<u8>),
    /// Çerçeve eksik. `fec` varsa bir sonraki paketten FEC ile kurtar, yoksa PLC.
    Conceal { fec: Option<Vec<u8>> },
    /// Kimse konuşmuyor; çözücüyü hiç çağırma.
    Silence,
}

pub struct JitterBuffer {
    packets: BTreeMap<u32, Vec<u8>>,
    /// Sıradaki çalınacak seq. Bundan küçük gelen paketler geç kalmıştır ve atılır.
    next: u32,
    started_once: bool,
    playing: bool,
    target: usize,
    end_at: Option<u32>,
    misses: u32,
    pub late_drops: u64,
}

impl JitterBuffer {
    /// `target`: çalmaya başlamadan önce biriktirilecek çerçeve sayısı (2 = 40 ms).
    pub fn new(target: usize) -> Self {
        Self {
            packets: BTreeMap::new(),
            next: 0,
            started_once: false,
            playing: false,
            target: target.max(1),
            end_at: None,
            misses: 0,
            late_drops: 0,
        }
    }

    pub fn depth(&self) -> usize {
        self.packets.len()
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Hedef derinliği değiştirir. Yalnızca bir sonraki konuşmanın başında etkili olur
    /// (konuşma ortasında değiştirmek sesi bozar).
    pub fn set_target(&mut self, target: usize) {
        self.target = target.max(1);
    }

    pub fn push(&mut self, seq: u32, data: Vec<u8>) {
        if self.started_once && seq < self.next {
            self.late_drops += 1;
            return;
        }
        if self.packets.len() >= MAX_PACKETS {
            self.packets.pop_first();
        }
        self.packets.insert(seq, data);
    }

    /// Gönderen konuşmayı bitirdi; `seq` son ses paketinden sonraki numara.
    pub fn mark_end(&mut self, seq: u32) {
        if !self.started_once || seq >= self.next {
            self.end_at = Some(seq);
        }
    }

    /// Her 20 ms'de bir çağrılır.
    pub fn pop(&mut self) -> Playout {
        if !self.playing {
            let short_utterance = self.end_at.is_some() && !self.packets.is_empty();
            if self.packets.len() < self.target && !short_utterance {
                return Playout::Silence;
            }
            let (&first, _) = self.packets.first_key_value().unwrap();
            self.next = first;
            self.started_once = true;
            self.playing = true;
            self.misses = 0;
        }

        if let Some(end) = self.end_at {
            if self.next >= end {
                self.stop(end);
                return Playout::Silence;
            }
        }

        if let Some(p) = self.packets.remove(&self.next) {
            self.next += 1;
            self.misses = 0;
            return Playout::Frame(p);
        }

        self.misses += 1;
        if self.misses > MAX_MISSES {
            let at = self.next;
            self.stop(at);
            return Playout::Silence;
        }
        let fec = self.packets.get(&(self.next + 1)).cloned();
        self.next += 1;
        Playout::Conceal { fec }
    }

    fn stop(&mut self, at: u32) {
        self.playing = false;
        self.end_at = None;
        self.next = self.next.max(at);
        let next = self.next;
        self.packets.retain(|&s, _| s >= next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(n: u8) -> Vec<u8> {
        vec![n]
    }

    #[test]
    fn waits_for_target_then_plays_in_order() {
        let mut j = JitterBuffer::new(2);
        j.push(10, p(10));
        assert_eq!(j.pop(), Playout::Silence);
        j.push(11, p(11));
        assert_eq!(j.pop(), Playout::Frame(p(10)));
        assert_eq!(j.pop(), Playout::Frame(p(11)));
    }

    #[test]
    fn reorders() {
        let mut j = JitterBuffer::new(2);
        j.push(2, p(2));
        j.push(1, p(1));
        assert_eq!(j.pop(), Playout::Frame(p(1)));
        assert_eq!(j.pop(), Playout::Frame(p(2)));
    }

    #[test]
    fn missing_frame_offers_fec_from_next() {
        let mut j = JitterBuffer::new(2);
        j.push(1, p(1));
        j.push(3, p(3));
        assert_eq!(j.pop(), Playout::Frame(p(1)));
        assert_eq!(j.pop(), Playout::Conceal { fec: Some(p(3)) });
        assert_eq!(j.pop(), Playout::Frame(p(3)));
    }

    #[test]
    fn late_packet_dropped() {
        let mut j = JitterBuffer::new(1);
        j.push(5, p(5));
        assert_eq!(j.pop(), Playout::Frame(p(5)));
        j.push(4, p(4));
        assert_eq!(j.late_drops, 1);
    }

    #[test]
    fn end_marker_stops_without_concealment() {
        let mut j = JitterBuffer::new(2);
        j.push(1, p(1));
        j.push(2, p(2));
        j.mark_end(3);
        assert_eq!(j.pop(), Playout::Frame(p(1)));
        assert_eq!(j.pop(), Playout::Frame(p(2)));
        assert_eq!(j.pop(), Playout::Silence);
        // Yeni konuşma devam eden seq ile başlar.
        j.push(3, p(3));
        j.push(4, p(4));
        assert_eq!(j.pop(), Playout::Frame(p(3)));
    }

    #[test]
    fn short_utterance_below_target_still_plays() {
        let mut j = JitterBuffer::new(3);
        j.push(1, p(1));
        j.mark_end(2);
        assert_eq!(j.pop(), Playout::Frame(p(1)));
        assert_eq!(j.pop(), Playout::Silence);
    }

    #[test]
    fn gives_up_after_too_many_misses() {
        let mut j = JitterBuffer::new(1);
        j.push(1, p(1));
        assert_eq!(j.pop(), Playout::Frame(p(1)));
        for _ in 0..MAX_MISSES {
            assert_eq!(j.pop(), Playout::Conceal { fec: None });
        }
        assert_eq!(j.pop(), Playout::Silence);
        assert_eq!(j.pop(), Playout::Silence);
    }
}

//! Global mikrofon kısayolu (MVP.md §8): oyun odaktayken de çalışır.
//!
//! `RegisterHotKey` kullanılır; klavye hook'u yoktur, bu yüzden anti-cheat açısından risksizdir.
//! Kısayol kendi thread'inin mesaj kuyruğuna gelir.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

pub const LABEL: &str = "Ctrl+Shift+M";

pub struct Hotkey {
    presses: Arc<AtomicU32>,
    seen: u32,
    pub registered: bool,
}

impl Hotkey {
    /// Kısayolu kaydeder; her basışta `on_press` çağrılır (arayüzü uyandırmak için).
    pub fn start(on_press: impl Fn() + Send + 'static) -> Self {
        let presses = Arc::new(AtomicU32::new(0));
        let registered = start_thread(presses.clone(), on_press);
        Self { presses, seen: 0, registered }
    }

    /// Son çağrıdan beri kaç kez basıldı.
    pub fn take(&mut self) -> u32 {
        let now = self.presses.load(Ordering::Relaxed);
        let n = now.wrapping_sub(self.seen);
        self.seen = now;
        n
    }
}

#[cfg(windows)]
fn start_thread(presses: Arc<AtomicU32>, on_press: impl Fn() + Send + 'static) -> bool {
    use std::sync::mpsc;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};

    let (ok_tx, ok_rx) = mpsc::channel();
    let spawned = std::thread::Builder::new().name("corddis-hotkey".into()).spawn(move || {
        let ok = unsafe { RegisterHotKey(std::ptr::null_mut(), 1, MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT, b'M' as u32) } != 0;
        let _ = ok_tx.send(ok);
        if !ok {
            return;
        }
        let mut msg: MSG = unsafe { std::mem::zeroed() };
        // Uygulama kapanınca süreçle birlikte biter.
        while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
            if msg.message == WM_HOTKEY {
                presses.fetch_add(1, Ordering::Relaxed);
                on_press();
            }
        }
    });
    spawned.is_ok() && ok_rx.recv().unwrap_or(false)
}

#[cfg(not(windows))]
fn start_thread(_: Arc<AtomicU32>, _: impl Fn() + Send + 'static) -> bool {
    false
}

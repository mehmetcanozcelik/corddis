//! Sistem tepsisi ikonu: pencere kapatılınca uygulama tepside çalışmaya devam eder
//! (sesli kanaldan düşmez). Sol tık: pencereyi göster. Sağ tık: menü.

use std::sync::{mpsc, Arc, Mutex};

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Show,
    ToggleMute,
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
    mute_item: MenuItem,
    rx: mpsc::Receiver<TrayAction>,
}

/// Kare RGBA görüntüyü `size`×`size`'a kutu filtresiyle küçültür (tepsi ikonu 32 px).
pub fn downscale(rgba: &[u8], src: u32, size: u32) -> Vec<u8> {
    let f = (src / size).max(1);
    let mut out = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let mut acc = [0u32; 4];
            for dy in 0..f {
                for dx in 0..f {
                    let i = (((y * f + dy) * src + (x * f + dx)) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += rgba[i + c] as u32;
                    }
                }
            }
            let o = ((y * size + x) * 4) as usize;
            for c in 0..4 {
                out[o + c] = (acc[c] / (f * f)) as u8;
            }
        }
    }
    out
}

impl Tray {
    /// `wake`: bir tepsi olayı olunca arayüzü uyandırmak için.
    pub fn new(rgba: Vec<u8>, size: u32, wake: impl Fn() + Send + Sync + 'static) -> Option<Self> {
        let show = MenuItem::new("Corddis'i göster", true, None);
        let mute = MenuItem::new("Mikrofonu kapat", true, None);
        let quit = MenuItem::new("Çıkış", true, None);
        let menu = Menu::new();
        menu.append_items(&[&show, &mute, &PredefinedMenuItem::separator(), &quit]).ok()?;

        let (tx, rx) = mpsc::channel();
        let tx = Arc::new(Mutex::new(tx));
        let wake = Arc::new(wake);
        let send = {
            let (tx, wake) = (tx.clone(), wake.clone());
            move |a: TrayAction| {
                if let Ok(t) = tx.lock() {
                    let _ = t.send(a);
                }
                wake();
            }
        };
        let ids = (show.id().clone(), mute.id().clone(), quit.id().clone());
        let s = send.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let a = if e.id == ids.0 {
                TrayAction::Show
            } else if e.id == ids.1 {
                TrayAction::ToggleMute
            } else if e.id == ids.2 {
                TrayAction::Quit
            } else {
                return;
            };
            s(a);
        }));
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                send(TrayAction::Show);
            }
        }));

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip("Corddis")
            .with_icon(Icon::from_rgba(rgba, size, size).ok()?)
            .build()
            .ok()?;
        Some(Self { icon, mute_item: mute, rx })
    }

    pub fn poll(&self) -> Vec<TrayAction> {
        self.rx.try_iter().collect()
    }

    pub fn set_muted(&self, muted: bool) {
        self.mute_item.set_text(if muted { "Mikrofonu aç" } else { "Mikrofonu kapat" });
        let _ = self.icon.set_tooltip(Some(if muted { "Corddis — mikrofon kapalı" } else { "Corddis" }));
    }
}

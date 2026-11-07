//! Arayüz (MVP.md §9): olabildiğince basit, tek pencere.
//!
//! - İlk açılış: davet bağlantısını yapıştır → bağlan (bir daha sorulmaz).
//! - Sol: yazı ve ses kanalları, sesli kanaldakiler (konuşan yeşil), altta mikrofon/kulaklık/ayarlar.
//! - Orta: seçili yazı kanalının mesajları, altta mesaj kutusu.
//!
//! Performans (MVP.md §8): yalnızca olay olunca çizer; sesli kanaldayken konuşan göstergesi için
//! saniyede ~5 kare, pencere küçültülmüşken hiç çizmez.

use std::collections::HashMap;
use std::net::ToSocketAddrs;
use std::time::Duration;

use corddis_proto::control::{ChannelId, ClientMsg, UserId};
use corddis_proto::invite::Invite;
use eframe::egui::{self, Color32, Key, RichText, Sense, Stroke, TextEdit, Vec2};

use crate::audio::device;
use crate::audio::sounds::{self, Sound};
use crate::config::Config;
use crate::hotkey::{self, Hotkey};
use crate::model::{Conn, Model, HISTORY_PAGE};
use crate::netloop::NetHandle;
use crate::tray::{self, Tray, TrayAction};
use crate::voice::{VoiceConfig, VoiceEngine, VoiceStatus};

const GREEN: Color32 = Color32::from_rgb(67, 181, 129);
const RED: Color32 = Color32::from_rgb(237, 66, 69);
const YELLOW: Color32 = Color32::from_rgb(250, 168, 26);
const GRAY: Color32 = Color32::from_rgb(128, 132, 142);
const NAME_COLORS: [Color32; 6] = [
    Color32::from_rgb(88, 166, 255),
    Color32::from_rgb(255, 140, 105),
    Color32::from_rgb(120, 220, 160),
    Color32::from_rgb(230, 160, 255),
    Color32::from_rgb(255, 210, 90),
    Color32::from_rgb(110, 210, 230),
];
/// Aynı kişinin bu kadar saniye içindeki ardışık mesajları tek başlık altında gösterilir.
const GROUP_SECS: i64 = 300;
const METER_MIN_DB: f32 = -70.0;

/// Pencere, tepsi ve exe ikonu (eframe'in varsayılan ikonu, MIT/Apache-2.0).
const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");
const TRAY_ICON_SIZE: u32 = 32;

pub fn run() -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Corddis")
        .with_inner_size([980.0, 640.0])
        .with_min_inner_size([680.0, 420.0]);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(ICON_PNG) {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("Corddis", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

fn make_tray(ctx: &egui::Context) -> Option<Tray> {
    let icon = eframe::icon_data::from_png_bytes(ICON_PNG).ok()?;
    let rgba = tray::downscale(&icon.rgba, icon.width, TRAY_ICON_SIZE);
    let c = ctx.clone();
    Tray::new(rgba, TRAY_ICON_SIZE, move || c.request_repaint())
}

/// egui'nin yerleşik fontunda simge/emoji yok; Windows'un simge fontunu yedek olarak ekle
/// (●, 🔊, 🎤, 🎧, ⚙, 🔇). Bulunamazsa simgeler kutu görünür ama uygulama çalışır.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    if let Ok(bytes) = std::fs::read(format!("{windir}\\Fonts\\seguisym.ttf")) {
        fonts.font_data.insert("segoe-symbol".into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(fam).or_default().push("segoe-symbol".into());
        }
    }
    ctx.set_fonts(fonts);
}

enum Screen {
    Invite { text: String, error: Option<String> },
    Main,
}

struct DeviceLists {
    inputs: Vec<String>,
    outputs: Vec<String>,
}

struct App {
    ctx: egui::Context,
    cfg: Config,
    screen: Screen,
    host: String,
    net: Option<NetHandle>,
    engine: Option<VoiceEngine>,
    engine_error: Option<String>,
    /// Ses başlatma bu kanal için başarısız oldu; kullanıcı yeniden katılana dek tekrar deneme.
    engine_blocked: bool,
    draft: String,
    muted: bool,
    deafened: bool,
    settings: Option<DeviceLists>,
    hotkey: Hotkey,
    tray: Option<Tray>,
    /// Tepsiden "Çıkış" seçildi: pencere kapatma isteği artık tepsiye indirmez, kapatır.
    quitting: bool,
    /// Bildirim sesi için: bir önceki karede bulunduğum sesli kanal ve oradakiler.
    last_voice: Option<(ChannelId, Vec<UserId>)>,
}

/// Çizim sırasında toplanan, çizimden sonra uygulanan kullanıcı eylemleri.
enum Action {
    SelectText(ChannelId),
    JoinVoice(ChannelId),
    LeaveVoice,
    ToggleMute,
    ToggleDeafen,
    OpenSettings,
    Send(ChannelId, String),
    LoadOlder(ChannelId, i64),
    SetVolume(UserId, String, f32),
    ClearError,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = cc.egui_ctx.clone();
        ctx.set_visuals(egui::Visuals::dark());
        install_fonts(&ctx);
        let hk_ctx = ctx.clone();
        let mut app = Self {
            hotkey: Hotkey::start(move || hk_ctx.request_repaint()),
            tray: make_tray(&ctx),
            quitting: false,
            last_voice: None,
            ctx,
            cfg: Config::load(),
            screen: Screen::Invite { text: String::new(), error: None },
            host: String::new(),
            net: None,
            engine: None,
            engine_error: None,
            engine_blocked: false,
            draft: String::new(),
            muted: false,
            deafened: false,
            settings: None,
        };
        if let Some(inv) = app.cfg.invite().and_then(|s| s.parse::<Invite>().ok()) {
            app.connect(inv);
        }
        app
    }

    fn connect(&mut self, inv: Invite) {
        let c = self.ctx.clone();
        self.host = inv.host.clone();
        self.net = Some(NetHandle::start(inv, move || c.request_repaint()));
        self.screen = Screen::Main;
    }

    fn logout(&mut self) {
        self.engine = None;
        self.net = None;
        self.settings = None;
        self.cfg.set_invite(None);
        self.cfg.save();
        self.screen = Screen::Invite { text: String::new(), error: None };
    }

    fn send(&self, m: ClientMsg) {
        if let Some(n) = &self.net {
            n.send(m);
        }
    }

    fn sound(&self, s: Sound) {
        if self.cfg.sounds {
            sounds::play(s, self.cfg.output.clone());
        }
    }

    fn push_state(&mut self) {
        let off = self.muted || self.deafened;
        self.sound(if off { Sound::Mute } else { Sound::Unmute });
        if let Some(t) = &self.tray {
            t.set_muted(off);
        }
        if let Some(e) = &self.engine {
            e.set_muted(self.muted);
            e.set_deafened(self.deafened);
        }
        if let Some(n) = &self.net {
            let mut m = n.model();
            m.desired.muted = self.muted;
            m.desired.deafened = self.deafened;
        }
        self.send(ClientMsg::SetState { muted: self.muted, deafened: self.deafened });
    }

    fn toggle_mute(&mut self) {
        if self.deafened {
            // Sağırken mikrofonu açmak sağırlığı da kaldırır.
            self.deafened = false;
            self.muted = false;
        } else {
            self.muted = !self.muted;
        }
        self.push_state();
    }

    /// Sunucu bizi bir sesli kanala koyduysa ses motorunu başlatır, çıkardıysa durdurur.
    fn sync_engine(&mut self, m: &Model) {
        match (m.my_voice(), self.engine.is_some()) {
            (Some(_), false) if !self.engine_blocked => {
                let Some(udp) = &m.udp else { return };
                let server = (self.host.as_str(), udp.port).to_socket_addrs().ok().and_then(|mut a| a.next());
                let Some(server) = server else {
                    self.engine_error = Some("Sunucu adresi çözülemedi".into());
                    self.engine_blocked = true;
                    return;
                };
                let a = self.cfg.audio_options();
                let cfg = VoiceConfig {
                    server,
                    session: udp.session,
                    key: udp.key,
                    input: a.input,
                    output: a.output,
                    capture: a.capture,
                };
                match VoiceEngine::start(cfg) {
                    Ok(e) => {
                        e.set_muted(self.muted);
                        e.set_deafened(self.deafened);
                        for (id, u) in &m.users {
                            if let Some(&v) = self.cfg.volumes.get(&u.name) {
                                e.set_volume(*id, v);
                            }
                        }
                        self.engine = Some(e);
                        self.engine_error = None;
                    }
                    Err(e) => {
                        self.engine_error = Some(format!("Ses başlatılamadı: {e:#}"));
                        self.engine_blocked = true;
                    }
                }
            }
            (None, true) => self.engine = None,
            (None, false) => self.engine_blocked = false,
            _ => {}
        }
    }
}

impl App {
    fn show_window(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// Kanaldaki değişikliklere göre giriş/çıkış sesi çalar.
    fn voice_sounds(&mut self, m: &Model) {
        let now = m.my_voice().map(|c| (c, m.voice.get(&c).cloned().unwrap_or_default()));
        let sound = match (&self.last_voice, &now) {
            (None, Some(_)) => Some(Sound::Join),
            (Some(_), None) => Some(Sound::Leave),
            (Some((c0, before)), Some((c1, after))) if c0 == c1 => {
                if after.iter().any(|u| !before.contains(u)) {
                    Some(Sound::Join)
                } else if before.iter().any(|u| !after.contains(u)) {
                    Some(Sound::Leave)
                } else {
                    None
                }
            }
            (Some(_), Some(_)) => Some(Sound::Join), // başka kanala geçti
            (None, None) => None,
        };
        if let Some(s) = sound {
            self.sound(s);
        }
        self.last_voice = now;
    }
}

impl eframe::App for App {
    /// Pencere tepsideyken (gizliyken) de çalışır: kısayol, tepsi menüsü, ses motoru, bildirim sesleri.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.hotkey.take() % 2 == 1 {
            self.toggle_mute();
        }
        let actions = self.tray.as_ref().map(|t| t.poll()).unwrap_or_default();
        for a in actions {
            match a {
                TrayAction::Show => self.show_window(ctx),
                TrayAction::ToggleMute => self.toggle_mute(),
                TrayAction::Quit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        // Kapatma düğmesi: tepsi varsa uygulamayı kapatma, tepsiye indir (sesli kanaldan düşme).
        if ctx.input(|i| i.viewport().close_requested()) && self.tray.is_some() && !self.quitting {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        if let Some(model) = self.net.as_ref().map(|n| n.model.clone()) {
            let m = model.lock().unwrap_or_else(|p| p.into_inner());
            if m.conn != Conn::BadInvite {
                self.sync_engine(&m);
                self.voice_sounds(&m);
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if matches!(self.screen, Screen::Invite { .. }) {
            self.invite_screen(ui);
        } else {
            self.main_screen(ui);
        }
    }
}

// ---------------------------------------------------------------- davet ekranı

impl App {
    fn invite_screen(&mut self, ui: &mut egui::Ui) {
        let Screen::Invite { text, error } = &mut self.screen else { return };
        let mut submit = false;
        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() * 0.28).max(20.0));
                ui.label(RichText::new("Corddis").size(34.0).strong());
                ui.add_space(14.0);
                ui.label("Arkadaşından aldığın davet bağlantısını yapıştır:");
                ui.add_space(8.0);
                let r = ui.add(TextEdit::singleline(text).hint_text("corddis://...").desired_width(480.0));
                edit_menu(&r);
                submit |= r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                ui.add_space(10.0);
                submit |= ui.add(egui::Button::new("Bağlan").min_size(Vec2::new(120.0, 30.0))).clicked();
                if let Some(e) = error {
                    ui.add_space(8.0);
                    ui.colored_label(RED, e.as_str());
                }
                ui.add_space(16.0);
                ui.label(RichText::new("Bağlantı bu bilgisayarda şifreli saklanır, bir daha sorulmaz.").small().color(GRAY));
            });
        });
        if submit {
            let t = text.trim().to_string();
            match t.parse::<Invite>() {
                Ok(inv) => {
                    self.cfg.set_invite(Some(&t));
                    self.cfg.save();
                    self.connect(inv);
                }
                Err(e) => *error = Some(e.to_string()),
            }
        }
    }
}

// ---------------------------------------------------------------- ana ekran

impl App {
    fn main_screen(&mut self, ui: &mut egui::Ui) {
        let Some(net) = &self.net else { return };
        let model = net.model.clone();
        let m = model.lock().unwrap_or_else(|p| p.into_inner());

        if m.conn == Conn::BadInvite {
            drop(m);
            let mut again = false;
            egui::CentralPanel::default().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.3);
                    ui.label(RichText::new("Davet bağlantısı artık geçerli değil").size(20.0).color(RED));
                    ui.add_space(8.0);
                    ui.label("Yenilenmiş ya da silinmiş olabilir. Sunucuyu yöneten kişiden yeni bir davet iste.");
                    ui.add_space(12.0);
                    again = ui.button("Yeni davet gir").clicked();
                });
            });
            if again {
                self.logout();
            }
            return;
        }

        let status = self.engine.as_ref().map(|e| e.status());
        let mut actions = Vec::new();
        let ctx = ui.ctx().clone();

        egui::Panel::left("kanallar").exact_size(250.0).resizable(false).show(ui, |ui| {
            egui::Panel::bottom("ben").show(ui, |ui| {
                me_panel(ui, &m, status.as_ref(), self.muted, self.deafened, self.engine_error.as_deref(), &mut actions);
            });
            egui::Panel::bottom("not").show_separator_line(false).show(ui, |ui| {
                ui.add_space(2.0);
                ui.label(RichText::new("Yusuf'un g*tüne girsin.").small().italics().color(GRAY));
            });
            channel_list(ui, &m, status.as_ref(), self.muted, &self.cfg.volumes, &mut actions);
        });

        let viewing = m.viewing.filter(|c| m.text_channels().any(|t| t.id == *c));
        if let Some(ch) = viewing {
            let name = m.channels.iter().find(|c| c.id == ch).map(|c| c.name.clone()).unwrap_or_default();
            let draft = &mut self.draft;
            egui::Panel::bottom("yaz").show(ui, |ui| {
                ui.add_space(8.0);
                if let Some(err) = &m.last_error {
                    ui.horizontal(|ui| {
                        ui.colored_label(RED, err.as_str());
                        if ui.small_button("✕").clicked() {
                            actions.push(Action::ClearError);
                        }
                    });
                }
                let online = m.conn == Conn::Online;
                let hint = if online { format!("#{name} kanalına yaz") } else { "Bağlantı bekleniyor…".into() };
                let r = ui.add_enabled(
                    online,
                    TextEdit::singleline(draft).hint_text(hint).desired_width(f32::INFINITY).char_limit(2000),
                );
                edit_menu(&r);
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    let text = draft.trim().to_string();
                    if !text.is_empty() {
                        actions.push(Action::Send(ch, text));
                        draft.clear();
                    }
                    r.request_focus();
                }
                ui.add_space(8.0);
            });
        }

        egui::CentralPanel::default().show(ui, |ui| match viewing {
            Some(ch) => chat_view(ui, &m, ch, &mut actions),
            None => {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new(conn_text(&m.conn)).color(GRAY));
                });
            }
        });

        // Model kilidini bırak, eylemleri uygula.
        drop(m);
        for a in actions {
            self.apply(a);
        }
        self.settings_window(&ctx);

        // Gösterge için düşük hızda yenile; küçültülmüşken hiç çizme.
        let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        if !minimized && self.engine.is_some() {
            let every = if self.settings.is_some() { 50 } else { 200 };
            ctx.request_repaint_after(Duration::from_millis(every));
        }
    }

    fn apply(&mut self, a: Action) {
        let Some(net) = &self.net else { return };
        match a {
            Action::SelectText(ch) => net.model().set_viewing(Some(ch)),
            Action::JoinVoice(ch) => {
                net.model().desired.voice = Some(ch);
                self.engine_blocked = false;
                net.send(ClientMsg::JoinVoice { channel_id: ch });
            }
            Action::LeaveVoice => {
                net.model().desired.voice = None;
                net.send(ClientMsg::LeaveVoice);
            }
            Action::ToggleMute => self.toggle_mute(),
            Action::ToggleDeafen => {
                self.deafened = !self.deafened;
                self.push_state();
            }
            Action::OpenSettings => {
                let (inputs, outputs) = device::names();
                self.settings = Some(DeviceLists { inputs, outputs });
            }
            Action::Send(ch, text) => net.send(ClientMsg::SendMsg { channel_id: ch, text }),
            Action::LoadOlder(ch, before) => {
                net.send(ClientMsg::History { channel_id: ch, before_id: Some(before), limit: HISTORY_PAGE })
            }
            Action::SetVolume(id, name, v) => {
                if let Some(e) = &self.engine {
                    e.set_volume(id, v);
                }
                self.cfg.volumes.insert(name, v);
                self.cfg.save();
            }
            Action::ClearError => net.model().last_error = None,
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let Some(lists) = &self.settings else { return };
        let mut open = true;
        let (mut device_changed, mut capture_changed, mut logout) = (false, false, false);
        let level = self.engine.as_ref().map(|e| e.level_db());
        let cfg = &mut self.cfg;
        let hotkey_ok = self.hotkey.registered;

        egui::Window::new("Ayarlar")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(400.0)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(RichText::new("Ses cihazları").strong());
                device_changed |= device_combo(ui, "Mikrofon", &mut cfg.input, &lists.inputs);
                device_changed |= device_combo(ui, "Çıkış", &mut cfg.output, &lists.outputs);

                ui.separator();
                ui.label(RichText::new("Ses algılama").strong());
                ui.label(
                    RichText::new(
                        "Mikrofon hep açık; yalnızca konuşurken ses gider. Konuşurken çubuk sarı çizgiyi geçmeli, \
                         sessizken geçmemeli.",
                    )
                    .small(),
                );
                match level {
                    Some(l) => level_meter(ui, l, cfg.vad_min_db),
                    None => {
                        ui.label(RichText::new("Seviye göstergesi için bir sesli kanala katıl.").small().color(GRAY));
                    }
                }
                capture_changed |= ui
                    .add(egui::Slider::new(&mut cfg.vad_min_db, METER_MIN_DB..=-20.0).text("eşik (dB)").step_by(1.0))
                    .changed();
                capture_changed |= ui.checkbox(&mut cfg.denoise, "Gürültü engelleme (klavye, fan, oda sesi)").changed();
                capture_changed |= ui.checkbox(&mut cfg.sounds, "Bildirim sesleri (giriş, çıkış, mikrofon)").changed();
                capture_changed |= ui
                    .checkbox(&mut cfg.agc, "Otomatik ses seviyesi (kısık mikrofonu yükseltir)")
                    .on_hover_text("Seni karşı tarafa hep aynı yükseklikte duyurur. Kapatırsan Windows mikrofon seviyesi aynen gider.")
                    .changed();

                ui.separator();
                ui.label(RichText::new("Kısayol").strong());
                if hotkey_ok {
                    ui.label(format!("Mikrofonu aç/kapat: {} — oyun içindeyken de çalışır.", hotkey::LABEL));
                } else {
                    ui.colored_label(
                        YELLOW,
                        format!("{} kaydedilemedi; başka bir uygulama kullanıyor olabilir.", hotkey::LABEL),
                    );
                }

                ui.separator();
                logout = ui.button("Davet bağlantısını unut (çıkış yap)").clicked();
            });

        if device_changed {
            cfg.save();
            // Sonraki karede yeni cihazlarla yeniden başlar.
            self.engine = None;
            self.engine_blocked = false;
        }
        if capture_changed {
            cfg.save();
            if let Some(e) = &self.engine {
                e.set_capture(cfg.vad_prob, cfg.vad_min_db, cfg.denoise, cfg.agc);
            }
        }
        if !open {
            self.settings = None;
        }
        if logout {
            self.logout();
        }
    }
}

// ---------------------------------------------------------------- parçalar

fn conn_text(c: &Conn) -> String {
    match c {
        Conn::Connecting => "Bağlanıyor…".into(),
        Conn::Online => "Bağlı".into(),
        Conn::Offline { .. } => "Bağlantı yok — yeniden deneniyor".into(),
        Conn::BadInvite => "Davet geçersiz".into(),
    }
}

fn name_color(id: UserId) -> Color32 {
    NAME_COLORS[id as usize % NAME_COLORS.len()]
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(title).small().strong().color(GRAY));
}

fn channel_list(
    ui: &mut egui::Ui,
    m: &Model,
    status: Option<&VoiceStatus>,
    muted: bool,
    volumes: &HashMap<String, f32>,
    actions: &mut Vec<Action>,
) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Corddis").size(20.0).strong());
        let (color, tip) = match &m.conn {
            Conn::Online => (GREEN, "Bağlı".to_string()),
            Conn::Connecting => (YELLOW, "Bağlanıyor…".to_string()),
            Conn::Offline { reason } => (RED, format!("Bağlantı yok, yeniden deneniyor.\n{reason}")),
            Conn::BadInvite => (RED, "Davet geçersiz".to_string()),
        };
        ui.label(RichText::new("●").color(color)).on_hover_text(tip);
    });
    if let Conn::Offline { .. } = m.conn {
        ui.label(RichText::new("Bağlantı yok — yeniden deneniyor").small().color(RED));
    }

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        section(ui, "YAZI KANALLARI");
        for c in m.text_channels() {
            let unread = m.unread.get(&c.id).copied().unwrap_or(0);
            let mut text = RichText::new(format!("#  {}", c.name));
            if unread > 0 {
                text = RichText::new(format!("#  {}   ({unread})", c.name)).strong();
            }
            if ui.selectable_label(m.viewing == Some(c.id), text).clicked() {
                actions.push(Action::SelectText(c.id));
            }
        }

        section(ui, "SES KANALLARI");
        let mine = m.my_voice();
        for c in m.voice_channels() {
            let here = mine == Some(c.id);
            let r = ui
                .selectable_label(here, RichText::new(format!("🔊  {}", c.name)))
                .on_hover_text(if here { "Bu kanaldasın" } else { "Katılmak için tıkla" });
            if r.clicked() && !here {
                actions.push(Action::JoinVoice(c.id));
            }
            for &uid in m.voice.get(&c.id).into_iter().flatten() {
                let Some(u) = m.users.get(&uid) else { continue };
                let me = m.me == Some(uid);
                let speaking = status.is_some_and(|s| {
                    if me {
                        s.self_talking && !muted
                    } else {
                        s.speaking.contains(&uid)
                    }
                });
                ui.horizontal(|ui| {
                    ui.add_space(22.0);
                    ui.label(RichText::new("●").color(if speaking { GREEN } else { Color32::from_gray(70) }));
                    let mut name = RichText::new(&u.name);
                    if me {
                        name = name.strong();
                    }
                    if speaking {
                        name = name.color(GREEN);
                    }
                    let r = ui.add(egui::Label::new(name).sense(Sense::click()));
                    if u.deafened {
                        ui.label(RichText::new("🎧").color(RED)).on_hover_text("Sağır");
                    } else if u.muted {
                        ui.label(RichText::new("🔇").color(RED)).on_hover_text("Mikrofonu kapalı");
                    }
                    if !me {
                        let r = r.on_hover_text("Sağ tık: ses düzeyi");
                        r.context_menu(|ui| {
                            let mut pct = volumes.get(&u.name).copied().unwrap_or(1.0) * 100.0;
                            ui.label(format!("{} ses düzeyi", u.name));
                            if ui.add(egui::Slider::new(&mut pct, 0.0..=crate::audio::mixer::MAX_VOLUME * 100.0).suffix("%").step_by(5.0)).changed() {
                                actions.push(Action::SetVolume(uid, u.name.clone(), pct / 100.0));
                            }
                        });
                    }
                });
            }
        }
    });
}

fn me_panel(
    ui: &mut egui::Ui,
    m: &Model,
    status: Option<&VoiceStatus>,
    muted: bool,
    deafened: bool,
    engine_error: Option<&str>,
    actions: &mut Vec<Action>,
) {
    ui.add_space(6.0);
    if let Some(e) = engine_error {
        ui.label(RichText::new(e).small().color(RED));
    }
    if let Some(ch) = m.my_voice() {
        let name = m.channels.iter().find(|c| c.id == ch).map(|c| c.name.as_str()).unwrap_or("?");
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Sesli bağlı").small().strong().color(GREEN));
                let ping = status.and_then(|s| s.rtt_ms).map(|r| format!("{name} · {r} ms")).unwrap_or(name.to_string());
                ui.label(RichText::new(ping).small().color(GRAY));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Ayrıl").on_hover_text("Sesli kanaldan çık").clicked() {
                    actions.push(Action::LeaveVoice);
                }
            });
        });
        ui.separator();
    }
    ui.horizontal(|ui| {
        let me = m.me.map(|id| m.name(id).to_string()).unwrap_or_default();
        ui.label(RichText::new(me).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let big = |s: &str, on: bool| {
                egui::Button::new(RichText::new(s).size(16.0).color(if on { RED } else { Color32::LIGHT_GRAY }))
                    .min_size(Vec2::new(34.0, 30.0))
            };
            if ui.add(big("⚙", false)).on_hover_text("Ayarlar").clicked() {
                actions.push(Action::OpenSettings);
            }
            let tip = if deafened { "Sağırlığı kapat" } else { "Sağır ol: kimseyi duyma, konuşma" };
            if ui.add(big("🎧", deafened)).on_hover_text(tip).clicked() {
                actions.push(Action::ToggleDeafen);
            }
            let off = muted || deafened;
            let tip = format!("{} ({})", if off { "Mikrofonu aç" } else { "Mikrofonu kapat" }, hotkey::LABEL);
            if ui.add(big(if off { "🔇" } else { "🎤" }, off)).on_hover_text(tip).clicked() {
                actions.push(Action::ToggleMute);
            }
        });
    });
    ui.add_space(6.0);
}

fn chat_view(ui: &mut egui::Ui, m: &Model, ch: ChannelId, actions: &mut Vec<Action>) {
    let name = m.channels.iter().find(|c| c.id == ch).map(|c| c.name.as_str()).unwrap_or("?");
    ui.add_space(4.0);
    ui.label(RichText::new(format!("#  {name}")).size(18.0).strong());
    ui.separator();

    egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
        let msgs = m.messages.get(&ch).map(Vec::as_slice).unwrap_or(&[]);
        if !m.history_done.contains(&ch) {
            if let Some(oldest) = m.oldest_message(ch) {
                if ui.small_button("Daha eski mesajları yükle").clicked() {
                    actions.push(Action::LoadOlder(ch, oldest));
                }
            }
        }
        if msgs.is_empty() {
            ui.add_space(20.0);
            ui.label(RichText::new("Henüz mesaj yok. İlk mesajı sen yaz!").color(GRAY));
        }
        let mut prev: Option<(UserId, i64)> = None;
        for msg in msgs {
            let grouped = prev.is_some_and(|(u, t)| u == msg.user_id && msg.ts - t < GROUP_SECS);
            if !grouped {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(m.name(msg.user_id)).strong().color(name_color(msg.user_id)));
                    ui.label(RichText::new(clock(msg.ts)).small().color(GRAY));
                });
            }
            message_text(ui, &msg.text);
            prev = Some((msg.user_id, msg.ts));
        }
        ui.add_space(4.0);
    });
}

/// Türkiye yıl boyu UTC+3 (yaz saati yok).
fn clock(ts: i64) -> String {
    let t = ts + 3 * 3600;
    format!("{:02}:{:02}", (t / 3600).rem_euclid(24), (t / 60).rem_euclid(60))
}

fn message_text(ui: &mut egui::Ui, text: &str) {
    if !text.contains("http://") && !text.contains("https://") {
        ui.add(egui::Label::new(text).wrap());
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for word in text.split_whitespace() {
            if word.starts_with("http://") || word.starts_with("https://") {
                ui.hyperlink(word);
            } else {
                ui.label(word);
            }
        }
    });
}

/// Metin kutusuna sağ tık menüsü (egui'nin kendi menüsü yok). Komutlar odaktaki kutuya gider,
/// bu yüzden önce kutuya odaklanılır.
fn edit_menu(r: &egui::Response) {
    r.context_menu(|ui| {
        for (label, cmd) in [
            ("Yapıştır", egui::ViewportCommand::RequestPaste),
            ("Kopyala", egui::ViewportCommand::RequestCopy),
            ("Kes", egui::ViewportCommand::RequestCut),
        ] {
            if ui.button(label).clicked() {
                r.request_focus();
                ui.ctx().send_viewport_cmd(cmd);
                ui.close();
            }
        }
    });
}

fn device_combo(ui: &mut egui::Ui, label: &str, value: &mut Option<String>, list: &[String]) -> bool {
    let before = value.clone();
    egui::ComboBox::from_label(label)
        .selected_text(value.as_deref().unwrap_or("Sistem varsayılanı"))
        .width(280.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(value, None, "Sistem varsayılanı");
            for d in list {
                ui.selectable_value(value, Some(d.clone()), d);
            }
        });
    *value != before
}

/// Mikrofon seviyesi çubuğu + eşik çizgisi.
fn level_meter(ui: &mut egui::Ui, level_db: f32, threshold_db: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 12.0), Sense::hover());
    let frac = |db: f32| ((db - METER_MIN_DB) / -METER_MIN_DB).clamp(0.0, 1.0);
    let p = ui.painter();
    p.rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);
    let mut fill = rect;
    fill.set_width(rect.width() * frac(level_db));
    p.rect_filled(fill, 3.0, if level_db >= threshold_db { GREEN } else { GRAY });
    let x = rect.left() + rect.width() * frac(threshold_db);
    p.vline(x, rect.y_range(), Stroke::new(2.0, YELLOW));
}

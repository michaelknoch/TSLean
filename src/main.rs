mod audio;
mod config;
mod emoji;
mod net;
mod theme;

use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Duration;

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Rect, RichText, Sense, Stroke, Vec2};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tokio::sync::mpsc;
use tsclientlib::{ChannelId, Identity};

use audio::{Audio, Controls};
use config::{Appearance, Config, VoiceMode};
use net::{ChatTab, Cmd, Login, Row, Shared, SpacerAlign, State, Status};
use theme::p;

const SIDEBAR_W: f32 = 264.0;
const TITLEBAR_H: f32 = 52.0;
const ROW_H: f32 = 28.0;
const PAD: f32 = 20.0;
const INDENT: f32 = 14.0;

struct App {
    cfg: Config,
    shared: Arc<Shared>,
    cmd: mpsc::UnboundedSender<Cmd>,
    net: Option<std::thread::JoinHandle<()>>,
    controls: Arc<Controls>,
    mic_tx: mpsc::Sender<net::Mic>,
    _audio: Option<Audio>,
    audio_error: Option<String>,
    audio_lost: Arc<AtomicBool>,
    hotkeys: Option<GlobalHotKeyManager>,
    hotkey: Option<HotKey>,
    hotkey_error: Option<String>,
    settings_open: bool,
    recording: bool,
    tab: ChatTab,
    draft: String,
    join_password: Option<(ChannelId, String)>,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        theme::install(&cc.egui_ctx);
        let cfg = Config::load();
        cc.egui_ctx.set_theme(cfg.appearance);
        let controls = Arc::new(Controls::new(cfg.voice_mode == VoiceMode::PushToTalk, cfg.vad_threshold_db));
        let shared = Arc::new(Shared::default());
        let (mic_tx, mic_rx) = mpsc::channel(32);
        let (cmd, net) = net::spawn(shared.clone(), cc.egui_ctx.clone(), controls.clone(), mic_rx);

        let ptt = controls.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
            ptt.ptt_down.store(e.state == HotKeyState::Pressed, Relaxed);
        }));

        let mut app = Self {
            cfg,
            shared,
            cmd,
            net: Some(net),
            controls,
            mic_tx,
            _audio: None,
            audio_error: None,
            audio_lost: Arc::new(AtomicBool::new(false)),
            hotkeys: GlobalHotKeyManager::new().ok(),
            hotkey: None,
            hotkey_error: None,
            settings_open: false,
            recording: false,
            tab: ChatTab::Channel,
            draft: String::new(),
            join_password: None,
        };
        app.start_audio(&cc.egui_ctx);
        app.apply_hotkey();
        app
    }

    fn start_audio(&mut self, ctx: &egui::Context) {
        self._audio = None;
        let (producer, consumer) = rtrb::RingBuffer::new(512);
        let lost = self.audio_lost.clone();
        let ctx = ctx.clone();
        let on_lost = move || {
            lost.store(true, Relaxed);
            ctx.request_repaint();
        };
        match Audio::start(self.controls.clone(), self.mic_tx.clone(), consumer, on_lost) {
            Ok(a) => {
                self._audio = Some(a);
                self.audio_error = None;
                let _ = self.cmd.send(Cmd::Playback(producer));
            }
            Err(e) => self.audio_error = Some(e.to_string()),
        }
    }

    fn apply_hotkey(&mut self) {
        let Some(mgr) = &self.hotkeys else { return };
        if let Some(old) = self.hotkey.take() {
            let _ = mgr.unregister(old);
        }
        self.hotkey_error = None;
        if self.cfg.voice_mode != VoiceMode::PushToTalk || self.recording {
            return;
        }
        match HotKey::from_str(&self.cfg.ptt_hotkey)
            .map_err(|e| e.to_string())
            .and_then(|hk| mgr.register(hk).map(|_| hk).map_err(|e| e.to_string()))
        {
            Ok(hk) => self.hotkey = Some(hk),
            Err(e) => self.hotkey_error = Some(e),
        }
    }

    fn connect(&mut self) {
        let identity = self.cfg.identity.get_or_insert_with(Identity::create).clone();
        self.cfg.save();
        let _ = self.cmd.send(Cmd::Connect(Login {
            address: self.cfg.address.trim().to_string(),
            nickname: self.cfg.nickname.trim().to_string(),
            password: self.cfg.server_password.clone(),
            identity,
            input_muted: self.cfg.input_muted,
            output_muted: self.cfg.output_muted,
            volumes: self.cfg.volumes.clone(),
        }));
        self.tab = ChatTab::Channel;
    }

    fn set_muted(&mut self, input: bool, output: bool) {
        self.cfg.input_muted = input;
        self.cfg.output_muted = output;
        self.cfg.save();
        let _ = self.cmd.send(Cmd::SetMuted { input, output });
    }

    fn connect_view(&mut self, ctx: &egui::Context, s: &State) {
        egui::CentralPanel::default().frame(egui::Frame::new().fill(p().content)).show(ctx, |ui| {
            drag_region(ui, ui.max_rect().width());
            let size = Vec2::new(320.0, 420.0);
            let rect = Rect::from_center_size(ui.max_rect().center(), size);
            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                theme::caption(ui, "TSLean");
                ui.add_space(6.0);
                ui.label(RichText::new("Connect to a server").size(26.0));
                ui.add_space(32.0);

                theme::caption(ui, "Server");
                theme::field(ui, &mut self.cfg.address, "voice.example.com", false);
                ui.add_space(20.0);
                theme::caption(ui, "Nickname");
                theme::field(ui, &mut self.cfg.nickname, "Your name", false);
                ui.add_space(20.0);
                theme::caption(ui, "Password");
                theme::field(ui, &mut self.cfg.server_password, "Optional", true);
                ui.add_space(32.0);

                let connecting = s.status == Status::Connecting;
                let ready = !connecting && !self.cfg.address.trim().is_empty() && !self.cfg.nickname.trim().is_empty();
                let label = if connecting { "Connecting…" } else { "Connect" };
                if theme::primary_button(ui, label, ready).clicked()
                    || (ready && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                {
                    self.connect();
                }
                if let Some(e) = &s.error {
                    ui.add_space(14.0);
                    ui.label(RichText::new(e).size(12.0).color(p().accent));
                }
            });
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, s: &State) {
        drag_region(ui, SIDEBAR_W);
        egui::TopBottomPanel::top("server")
            .frame(egui::Frame::new().inner_margin(egui::Margin { left: PAD as i8, right: 12, top: 0, bottom: 14 }))
            .show_separator_line(false)
            .show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        emoji::label(ui, &s.server_name, FontId::proportional(15.0), p().text);
                        let sub =
                            if s.status == Status::Connecting { "Reconnecting…" } else { self.cfg.address.as_str() };
                        ui.label(RichText::new(sub).size(11.5).color(p().secondary));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::icon_button(ui, "Disconnect", theme::leave).clicked() {
                            let _ = self.cmd.send(Cmd::Disconnect);
                        }
                    });
                });
            });

        egui::TopBottomPanel::bottom("me")
            .frame(egui::Frame::new().inner_margin(egui::Margin { left: PAD as i8, right: 12, top: 10, bottom: 12 }))
            .show_inside(ui, |ui| self.me_bar(ui, s));

        egui::CentralPanel::default().frame(egui::Frame::new()).show_inside(ui, |ui| self.tree(ui, s));
    }

    fn me_bar(&mut self, ui: &mut egui::Ui, s: &State) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                emoji::label(ui, &self.cfg.nickname, FontId::proportional(13.5), p().text);
                let mode = match self.cfg.voice_mode {
                    VoiceMode::Activation => "Voice activation".to_string(),
                    VoiceMode::PushToTalk => format!("Push to talk · {}", pretty_hotkey(&self.cfg.ptt_hotkey)),
                };
                ui.label(RichText::new(mode).size(11.0).color(p().secondary));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if theme::icon_button(ui, "Settings", theme::sliders).clicked() {
                    self.settings_open = true;
                }
                let (input, output) = (s.input_muted, s.output_muted);
                let speaker =
                    theme::icon_button(ui, if output { "Unmute sound" } else { "Mute sound" }, |pt, c, col| {
                        theme::speaker(pt, c, if output { p().accent } else { col });
                        if output {
                            theme::slash(pt, c, p().accent);
                        }
                    });
                if speaker.clicked() {
                    self.set_muted(input, !output);
                }
                let mic = theme::icon_button(
                    ui,
                    if input { "Unmute microphone" } else { "Mute microphone" },
                    |pt, c, col| {
                        theme::mic(pt, c, if input { p().accent } else { col });
                        if input {
                            theme::slash(pt, c, p().accent);
                        }
                    },
                );
                if mic.clicked() {
                    self.set_muted(!input, output);
                }
            });
        });
    }

    fn tree(&mut self, ui: &mut egui::Ui, s: &State) {
        let rows = &s.tree;
        egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, ROW_H, rows.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for row in &rows[range] {
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
                let painter = ui.painter();
                let wash = rect.shrink2(Vec2::new(8.0, 1.0));
                match row {
                    Row::Channel { name, spacer: Some(align), .. } => {
                        let y = rect.center().y;
                        let line = caps_line(ui, name, 10.0);
                        match align {
                            SpacerAlign::Rule => {
                                painter.hline(
                                    rect.left() + PAD..=rect.right() - PAD,
                                    y,
                                    Stroke::new(1.0f32, p().hairline),
                                );
                            }
                            SpacerAlign::Left => {
                                line.paint(painter, egui::pos2(rect.left() + PAD, y), p().faint);
                            }
                            SpacerAlign::Center | SpacerAlign::Right => {
                                let x = if *align == SpacerAlign::Center {
                                    rect.center().x - line.size.x / 2.0
                                } else {
                                    rect.right() - PAD - line.size.x
                                };
                                line.paint(painter, egui::pos2(x, y), p().faint);
                            }
                        }
                    }
                    Row::Channel { id, name, depth, locked, topic, .. } => {
                        let own = s.own_channel == Some(*id);
                        let hovered = resp.hovered() && !own;
                        if hovered {
                            painter.rect_filled(wash, CornerRadius::same(6), p().hover);
                        }
                        let color = if own || hovered { p().text } else { p().secondary };
                        let line = caps_line(ui, name, 10.5);
                        let x = rect.left() + PAD + *depth as f32 * INDENT;
                        line.paint(painter, egui::pos2(x, rect.bottom() - 6.0 - line.size.y / 2.0), color);
                        if *locked {
                            theme::lock(painter, egui::pos2(rect.right() - PAD - 2.0, rect.bottom() - 12.0), p().faint);
                        }
                        let resp = if topic.is_empty() { resp } else { resp.on_hover_text(topic) };
                        if resp.clicked() && !own {
                            if *locked {
                                self.join_password = Some((*id, String::new()));
                            } else {
                                let _ = self.cmd.send(Cmd::Join(*id, None));
                            }
                        }
                        resp.on_hover_cursor(if own {
                            egui::CursorIcon::Default
                        } else {
                            egui::CursorIcon::PointingHand
                        });
                    }
                    Row::Client { id, uid, label, depth, muted, away } => {
                        let is_self = s.own_id == Some(*id);
                        let talking = s.talking.contains(id);
                        if resp.hovered() && !is_self {
                            painter.rect_filled(wash, CornerRadius::same(6), p().hover);
                        }
                        let x = rect.left() + PAD + (*depth - 1) as f32 * INDENT;
                        let dot = egui::pos2(x + 3.0, rect.center().y);
                        let t = ui.ctx().animate_bool_with_time(ui.id().with(id), talking, 0.12);
                        if t > 0.0 {
                            painter.circle_filled(dot, 3.5 + 4.0 * t, p().accent.gamma_multiply(0.16 * t));
                        }
                        painter.circle_filled(dot, 3.0, lerp(p().hairline, p().accent, t));
                        let color = if *away { p().faint } else { p().text };
                        let line = emoji::line(ui, label, FontId::proportional(13.5));
                        let name_w = line.size.x;
                        line.paint(painter, egui::pos2(x + 14.0, rect.center().y), color);
                        if is_self {
                            painter.text(
                                egui::pos2(x + 20.0 + name_w, rect.center().y),
                                Align2::LEFT_CENTER,
                                "you",
                                FontId::proportional(11.0),
                                p().faint,
                            );
                        }
                        let mut right = rect.right() - PAD;
                        if *muted {
                            let c = egui::pos2(right - 4.0, rect.center().y);
                            theme::mic(painter, c, p().faint);
                            theme::slash(painter, c, p().faint);
                            right -= 20.0;
                        }
                        let volume = uid
                            .as_ref()
                            .filter(|_| !self.cfg.volumes.is_empty())
                            .and_then(|u| self.cfg.volumes.get(&u.to_string()));
                        if let Some(v) = volume {
                            let pos = egui::pos2(right, rect.center().y);
                            let text = format!("{:.0}%", v * 100.0);
                            painter.text(pos, Align2::RIGHT_CENTER, text, FontId::proportional(11.0), p().faint);
                        }
                        if let (Some(uid), false) = (uid, is_self) {
                            resp.context_menu(|ui| self.volume_menu(ui, uid, label));
                        }
                        if resp.clicked() && !is_self {
                            if let Some(uid) = uid {
                                let title = label.split("  ·").next().unwrap_or(label).to_string();
                                let _ = self.cmd.send(Cmd::OpenDm(uid.clone(), title));
                                self.tab = ChatTab::Private(uid.clone());
                            }
                        }
                    }
                }
            }
        });
    }

    fn volume_menu(&mut self, ui: &mut egui::Ui, uid: &tsclientlib::UidBuf, name: &str) {
        let key = uid.to_string();
        let mut v = self.cfg.volumes.get(&key).copied().unwrap_or(1.0);
        ui.set_width(220.0);
        ui.add_space(4.0);
        emoji::label(ui, name.split("  ·").next().unwrap_or(name), FontId::proportional(13.5), p().text);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            theme::caption(ui, "Volume");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(format!("{:.0}%", v * 100.0)).size(11.0).color(p().secondary));
            });
        });
        ui.spacing_mut().slider_width = ui.available_width();
        let slider = ui.add(egui::Slider::new(&mut v, 0.0..=2.0).show_value(false));
        ui.add_space(4.0);
        let reset = ui.button(RichText::new("Reset").size(12.0).color(p().secondary)).clicked();
        if reset {
            v = 1.0;
        }
        if slider.changed() || reset {
            if (v - 1.0).abs() < 0.01 {
                self.cfg.volumes.remove(&key);
            } else {
                self.cfg.volumes.insert(key, v);
            }
            let _ = self.cmd.send(Cmd::Volume(uid.clone(), v));
        }
        if slider.drag_stopped() || reset || (slider.changed() && !slider.dragged()) {
            self.cfg.save();
        }
    }

    fn conversation(&mut self, ui: &mut egui::Ui, s: &State) {
        let tabs: Vec<(ChatTab, String)> =
            [(ChatTab::Channel, s.own_channel_name.clone()), (ChatTab::Server, s.server_name.clone())]
                .into_iter()
                .chain(
                    s.chats
                        .iter()
                        .filter(|c| matches!(c.tab, ChatTab::Private(_)))
                        .map(|c| (c.tab.clone(), c.title.clone())),
                )
                .collect();
        if !tabs.iter().any(|(t, _)| *t == self.tab) {
            self.tab = ChatTab::Channel;
        }
        if let ChatTab::Private(uid) = &self.tab {
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::W)) {
                let _ = self.cmd.send(Cmd::CloseDm(uid.clone()));
                self.tab = ChatTab::Channel;
            }
        }

        let bar = drag_region(ui, ui.max_rect().width());
        let mut x = bar.left() + PAD + 8.0;
        for (i, (tab, title)) in tabs.iter().enumerate() {
            let active = *tab == self.tab;
            let color = if active { p().text } else { p().faint };
            let line = emoji::line(ui, title, FontId::proportional(13.5));
            let r = Rect::from_min_size(egui::pos2(x, bar.bottom() - 30.0), Vec2::new(line.size.x, 30.0));
            let resp = ui.interact(r, ui.id().with(("tab", i)), Sense::click());
            let color = if resp.hovered() { p().text } else { color };
            line.paint(ui.painter(), egui::pos2(x, r.top() + 4.0 + line.size.y / 2.0), color);
            if active {
                ui.painter().hline(r.x_range(), bar.bottom() - 0.5, Stroke::new(1.5f32, p().text));
            }
            if resp.clicked() {
                self.tab = tab.clone();
            }
            if resp.middle_clicked() {
                if let ChatTab::Private(uid) = tab {
                    let _ = self.cmd.send(Cmd::CloseDm(uid.clone()));
                }
            }
            x += r.width() + 22.0;
        }
        ui.painter().hline(ui.max_rect().x_range(), bar.bottom(), Stroke::new(1.0f32, p().hairline));

        let title = tabs.iter().find(|(t, _)| *t == self.tab).map(|(_, n)| n.clone()).unwrap_or_default();
        egui::TopBottomPanel::bottom("composer")
            .frame(egui::Frame::new().inner_margin(egui::Margin {
                left: (PAD + 8.0) as i8,
                right: (PAD + 8.0) as i8,
                top: 14,
                bottom: 18,
            }))
            .show_separator_line(true)
            .show_inside(ui, |ui| {
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.draft)
                        .frame(false)
                        .font(FontId::proportional(14.0))
                        .hint_text(RichText::new(format!("Message {title}")).color(p().faint))
                        .desired_width(f32::INFINITY),
                );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let text = self.draft.trim();
                    if !text.is_empty() {
                        let _ = self.cmd.send(Cmd::Chat(self.tab.clone(), text.to_string()));
                    }
                    self.draft.clear();
                    resp.request_focus();
                }
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(egui::Margin {
                left: (PAD + 8.0) as i8,
                right: (PAD + 8.0) as i8,
                top: 18,
                bottom: 8,
            }))
            .show_inside(ui, |ui| self.messages(ui, s));
    }

    fn messages(&self, ui: &mut egui::Ui, s: &State) {
        let Some(chat) = s.chats.iter().find(|c| c.tab == self.tab).filter(|c| !c.lines.is_empty()) else {
            ui.centered_and_justified(|ui| ui.label(RichText::new("Nothing here yet").color(p().faint)));
            return;
        };
        egui::ScrollArea::vertical().auto_shrink(false).stick_to_bottom(true).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            let mut last_from: Option<&str> = None;
            for line in chat.lines.iter() {
                match &line.from {
                    Some(from) => {
                        if last_from != Some(from.as_str()) {
                            ui.add_space(if last_from.is_some() { 12.0 } else { 6.0 });
                            ui.horizontal(|ui| {
                                emoji::label(ui, from, FontId::proportional(13.0), p().text);
                                ui.label(RichText::new(&line.time).size(11.0).color(p().faint));
                            });
                        }
                        emoji::paragraph(ui, &line.text, FontId::proportional(14.0), p().text);
                        last_from = Some(from);
                    }
                    None => {
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&line.time).size(11.0).color(p().faint));
                            emoji::paragraph(ui, &line.text, FontId::proportional(12.5), p().secondary);
                        });
                        last_from = None;
                    }
                }
            }
            ui.add_space(4.0);
        });
    }

    fn settings(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let modal = egui::Modal::new(egui::Id::new("settings")).frame(sheet()).show(ctx, |ui| {
            ui.set_width(340.0);
            ui.label(RichText::new("Settings").size(17.0));
            ui.add_space(22.0);
            theme::caption(ui, "Appearance");
            ui.add_space(4.0);
            let options = [(Appearance::System, "System"), (Appearance::Light, "Light"), (Appearance::Dark, "Dark")];
            if theme::segmented(ui, &mut self.cfg.appearance, &options) {
                ctx.set_theme(self.cfg.appearance);
                self.cfg.save();
            }
            ui.add_space(20.0);
            theme::caption(ui, "Transmission");
            ui.add_space(4.0);
            let changed = theme::segmented(
                ui,
                &mut self.cfg.voice_mode,
                &[(VoiceMode::Activation, "Voice activation"), (VoiceMode::PushToTalk, "Push to talk")],
            );
            if changed {
                self.controls.ptt_mode.store(self.cfg.voice_mode == VoiceMode::PushToTalk, Relaxed);
                self.apply_hotkey();
                self.cfg.save();
            }
            ui.add_space(20.0);

            match self.cfg.voice_mode {
                VoiceMode::Activation => {
                    ui.horizontal(|ui| {
                        theme::caption(ui, "Sensitivity");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(format!("{:.0} dB", self.cfg.vad_threshold_db))
                                    .size(11.0)
                                    .color(p().secondary),
                            );
                        });
                    });
                    ui.add_space(6.0);
                    let level = self.controls.level_db.get();
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 4.0), Sense::hover());
                    let frac = |db: f32| egui::remap_clamp(db, -70.0..=0.0, 0.0..=1.0);
                    ui.painter().rect_filled(rect, CornerRadius::same(2), p().hover);
                    let mut fill = rect;
                    fill.set_width(rect.width() * frac(level));
                    let open = level >= self.cfg.vad_threshold_db;
                    ui.painter().rect_filled(fill, CornerRadius::same(2), if open { p().accent } else { p().faint });
                    let tx = rect.left() + rect.width() * frac(self.cfg.vad_threshold_db);
                    ui.painter().vline(tx, rect.expand(4.0).y_range(), Stroke::new(1.5f32, p().text));
                    ui.add_space(8.0);
                    ui.spacing_mut().slider_width = ui.available_width();
                    let resp = ui.add(egui::Slider::new(&mut self.cfg.vad_threshold_db, -70.0..=0.0).show_value(false));
                    if resp.changed() {
                        self.controls.threshold_db.set(self.cfg.vad_threshold_db);
                    }
                    if resp.drag_stopped() {
                        self.cfg.save();
                    }
                    ctx.request_repaint_after(Duration::from_millis(50));
                }
                VoiceMode::PushToTalk => {
                    theme::caption(ui, "Shortcut");
                    ui.add_space(4.0);
                    let label = if self.recording {
                        "Press a shortcut…".to_string()
                    } else {
                        pretty_hotkey(&self.cfg.ptt_hotkey)
                    };
                    if theme::shortcut_button(ui, &label, self.recording).clicked() && !self.recording {
                        self.recording = true;
                        self.apply_hotkey();
                    }
                    ui.add_space(6.0);
                    let hint = if self.recording { "Esc cancels." } else { "Works in any app. Click to change." };
                    ui.label(RichText::new(hint).size(11.0).color(p().faint));
                    if let Some(e) = &self.hotkey_error {
                        ui.label(RichText::new(e).size(11.5).color(p().accent));
                    }
                }
            }
            ui.add_space(20.0);
            theme::caption(ui, "Devices");
            ui.label(RichText::new("Follows the macOS default input and output.").size(12.5).color(p().secondary));
            if let Some(e) = &self.audio_error {
                ui.label(RichText::new(e).size(11.5).color(p().accent));
            }
        });
        if modal.should_close() {
            self.settings_open = false;
            if self.recording {
                self.recording = false;
                self.apply_hotkey();
            }
        }
    }

    fn password_prompt(&mut self, ctx: &egui::Context) {
        let Some((id, mut pw)) = self.join_password.take() else { return };
        let mut submit = false;
        let modal = egui::Modal::new(egui::Id::new("password")).frame(sheet()).show(ctx, |ui| {
            ui.set_width(300.0);
            ui.label(RichText::new("This channel is locked").size(17.0));
            ui.add_space(20.0);
            theme::caption(ui, "Password");
            theme::field(ui, &mut pw, "", true).request_focus();
            ui.add_space(22.0);
            submit = theme::primary_button(ui, "Join", !pw.is_empty()).clicked()
                || (!pw.is_empty() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
        });
        if submit {
            let _ = self.cmd.send(Cmd::Join(id, Some(pw)));
        } else if !modal.should_close() {
            self.join_password = Some((id, pw));
        }
    }
}

impl App {
    /// Runs before any widget sees input, so the recorded keys never reach the modal or the composer.
    fn record_shortcut(&mut self, ctx: &egui::Context) {
        let hit = ctx.input_mut(|i| {
            i.events.retain(|e| !matches!(e, egui::Event::Text(_)));
            let pos = i.events.iter().position(|e| matches!(e, egui::Event::Key { pressed: true, repeat: false, .. }));
            pos.map(|p| i.events.remove(p))
        });
        let Some(egui::Event::Key { key, modifiers, .. }) = hit else { return };
        self.recording = false;
        if !(key == egui::Key::Escape && modifiers.is_none()) {
            self.cfg.ptt_hotkey = hotkey_string(key, modifiers);
            self.cfg.save();
        }
        self.apply_hotkey();
    }
}

/// Builds the string `global_hotkey` parses; a few egui key names differ from its tokens.
fn hotkey_string(key: egui::Key, m: egui::Modifiers) -> String {
    let mut parts = Vec::new();
    for (on, name) in [(m.ctrl, "Control"), (m.alt, "Alt"), (m.shift, "Shift"), (m.mac_cmd, "Super")] {
        if on {
            parts.push(name);
        }
    }
    parts.push(match key {
        egui::Key::Backtick => "Backquote",
        egui::Key::Equals => "Equal",
        egui::Key::OpenBracket => "BracketLeft",
        egui::Key::CloseBracket => "BracketRight",
        k => k.name(),
    });
    parts.join("+")
}

fn pretty_hotkey(s: &str) -> String {
    s.split('+')
        .map(|t| match t.trim().to_ascii_uppercase().as_str() {
            "CONTROL" | "CTRL" => "⌃",
            "ALT" | "OPTION" => "⌥",
            "SHIFT" => "⇧",
            "SUPER" | "CMD" | "COMMAND" => "⌘",
            "BACKQUOTE" => "`",
            _ => t.trim(),
        })
        .collect()
}

/// Empty strip under the traffic lights that moves the window, like a native title bar.
fn drag_region(ui: &mut egui::Ui, width: f32) -> Rect {
    let rect = Rect::from_min_size(ui.max_rect().min, Vec2::new(width, TITLEBAR_H));
    let resp = ui.interact(rect, ui.id().with("titlebar"), Sense::click_and_drag());
    if resp.drag_started() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if resp.double_clicked() {
        let maximized = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    }
    ui.allocate_rect(rect, Sense::hover());
    rect
}

fn caps_line(ui: &egui::Ui, text: &str, size: f32) -> emoji::Line {
    emoji::Line::new(ui.ctx(), text, size, |s| {
        ui.painter().layout_job(theme::spaced(s, size, Color32::PLACEHOLDER, 0.8))
    })
}

fn sheet() -> egui::Frame {
    egui::Frame::new()
        .fill(p().content)
        .corner_radius(CornerRadius::same(16))
        .inner_margin(28.0)
        .shadow(egui::epaint::Shadow { offset: [0, 18], blur: 48, spread: 0, color: Color32::from_black_alpha(36) })
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        theme::sync(ctx);
        if self.recording {
            self.record_shortcut(ctx);
        }
        if self.audio_lost.swap(false, Relaxed) {
            self.start_audio(ctx);
        }
        if let Some(identity) = self.shared.identity.lock().unwrap().take() {
            self.cfg.identity = Some(identity);
            self.cfg.save();
        }
        let s = self.shared.state.lock().unwrap().clone();

        if s.tree.is_empty() {
            self.connect_view(ctx, &s);
        } else {
            egui::SidePanel::left("sidebar")
                .exact_width(SIDEBAR_W)
                .resizable(false)
                .frame(egui::Frame::new().fill(p().sidebar))
                .show(ctx, |ui| self.sidebar(ui, &s));
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(p().content))
                .show(ctx, |ui| self.conversation(ui, &s));
        }
        self.settings(ctx);
        self.password_prompt(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.cfg.save();
        let _ = self.cmd.send(Cmd::Quit);
        if let Some(net) = self.net.take() {
            let _ = net.join();
        }
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 660.0])
            .with_min_inner_size([720.0, 440.0])
            .with_title("TSLean")
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false)
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")).unwrap_or_default()),
        ..Default::default()
    };
    eframe::run_native("TSLean", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

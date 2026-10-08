use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Key, KeyboardShortcut, Modifiers};
use egui_plot::{Line, Plot, PlotBounds};

use crate::beep::Beeper;
use crate::clock::{self, GB_LATITUDE};
use crate::core::{BASE_PERIOD, Core, MAX_STRIPCHART_SECONDS};
use crate::dataq::{self, DataQ};
use crate::deccalc::CAL_FILENAME;
use crate::declinometer::Declinometer;
use crate::dialogs::{self, DARK_ORANGE, DecDialog, ObsDialog, QuitChoice, RaDialog};
use crate::logo;
use crate::observation::{ObsType, State};

// Basic time
const STRIPCHART_PERIOD: Duration = Duration::from_micros(16_700); // = 60Hz

// Style
const BLUE: egui::Color32 = egui::Color32::from_rgb(0x21, 0x96, 0xF3);
const RED: egui::Color32 = egui::Color32::from_rgb(0xFF, 0x52, 0x52);
const GRAY: egui::Color32 = egui::Color32::from_rgb(0x7A, 0x7C, 0x7E);
const LEGACY_GREEN: egui::Color32 = egui::Color32::from_rgb(0x00, 0xFF, 0x00);
const LEGACY_RED: egui::Color32 = egui::Color32::from_rgb(0xFF, 0x00, 0x00);
pub const MIN_WIDTH: f32 = 860.0;
const CONTROLS_WIDTH: f32 = 460.0;
const CONSOLE_LINE_HEIGHT: f32 = 14.0;

const DATA_DIR: &str = "./data/";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Normal,
    Testing,
}

/// Everything in the menu bar
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Credits,
    Quit,
    Scan,
    Survey,
    Spectrum,
    GetInfo,
    Ra,
    Dec,
    Normal,
    Testing,
    Legacy,
}

impl Action {
    const ALL: [Self; 11] = [
        Self::Credits,
        Self::Quit,
        Self::Scan,
        Self::Survey,
        Self::Spectrum,
        Self::GetInfo,
        Self::Ra,
        Self::Dec,
        Self::Normal,
        Self::Testing,
        Self::Legacy,
    ];

    fn text(self) -> &'static str {
        match self {
            Self::Credits => "Credits...",
            Self::Quit => "Exit",
            Self::Scan => "New Scan...",
            Self::Survey => "New Survey...",
            Self::Spectrum => "New Spectrum...",
            Self::GetInfo => "Get Info...",
            Self::Ra => "RA...",
            Self::Dec => "Dec...",
            Self::Normal => "Normal",
            Self::Testing => "Testing",
            Self::Legacy => "Legacy",
        }
    }

    fn shortcut(self) -> Option<KeyboardShortcut> {
        let command = |key| Some(KeyboardShortcut::new(Modifiers::COMMAND, key));
        match self {
            Self::Credits | Self::Legacy => None,
            Self::Quit => Some(KeyboardShortcut::new(Modifiers::NONE, Key::Escape)),
            Self::Scan => command(Key::Num1),
            Self::Survey => command(Key::Num2),
            Self::Spectrum => command(Key::Num3),
            Self::GetInfo => command(Key::I),
            Self::Ra => command(Key::R),
            Self::Dec => command(Key::D),
            Self::Normal => command(Key::N),
            Self::Testing => command(Key::T),
        }
    }
}

fn lock(core: &Mutex<Core>) -> MutexGuard<'_, Core> {
    core.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn load_texture(ctx: &egui::Context, name: &str, png: &[u8]) -> egui::TextureHandle {
    let image = image::load_from_memory(png)
        .expect("bundled images are valid")
        .to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    let image = egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
    ctx.load_texture(name, image, egui::TextureOptions::LINEAR)
}

/// The logo, drawn with enough pixels to stay sharp on high-density displays
fn load_logo(ctx: &egui::Context) -> egui::TextureHandle {
    let size = 2 * dialogs::LOGO_SIZE as usize;
    let image =
        egui::ColorImage::from_rgba_unmultiplied([size, size], &logo::rasterize(size as u32));
    ctx.load_texture("logo", image, egui::TextureOptions::LINEAR)
}

/// Green Bank Observatory's 40-Foot Telescope's very own data acquisition system.
/// This is the main window of the application.
pub struct LunaMoth {
    core: Arc<Mutex<Core>>,
    /// Cleared to stop the thread that ticks the core
    running: Arc<AtomicBool>,

    // Mode
    mode: Mode,
    legacy_mode: bool,

    // Stripchart
    stripchart_speed: u32,
    channel_visibility: (bool, bool),

    // Dialogs
    obs_dialog: Option<ObsDialog>,
    dec_dialog: Option<DecDialog>,
    ra_dialog: Option<RaDialog>,
    credits_open: bool,
    quit_open: bool,
    quit_confirmed: bool,
    /// Whether the user is being asked if they really want to stop the observation
    confirming_stop: bool,
    /// How many prompts the user's attention has been called to
    prompts_seen: u64,

    // Images
    dish: egui::TextureHandle,
    base: egui::TextureHandle,
    logo: egui::TextureHandle,
}

impl LunaMoth {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Instruments
        let (dataq, declinometer) = dataq::discovery();
        let dataq = dataq.map_or(Ok(DataQ::simulated()), |device| {
            DataQ::new(&device).map_err(|err| err.to_string())
        });
        let declinometer = declinometer.map_or(Ok(Declinometer::simulated()), |device| {
            Declinometer::new(&device).map_err(|err| err.to_string())
        });

        let core = Core::new(
            clock::now(),
            dataq,
            declinometer,
            Beeper::new(),
            Path::new(CAL_FILENAME),
            Path::new(DATA_DIR),
        );
        Self::with_core(&cc.egui_ctx, core)
    }

    fn with_core(ctx: &egui::Context, core: Core) -> Self {
        let core = Arc::new(Mutex::new(core));
        let running = Arc::new(AtomicBool::new(true));
        spawn_core_thread(Arc::clone(&core), Arc::clone(&running));

        Self {
            core,
            running,
            mode: Mode::Normal,
            legacy_mode: false,
            stripchart_speed: 3,
            channel_visibility: (true, true),
            obs_dialog: None,
            dec_dialog: None,
            ra_dialog: None,
            credits_open: false,
            quit_open: false,
            quit_confirmed: false,
            confirming_stop: false,
            prompts_seen: 0,
            dish: load_texture(ctx, "dish", include_bytes!("../assets/dish.png")),
            base: load_texture(ctx, "base", include_bytes!("../assets/base.png")),
            logo: load_logo(ctx),
        }
    }

    fn stripchart_display_seconds(&self) -> f64 {
        MAX_STRIPCHART_SECONDS - (110.0 / 6.0) * f64::from(self.stripchart_speed)
    }

    fn any_dialog_open(&self) -> bool {
        self.obs_dialog.is_some()
            || self.dec_dialog.is_some()
            || self.ra_dialog.is_some()
            || self.credits_open
            || self.quit_open
    }

    /// Whether the action is available right now. Most things can't be done in the
    /// middle of an observation.
    fn is_enabled(action: Action, core: &Core) -> bool {
        let obs_is_loaded = core.obs.is_some();
        match action {
            Action::Scan | Action::Survey | Action::Spectrum | Action::Ra | Action::Dec => {
                !obs_is_loaded
            }
            Action::GetInfo => obs_is_loaded,
            _ => true,
        }
    }

    fn is_checked(&self, action: Action) -> Option<bool> {
        match action {
            Action::Normal => Some(self.mode == Mode::Normal),
            Action::Testing => Some(self.mode == Mode::Testing),
            Action::Legacy => Some(self.legacy_mode),
            _ => None,
        }
    }

    fn handle(&mut self, action: Action, ctx: &egui::Context, core: &mut Core, current_time: f64) {
        match action {
            Action::Credits => self.credits_open = true,
            Action::Quit => self.quit_open = true,
            Action::Scan => self.new_observation(ObsType::Scan, core, current_time),
            Action::Survey => self.new_observation(ObsType::Survey, core, current_time),
            Action::Spectrum => self.new_observation(ObsType::Spectrum, core, current_time),
            Action::GetInfo => self.obs_dialog = core.obs.as_ref().and_then(ObsDialog::info),
            Action::Ra => self.ra_dialog = Some(RaDialog::default()),
            Action::Dec => self.dec_dialog = Some(DecDialog::new()),
            Action::Normal => self.mode = Mode::Normal,
            Action::Testing => self.mode = Mode::Testing,
            Action::Legacy => self.toggle_state_legacy(ctx, core),
        }
    }

    fn new_observation(&mut self, obs_type: ObsType, core: &Core, current_time: f64) {
        self.obs_dialog = Some(ObsDialog::new(obs_type, &core.clock, current_time));
    }

    /// Makes Luna Moth look like the outgoing ERIRA DAQ software.
    fn toggle_state_legacy(&mut self, ctx: &egui::Context, core: &mut Core) {
        self.legacy_mode = !self.legacy_mode;
        core.legacy_mode = self.legacy_mode;
        if self.legacy_mode {
            let mut visuals = egui::Visuals::light();
            visuals.override_text_color = Some(LEGACY_RED);
            visuals.panel_fill = LEGACY_GREEN;
            visuals.window_fill = LEGACY_GREEN;
            visuals.extreme_bg_color = LEGACY_GREEN;
            ctx.set_visuals(visuals);
        } else {
            ctx.set_visuals(ctx.theme().default_visuals());
        }
    }

    fn menu_bar(&self, ui: &mut egui::Ui, core: &Core) -> Option<Action> {
        let mut clicked = None;
        let mut item = |ui: &mut egui::Ui, action: Action| {
            let mut button = match self.is_checked(action) {
                Some(checked) => egui::Button::selectable(checked, action.text()),
                None => egui::Button::new(action.text()),
            };
            if let Some(shortcut) = action.shortcut() {
                button = button.shortcut_text(ui.ctx().format_shortcut(&shortcut));
            }
            if ui
                .add_enabled(Self::is_enabled(action, core), button)
                .clicked()
            {
                clicked = Some(action);
            }
        };

        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                ui.add_enabled(false, egui::Button::new("Help..."));
                item(ui, Action::Credits);
                ui.separator();
                item(ui, Action::Quit);
            });
            ui.menu_button("Observation", |ui| {
                item(ui, Action::Scan);
                item(ui, Action::Survey);
                item(ui, Action::Spectrum);
                ui.separator();
                item(ui, Action::GetInfo);
            });
            ui.menu_button("Calibrate", |ui| {
                item(ui, Action::Ra);
                item(ui, Action::Dec);
            });
            ui.menu_button("Mode", |ui| {
                item(ui, Action::Normal);
                item(ui, Action::Testing);
                ui.separator();
                item(ui, Action::Legacy);
            });
        });
        clicked
    }

    /// Knobs for the simulated signal and declinometer
    fn testing_frame(ui: &mut egui::Ui, core: &mut Core) {
        ui.horizontal(|ui| {
            ui.group(|ui| {
                ui.vertical(|ui| {
                    ui.label("Signal");
                    egui::Grid::new("signal_dials")
                        .num_columns(2)
                        .show(ui, |ui| {
                            ui.label("Variance");
                            ui.add(egui::Slider::new(&mut core.sim.variance, 0..=16));
                            ui.end_row();
                            ui.label("Polarization");
                            ui.add(egui::Slider::new(&mut core.sim.polarization, 0..=16));
                            ui.end_row();
                            ui.label("Interference");
                            ui.add(egui::Slider::new(&mut core.sim.noise, 0..=16));
                            ui.end_row();
                        });
                    ui.checkbox(&mut core.sim.calibration, "Calibration");
                });
            });
            ui.group(|ui| {
                ui.vertical(|ui| {
                    ui.label("Declinometer");
                    ui.horizontal(|ui| {
                        ui.add_enabled(
                            !core.sim.dec_auto,
                            egui::Slider::new(&mut core.sim.declination, -1.0..=1.0).vertical(),
                        );
                        ui.checkbox(&mut core.sim.dec_auto, "Auto");
                    });
                });
            });
        });
    }

    /// Anything that could ruin an observation, where it can't be missed. Returns what
    /// the user chose to do about it, if anything.
    fn hazards(ui: &mut egui::Ui, core: &Core) -> Option<Action> {
        let mut action = None;
        let hazard = |ui: &mut egui::Ui, text: &str| {
            ui.label(
                egui::RichText::new(text)
                    .strong()
                    .color(egui::Color32::BLACK),
            );
        };
        for instrument in core.instruments_not_responding() {
            hazard(
                ui,
                &format!("{instrument} is not responding: nothing is being recorded"),
            );
        }
        if core.dataq_is_simulated() {
            hazard(ui, "No DataQ: channel voltages are SIMULATED");
        }
        if core.declinometer_is_simulated() {
            hazard(ui, "No declinometer: declination is SIMULATED");
        }
        if !core.dec_is_calibrated() {
            ui.horizontal(|ui| {
                hazard(ui, "Declination is not calibrated");
                let calibrate = egui::Button::new("Calibrate...");
                if ui
                    .add_enabled(Self::is_enabled(Action::Dec, core), calibrate)
                    .clicked()
                {
                    action = Some(Action::Dec);
                }
            });
        }
        action
    }

    fn has_hazards(core: &Core) -> bool {
        core.dataq_is_simulated()
            || core.declinometer_is_simulated()
            || !core.dec_is_calibrated()
            || !core.instruments_not_responding().is_empty()
    }

    /// Where the observation is at, and what the user has to do about it
    fn observation_panel(&mut self, ui: &mut egui::Ui, core: &mut Core, current_time: f64) {
        ui.label(egui::RichText::new(&core.message).size(20.0));
        let Some(obs) = &core.obs else {
            self.confirming_stop = false;
            ui.label(egui::RichText::new("No observation running").color(GRAY));
            return;
        };
        let title = format!("{} {}", obs.obs_type.capitalized(), obs.name);
        let state = obs.state;
        let target_dec = obs.target_dec();
        let prompt = core.prompt().cloned();
        let (progress, countdown) = core.obs_progress(current_time).unwrap_or_default();

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(title).strong());
            right_aligned(ui, |ui| {
                if !self.confirming_stop {
                    self.confirming_stop = ui.button("Stop").clicked();
                    return;
                }
                if ui.button("Keep going").clicked() {
                    self.confirming_stop = false;
                }
                if ui.button("Yes, stop").clicked() {
                    core.stop_observation(current_time);
                    self.confirming_stop = false;
                }
                ui.label("Stop early?");
            });
        });
        if core.obs.is_none() {
            return; // Just got stopped
        }

        phase_timeline(ui, state, progress);
        let status = match state {
            _ if prompt.is_some() => "Waiting for you",
            State::Off => "Calibration prompt",
            State::Cal1 | State::Cal2 => "Calibrating",
            State::Bg1 | State::Bg2 => "Taking background",
            State::Waiting => "Waiting for the starting RA",
            State::Data => "Taking data",
            State::Done => "Done",
        };
        ui.horizontal(|ui| {
            ui.label(status);
            right_aligned(ui, |ui| {
                ui.monospace(countdown);
            });
        });

        let Some(prompt) = prompt else {
            if state == State::Off {
                let get_ready =
                    format!("Get ready: move the telescope to {target_dec:.1}° declination");
                ui.label(egui::RichText::new(get_ready).color(GRAY));
            }
            return;
        };
        // What the user has to do, and one big button for when they've done it
        egui::Frame::group(ui.style())
            .stroke(egui::Stroke::new(2.0, DARK_ORANGE))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let numbered = prompt.instructions.len() > 1;
                for (i, instruction) in prompt.instructions.iter().enumerate() {
                    let text = if numbered {
                        format!("{}. {instruction}", i + 1)
                    } else {
                        instruction.clone()
                    };
                    ui.label(egui::RichText::new(text).size(18.0).strong());
                }
                let button = egui::Button::new(egui::RichText::new(&prompt.button).size(20.0))
                    .min_size(egui::vec2(ui.available_width(), 44.0));
                if ui.add(button).clicked() {
                    core.confirm_prompt(current_time);
                }
            });
    }

    fn controls(&mut self, ui: &mut egui::Ui, core: &mut Core, current_time: f64) {
        let readout = core.readout.clone();

        group_box(ui, "Observation", |ui| {
            self.observation_panel(ui, core, current_time);
        });

        group_box(ui, "Data", |ui| {
            value_row(ui, "Right Ascension:", &readout.ra);
            value_row(ui, "Declination:", &readout.dec);
            value_row(ui, "Channel A:", &readout.channel_a);
            value_row(ui, "Channel B:", &readout.channel_b);
            value_row(ui, "Sweep:", &readout.sweep);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Refresh rate:").color(GRAY));
                right_aligned(ui, |ui| {
                    ui.label(egui::RichText::new(readout.refresh).monospace().color(GRAY));
                });
            });
        });

        group_box(ui, "Strip chart", |ui| {
            ui.horizontal(|ui| {
                ui.label("Slower");
                ui.spacing_mut().slider_width = ui.available_width() - 60.0;
                ui.add(egui::Slider::new(&mut self.stripchart_speed, 0..=6).show_value(false));
                ui.label("Faster");
            });
            ui.horizontal(|ui| {
                if ui.button("Toggle Channels").clicked() {
                    let (a, b) = self.channel_visibility;
                    self.channel_visibility = (b, a != b);
                }
                if ui.button("Clear Chart").clicked() {
                    core.clear_stripchart();
                }
            });
        });

        self.console(ui, core);
    }

    /// "Console" output, with a view of where the telescope is pointing
    fn console(&self, ui: &mut egui::Ui, core: &Core) {
        let (rect, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
        let painter = ui.painter().with_clip_rect(rect);
        painter.rect_filled(rect, 5.0, egui::Color32::BLACK);
        let inner = rect.shrink(8.0);

        // Telescope visualization
        let base_size = self.base.size_vec2();
        let dish_size = self.dish.size_vec2();
        let base_rect = egui::Rect::from_min_size(
            egui::pos2(
                inner.right() - base_size.x,
                inner.center().y - base_size.y / 2.0,
            ),
            base_size,
        );
        let dish_rect = egui::Rect::from_min_size(base_rect.min + egui::vec2(0.0, 16.0), dish_size);
        let angle = (core.current_dec - GB_LATITUDE).to_radians() as f32;
        egui::Image::from_texture(&self.dish)
            .rotate(angle, egui::Vec2::splat(0.5))
            .paint_at(ui, dish_rect);
        egui::Image::from_texture(&self.base).paint_at(ui, base_rect);

        // The latest statuses and as many logs as fit
        let text_rect = inner.with_max_x(base_rect.left() - 8.0);
        let painter = painter.with_clip_rect(text_rect);
        let number_of_logs = (text_rect.height() / CONSOLE_LINE_HEIGHT).floor() as usize;
        let mut y = text_rect.bottom();
        for line in core.console_lines(number_of_logs).iter().rev() {
            painter.text(
                egui::pos2(text_rect.left(), y),
                egui::Align2::LEFT_BOTTOM,
                line,
                egui::FontId::monospace(11.0),
                egui::Color32::from_rgb(0x00, 0xFF, 0x00),
            );
            y -= CONSOLE_LINE_HEIGHT;
        }
    }

    fn stripchart(&self, ui: &mut egui::Ui, core: &Core, current_time: f64) {
        // We use these values several times
        let current_sidereal_seconds = core.clock.sidereal_seconds(current_time);
        let oldest_y = current_sidereal_seconds - self.stripchart_display_seconds();

        let (show_a, show_b) = self.channel_visibility;
        let visible = core
            .stripchart
            .iter()
            .filter(|point| point.timestamp >= oldest_y);
        let mut series_a = Vec::new();
        let mut series_b = Vec::new();
        let (mut min_x, mut max_x) = (f64::INFINITY, f64::NEG_INFINITY);
        for point in visible {
            if show_a {
                series_a.push([point.a, point.timestamp]);
                (min_x, max_x) = (min_x.min(point.a), max_x.max(point.a));
            }
            if show_b {
                series_b.push([point.b, point.timestamp]);
                (min_x, max_x) = (min_x.min(point.b), max_x.max(point.b));
            }
        }
        if min_x > max_x {
            (min_x, max_x) = (0.0, 1.0); // No data yet
        }
        let margin = ((max_x - min_x) * 0.05).max(0.005);

        Plot::new("stripchart")
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .allow_boxed_zoom(false)
            .allow_double_click_reset(false)
            .show_x(false)
            .show_y(false)
            .show_axes([true, false])
            .show_grid([true, false])
            .show(ui, |plot_ui| {
                plot_ui.set_plot_bounds(PlotBounds::from_min_max(
                    [min_x - margin, oldest_y],
                    [max_x + margin, current_sidereal_seconds],
                ));
                plot_ui.line(
                    Line::new("Channel B", series_b)
                        .color(RED)
                        .allow_hover(false),
                );
                plot_ui.line(
                    Line::new("Channel A", series_a)
                        .color(BLUE)
                        .allow_hover(false),
                );
            });
    }

    fn show_dialogs(&mut self, ctx: &egui::Context, core: &mut Core, current_time: f64) {
        if let Some(dialog) = &mut self.obs_dialog
            && !dialog.show(ctx, core, current_time)
        {
            self.obs_dialog = None;
        }

        if let Some(dialog) = &mut self.dec_dialog
            && !dialog.show(ctx, core, current_time)
        {
            self.dec_dialog = None;
            core.reload_dec_cal();
        }

        if let Some(dialog) = &mut self.ra_dialog
            && !dialog.show(ctx, core, current_time)
        {
            self.ra_dialog = None;
        }

        if self.credits_open {
            self.credits_open = dialogs::credits_dialog(ctx, &self.logo);
        }

        // Call attention to anything new that the user has to do
        if core.prompts_raised != self.prompts_seen {
            self.prompts_seen = core.prompts_raised;
            let attention = egui::UserAttentionType::Critical;
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(attention));
        }

        if self.quit_open {
            match dialogs::quit_dialog(ctx) {
                QuitChoice::Undecided => {}
                QuitChoice::GoBack => self.quit_open = false,
                QuitChoice::Exit => {
                    self.quit_confirmed = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }
}

impl eframe::App for LunaMoth {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let current_time = clock::now();
        let core = Arc::clone(&self.core);
        let mut core = lock(&core);

        // Confirm before closing
        if ctx.input(|i| i.viewport().close_requested()) && !self.quit_confirmed {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.quit_open = true;
        }

        let mut action = None;
        if !self.any_dialog_open() {
            for candidate in Action::ALL {
                let pressed = candidate
                    .shortcut()
                    .is_some_and(|shortcut| ctx.input_mut(|i| i.consume_shortcut(&shortcut)));
                if pressed && Self::is_enabled(candidate, &core) {
                    action = Some(candidate);
                }
            }
        }

        egui::Panel::top("menu_bar").show(ui, |ui| {
            action = self.menu_bar(ui, &core).or(action);
        });
        if Self::has_hazards(&core) {
            let frame = egui::Frame::new().fill(DARK_ORANGE).inner_margin(6.0);
            egui::Panel::top("hazards").frame(frame).show(ui, |ui| {
                action = Self::hazards(ui, &core).or(action);
            });
        }
        if self.mode == Mode::Testing {
            egui::Panel::bottom("testing_frame").show(ui, |ui| {
                Self::testing_frame(ui, &mut core);
            });
        }
        egui::Panel::left("controls")
            .resizable(false)
            .exact_size(CONTROLS_WIDTH)
            .show(ui, |ui| self.controls(ui, &mut core, current_time));
        egui::CentralPanel::default().show(ui, |ui| {
            self.stripchart(ui, &core, current_time);
        });

        if let Some(action) = action {
            self.handle(action, &ctx, &mut core, current_time);
        }
        self.show_dialogs(&ctx, &mut core, current_time);

        // Keep the stripchart moving
        ctx.request_repaint_after(STRIPCHART_PERIOD);
    }
}

impl Drop for LunaMoth {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

/// Tick the core at 100Hz, whether or not the window is being drawn
fn spawn_core_thread(core: Arc<Mutex<Core>>, running: Arc<AtomicBool>) {
    thread::spawn(move || {
        let period = Duration::from_secs_f64(BASE_PERIOD / 1000.0);
        let mut next_tick = Instant::now();
        while running.load(Ordering::Relaxed) {
            lock(&core).tick(clock::now());

            next_tick += period;
            let now = Instant::now();
            if next_tick > now {
                thread::sleep(next_tick - now);
            } else {
                next_tick = now; // Fell behind; don't try to catch up
            }
        }
    });
}

/// Every phase of an observation in a row, filled in as far as it's gotten
fn phase_timeline(ui: &mut egui::Ui, state: State, progress: f32) {
    const PHASES: [&str; 6] = [
        "Calibrate",
        "Background",
        "Wait",
        "Data",
        "Calibrate",
        "Background",
    ];
    // Nothing has begun in `Off`, and everything is over in `Done`
    let current = match state {
        State::Off => None,
        State::Cal1 => Some(0),
        State::Bg1 => Some(1),
        State::Waiting => Some(2),
        State::Data => Some(3),
        State::Cal2 => Some(4),
        State::Bg2 => Some(5),
        State::Done => Some(PHASES.len()),
    };
    ui.columns(PHASES.len(), |columns| {
        for (i, (ui, phase)) in columns.iter_mut().zip(PHASES).enumerate() {
            let filled = match current {
                Some(current) if i < current => 1.0,
                Some(current) if i == current => progress,
                _ => 0.0,
            };
            // Progress bars are wider than a column unless they're told otherwise
            let width = ui.available_width();
            ui.add(
                egui::ProgressBar::new(filled)
                    .desired_width(width)
                    .desired_height(8.0),
            );
            let mut label = egui::RichText::new(phase).small();
            label = if current == Some(i) {
                label.strong()
            } else {
                label.color(GRAY)
            };
            ui.vertical_centered(|ui| ui.label(label));
        }
    });
}

/// A titled frame around a group of widgets, as wide as the space available
fn group_box(ui: &mut egui::Ui, title: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.label(egui::RichText::new(title).small().strong());
        add_contents(ui);
    });
}

fn right_aligned(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    ui.with_layout(
        egui::Layout::right_to_left(egui::Align::Center),
        add_contents,
    );
}

/// A big label on the left with its value on the right
fn value_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).size(20.0));
        right_aligned(ui, |ui| {
            ui.label(egui::RichText::new(value).size(20.0).monospace().strong());
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::tests::temp_data_dir;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use std::path::PathBuf;

    /// Set this to a directory to have the tests save screenshots there (needs a GPU)
    const SCREENSHOTS_VAR: &str = "LUNA_MOTH_SCREENSHOTS";

    /// The app with simulated instruments
    fn harness(test_name: &str) -> (Harness<'static, LunaMoth>, PathBuf) {
        let dir = temp_data_dir(test_name);
        let core = Core::new(
            clock::now(),
            Ok(DataQ::simulated()),
            Ok(Declinometer::simulated()),
            Beeper::silent(),
            &dir.join(CAL_FILENAME),
            &dir,
        );
        let mut builder = Harness::builder().with_size([MIN_WIDTH, 900.0]);
        if std::env::var_os(SCREENSHOTS_VAR).is_some() {
            builder = builder.wgpu();
        }
        let mut harness = builder.build_eframe(|cc| LunaMoth::with_core(&cc.egui_ctx, core));
        harness.run_steps(2);
        screenshot(&mut harness, &format!("{test_name}-0-startup"));
        // Nothing real is plugged in, and that can't be missed
        harness.get_by_label("No DataQ: channel voltages are SIMULATED");
        harness.get_by_label("No declinometer: declination is SIMULATED");
        harness.get_by_label("Declination is not calibrated");
        harness.get_by_label("No observation running");
        (harness, dir)
    }

    fn screenshot(harness: &mut Harness<'_, LunaMoth>, name: &str) {
        if let Some(dir) = std::env::var_os(SCREENSHOTS_VAR) {
            let path = Path::new(&dir).join(format!("{name}.png"));
            harness.render().unwrap().save(path).unwrap();
        }
    }

    fn click(harness: &mut Harness<'_, LunaMoth>, label: &str) {
        harness.get_by_label(label).click();
        harness.run_steps(2);
    }

    fn press(harness: &mut Harness<'_, LunaMoth>, key: Key) {
        harness.key_press_modifiers(Modifiers::COMMAND, key);
        harness.run_steps(2);
    }

    #[test]
    fn switches_modes() {
        let (mut harness, _) = harness("ui-modes");
        // Let some data come in
        thread::sleep(Duration::from_millis(1200));
        harness.run_steps(2);
        screenshot(&mut harness, "ui-modes-1-normal");
        assert!(harness.query_by_label("Declinometer").is_none());

        press(&mut harness, Key::T);
        harness.get_by_label("Declinometer");
        harness.get_by_label("Interference");
        screenshot(&mut harness, "ui-modes-2-testing");

        let ctx = harness.ctx.clone();
        let app = harness.state_mut();
        let core = Arc::clone(&app.core);
        app.toggle_state_legacy(&ctx, &mut lock(&core));
        harness.run_steps(2);
        assert!(lock(&core).legacy_mode);
        screenshot(&mut harness, "ui-modes-3-legacy");

        press(&mut harness, Key::N);
        assert!(harness.query_by_label("Declinometer").is_none());
    }

    #[test]
    fn sets_up_a_scan_from_the_menu() {
        let (mut harness, dir) = harness("ui-scan");
        press(&mut harness, Key::Num1);
        harness.get_by_label("New Scan");
        harness.get_by_label("Declination");
        assert!(harness.query_by_label("Maximum Declination").is_none());
        // Shortcuts don't do anything while a dialog is open
        press(&mut harness, Key::T);
        assert_eq!(harness.state().mode, Mode::Normal);

        // Can't be started without a declination
        harness.get_by_label("Enter a declination");
        click(&mut harness, "Start Observation");
        harness.get_by_label("New Scan");
        screenshot(&mut harness, "ui-scan-1-incomplete");
        assert!(!dir.exists());
        click(&mut harness, "Cancel");
        assert!(harness.query_by_label("New Scan").is_none());

        // Surveys ask for both declinations, but not for a data acquisition rate
        press(&mut harness, Key::Num2);
        harness.get_by_label("New Survey");
        harness.get_by_label("Maximum Declination");
        assert!(harness.query_by_label("Data Acquisition Rate").is_none());
        screenshot(&mut harness, "ui-scan-2-survey");
        click(&mut harness, "Cancel");

        // There's no observation to get info about
        press(&mut harness, Key::I);
        assert!(harness.state().obs_dialog.is_none());
    }

    #[test]
    fn calibrates() {
        let (mut harness, dir) = harness("ui-calibrate");
        press(&mut harness, Key::R);
        harness.get_by_label("RA Calibration");
        screenshot(&mut harness, "ui-calibrate-1-ra");
        click(&mut harness, "Set RA");
        assert!(harness.query_by_label("RA Calibration").is_none());
        let core = Arc::clone(&harness.state().core);
        let sidereal_time = lock(&core).clock.sidereal_tuple(clock::now());
        assert_eq!(sidereal_time[..2], [0, 0]);

        press(&mut harness, Key::D);
        harness.get_by_label("Calibrate declination");
        harness.get_by_label("-25°");
        click(&mut harness, "Record");
        harness.get_by_label("-20°");
        click(&mut harness, "<");
        harness.get_by_label("-25°");
        screenshot(&mut harness, "ui-calibrate-2-dec");
        click(&mut harness, "Discard All");
        assert!(harness.query_by_label("Calibrate declination").is_none());
        assert!(!dir.join(CAL_FILENAME).exists());
    }

    #[test]
    fn confirms_before_quitting() {
        let (mut harness, _) = harness("ui-quit");
        harness.key_press(Key::Escape);
        harness.run_steps(2);
        harness.get_by_label("Exit?");
        screenshot(&mut harness, "ui-quit-1-confirm");
        click(&mut harness, "No, go back");
        assert!(harness.query_by_label("Exit?").is_none());
        assert!(!harness.state().quit_confirmed);

        click(&mut harness, "File");
        click(&mut harness, "Credits...");
        harness.get_by_label("Written by Finn James");
        screenshot(&mut harness, "ui-quit-2-credits");
        click(&mut harness, "Close");
        assert!(!harness.state().credits_open);
    }

    #[test]
    fn walks_through_an_observation() {
        let (mut harness, dir) = harness("ui-observation");
        let core = Arc::clone(&harness.state().core);

        // A spectrum that starts so soon that the calibration prompt comes right away
        let start_time = clock::now() + 30.0;
        let mut obs = crate::observation::Observation::new(ObsType::Spectrum);
        obs.set_name("ngc7027", &dir).unwrap();
        obs.set_start_and_end_times(start_time, start_time + 180.0);
        obs.set_dec(42.0, 65535.0).unwrap();
        lock(&core).start_observation(obs, clock::now());
        harness.run_steps(2);
        harness.get_by_label("Spectrum ngc7027");
        assert!(harness.query_by_label("No observation running").is_none());

        thread::sleep(Duration::from_millis(1500));
        harness.run_steps(2);
        harness.get_by_label("Waiting for you");
        harness.get_by_label("1. Move the telescope to 42.0° declination");
        harness.get_by_label("2. Set frequency to 1319.5MHz");
        harness.get_by_label("3. Turn the calibration switches ON");
        // Everything fits in the panel
        let button = harness.get_by_label("Start calibration").rect();
        assert!(button.right() < CONTROLS_WIDTH, "{button}");
        screenshot(&mut harness, "ui-observation-1-prompt");
        // The new observation is off limits while this one is running
        press(&mut harness, Key::Num1);
        assert!(harness.state().obs_dialog.is_none());

        click(&mut harness, "Start calibration");
        assert_eq!(lock(&core).obs.as_ref().unwrap().state, State::Cal1);
        assert!(harness.query_by_label("Start calibration").is_none());
        harness.get_by_label("Calibrating");
        thread::sleep(Duration::from_millis(1500));
        harness.run_steps(2);
        screenshot(&mut harness, "ui-observation-2-calibrating");

        // Stopping takes two clicks
        click(&mut harness, "Stop");
        harness.get_by_label("Stop early?");
        click(&mut harness, "Keep going");
        assert!(lock(&core).obs.is_some());
        click(&mut harness, "Stop");
        click(&mut harness, "Yes, stop");
        assert!(lock(&core).obs.is_none());
        harness.get_by_label("Spectrum stopped early");
        harness.get_by_label("No observation running");
        let contents = std::fs::read_to_string(dir.join("ngc7027_a.md1")).unwrap();
        assert!(contents.contains("TELESCOPE: The Mighty Forty"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

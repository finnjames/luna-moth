//! Dialogue boxes

use eframe::egui;

use crate::clock::hms_to_seconds;
use crate::core::Core;

mod dec;
mod obs;

pub use dec::DecDialog;
pub use obs::ObsDialog;

/// Show a modal dialog with a title. Returns what `add_contents` returned and whether
/// the user tried to dismiss the dialog (by pressing escape or clicking outside of it).
fn modal<R>(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    width: f32,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> (R, bool) {
    let response = egui::Modal::new(egui::Id::new(id)).show(ctx, |ui| {
        ui.set_width(width);
        ui.label(egui::RichText::new(title).strong());
        ui.separator();
        add_contents(ui)
    });
    let should_close = response.should_close();
    (response.inner, should_close)
}

/// A row of buttons along the bottom of a dialog, laid out from the right
fn button_row<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.add_space(8.0);
    // The row is exactly one button tall. A layout that's free to take up all of the
    // remaining height makes the dialog grow a little every frame.
    let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
    let layout = egui::Layout::right_to_left(egui::Align::Center);
    ui.allocate_ui_with_layout(size, layout, add_contents).inner
}

/// A label on the left with a widget on the right
fn field<R>(ui: &mut egui::Ui, label: &str, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            add_contents,
        )
        .inner
    })
    .inner
}

fn message_label(ui: &mut egui::Ui, message: &str, color: egui::Color32) {
    ui.vertical_centered(|ui| {
        ui.label(egui::RichText::new(message).strong().color(color));
    });
}

pub const DARK_ORANGE: egui::Color32 = egui::Color32::from_rgb(0xFF, 0x8C, 0x00);

/// Editor for a time of the form HH:MM:SS
fn time_edit(ui: &mut egui::Ui, hms: &mut [u32; 3]) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        // Rows nested in a right-to-left layout are filled in from the right, too
        let mut order = [0, 1, 2];
        if ui.layout().prefer_right_to_left() {
            order.reverse();
        }
        for (position, i) in order.into_iter().enumerate() {
            if position > 0 {
                ui.monospace(":");
            }
            let max = if i == 0 { 23 } else { 59 };
            ui.add(
                egui::DragValue::new(&mut hms[i])
                    .range(0..=max)
                    .speed(0.1)
                    .custom_formatter(|n, _| format!("{n:02}")),
            );
        }
    });
}

/// How big the logo is in the credits dialog, in points
pub const LOGO_SIZE: f32 = 200.0;

/// Credits dialog window. Returns whether to keep it open.
pub fn credits_dialog(ctx: &egui::Context, logo: &egui::TextureHandle) -> bool {
    let (close, dismissed) = modal(ctx, "credits_dialog", "Credits", 366.0, |ui| {
        ui.vertical_centered(|ui| {
            let logo_size = egui::Vec2::splat(LOGO_SIZE);
            ui.add(egui::Image::from_texture(logo).fit_to_exact_size(logo_size));
            ui.label(egui::RichText::new("Luna Moth").strong());
            ui.label("Written by Finn James");
            ui.label("Licensed under AGPLv3");
            ui.add_space(12.0);
            ui.button("Close").clicked()
        })
        .inner
    });
    !(close || dismissed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuitChoice {
    Undecided,
    GoBack,
    Exit,
}

/// Confirm before closing
pub fn quit_dialog(ctx: &egui::Context) -> QuitChoice {
    let (choice, dismissed) = modal(ctx, "quit_dialog", "Exit?", 320.0, |ui| {
        ui.label("Are you sure you want to exit? Incomplete observations may not be usable.");
        button_row(ui, |ui| {
            if ui.button("Yes, Exit").clicked() {
                QuitChoice::Exit
            } else if ui.button("No, go back").clicked() {
                QuitChoice::GoBack
            } else {
                QuitChoice::Undecided
            }
        })
    });
    if choice == QuitChoice::Undecided && dismissed {
        return QuitChoice::GoBack;
    }
    choice
}

/// RA calibration dialogue window
#[derive(Debug, Default)]
pub struct RaDialog {
    sidereal_value: [u32; 3],
}

impl RaDialog {
    /// Returns whether to keep the dialog open
    pub fn show(&mut self, ctx: &egui::Context, core: &mut Core, current_time: f64) -> bool {
        let (close, _) = modal(ctx, "ra_dialog", "RA Calibration", 320.0, |ui| {
            field(ui, "Current Sidereal Time", |ui| {
                time_edit(ui, &mut self.sidereal_value)
            });
            button_row(ui, |ui| {
                if ui.button("Set RA").clicked() {
                    core.calibrate_ra(hms_to_seconds(self.sidereal_value), current_time);
                    return true;
                }
                ui.button("Cancel").clicked()
            })
        });
        !close
    }
}

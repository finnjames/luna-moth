// Don't open a console window alongside the app on Windows
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod beep;
mod clock;
mod core;
mod data_file;
mod dataq;
mod deccalc;
mod declinometer;
mod dialogs;
mod logo;
mod logtask;
mod observation;

use eframe::egui;

const ICON_SIZE: u32 = 256;

fn main() -> eframe::Result {
    let icon = egui::IconData {
        rgba: logo::rasterize(ICON_SIZE),
        width: ICON_SIZE,
        height: ICON_SIZE,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Luna Moth")
            .with_inner_size([app::MIN_WIDTH, 900.0])
            .with_min_inner_size([app::MIN_WIDTH, 640.0])
            .with_icon(icon),
        ..Default::default()
    };
    eframe::run_native(
        "Luna Moth",
        options,
        Box::new(|cc| Ok(Box::new(app::LunaMoth::new(cc)))),
    )
}

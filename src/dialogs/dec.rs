use std::fs;
use std::io;
use std::path::Path;

use eframe::egui;

use super::{DARK_ORANGE, button_row, message_label, modal};
use crate::core::Core;
use crate::deccalc::{CAL_BACKUP_FILENAME, NORTH_DEC, SOUTH_DEC, STEP, dec_list};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NorthSouth {
    North,
    South,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InvalidData {
    NonMonotonic,
    Incomplete,
}

/// Declination calibration dialogue window
#[derive(Debug)]
pub struct DecDialog {
    /// The direction the telescope moves in during calibration
    direction: NorthSouth,
    current_dec: i32,
    /// Declinometer reading at each declination, from south to north
    data: Vec<Option<f64>>,
    warning: Option<String>,
}

fn index_of(dec: i32) -> usize {
    ((dec - SOUTH_DEC) / STEP) as usize
}

impl DecDialog {
    pub fn new() -> Self {
        Self {
            direction: NorthSouth::North,
            current_dec: SOUTH_DEC,
            data: vec![None; dec_list().len()],
            warning: None,
        }
    }

    fn starting_dec(&self) -> i32 {
        match self.direction {
            NorthSouth::North => SOUTH_DEC,
            NorthSouth::South => NORTH_DEC,
        }
    }

    fn step(&self) -> i32 {
        match self.direction {
            NorthSouth::North => STEP,
            NorthSouth::South => -STEP,
        }
    }

    /// Every declination, in the order that they get calibrated
    fn dec_range(&self) -> Vec<i32> {
        let mut decs = dec_list();
        if self.direction == NorthSouth::South {
            decs.reverse();
        }
        decs
    }

    fn set_direction(&mut self, direction: NorthSouth) {
        self.direction = direction;
        self.current_dec = self.starting_dec();
        self.data = vec![None; dec_list().len()];
    }

    fn handle_record(&mut self, core: &mut Core, current_time: f64) {
        core.beep(current_time);

        // Read just the declination value
        let Some(new_dec) = core.latest_declinometer_reading else {
            self.warning = Some("Warning: no reading from the declinometer".to_owned());
            return;
        };
        self.data[index_of(self.current_dec)] = Some(new_dec);

        self.warning = match self.validate_data(true) {
            Err(InvalidData::NonMonotonic) => Some("Warning: data is not monotonic".to_owned()),
            _ => None,
        };

        self.move_step(self.step());
    }

    /// Returns whether the calibration was saved
    fn handle_save(&self, core: &mut Core) -> bool {
        let result = match self.validate_data(false) {
            Ok(()) => self.save(&core.dec_cal_path).map_err(|err| err.to_string()),
            Err(InvalidData::NonMonotonic) => Err("non-monotonic data".to_owned()),
            Err(InvalidData::Incomplete) => Err("not all declinations recorded".to_owned()),
        };
        if let Err(err) = &result {
            core.log_warning(&format!("Dec cal failed: {err}"));
        }
        result.is_ok()
    }

    fn save(&self, path: &Path) -> io::Result<()> {
        // Copy over the current file to the backup file
        match fs::copy(path, path.with_file_name(CAL_BACKUP_FILENAME)) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }

        let contents: String = self
            .data
            .iter()
            .flatten()
            .map(|reading| format!("{reading}\n"))
            .collect();
        fs::write(path, contents)
    }

    fn validate_data(&self, allow_incomplete: bool) -> Result<(), InvalidData> {
        let recorded: Vec<f64> = self
            .dec_range()
            .into_iter()
            .filter_map(|dec| self.data[index_of(dec)])
            .collect();
        if !allow_incomplete && recorded.len() != self.data.len() {
            return Err(InvalidData::Incomplete);
        }
        for decs in recorded.windows(3) {
            if (decs[2] - decs[1]) * (decs[1] - decs[0]) <= 0.0 {
                return Err(InvalidData::NonMonotonic);
            }
        }
        Ok(())
    }

    fn move_step(&mut self, step: i32) {
        let new_dec = self.current_dec + step;
        if (SOUTH_DEC..=NORTH_DEC).contains(&new_dec) {
            self.current_dec = new_dec;
        }
    }

    /// Returns whether to keep the dialog open
    pub fn show(&mut self, ctx: &egui::Context, core: &mut Core, current_time: f64) -> bool {
        let (close, _) = modal(ctx, "dec_dialog", "Calibrate declination", 480.0, |ui| {
            ui.vertical_centered(|ui| {
                egui::Grid::new("dec_cal_values")
                    .num_columns(2)
                    .spacing([12.0, 1.0])
                    .show(ui, |ui| {
                        for dec in self.dec_range() {
                            let marker = if dec == self.current_dec { "-> " } else { "" };
                            let label = format!("{:>6}", format!("{marker}{dec}"));
                            ui.label(egui::RichText::new(label).monospace().strong());
                            let reading = self.data[index_of(dec)];
                            ui.monospace(reading.map_or(String::new(), |r| format!("{r:.2}")));
                            ui.end_row();
                        }
                    });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("Currently calibrating");
                    ui.label(egui::RichText::new(format!("{}°", self.current_dec)).size(20.0));
                });
            });
            if let Some(warning) = &self.warning {
                message_label(ui, warning, DARK_ORANGE);
            }

            button_row(ui, |ui| {
                let mut close = false;
                // If all values are filled, enable the save button
                let complete = self.data.iter().all(Option::is_some);
                if ui
                    .add_enabled(complete, egui::Button::new("Save"))
                    .clicked()
                {
                    self.handle_save(core);
                    close = true;
                }
                if ui.button("Record").clicked() {
                    self.handle_record(core, current_time);
                }
                if ui.button(">").clicked() {
                    self.move_step(self.step());
                }
                if ui.button("<").clicked() {
                    self.move_step(-self.step());
                }
                close |= ui.button("Discard All").clicked();

                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    // Only allow N/S choice at the first declination
                    ui.add_enabled_ui(self.current_dec == self.starting_dec(), |ui| {
                        let mut direction = self.direction;
                        egui::ComboBox::from_id_salt("north_south")
                            .selected_text(direction_text(direction))
                            .show_ui(ui, |ui| {
                                for choice in [NorthSouth::North, NorthSouth::South] {
                                    ui.selectable_value(
                                        &mut direction,
                                        choice,
                                        direction_text(choice),
                                    );
                                }
                            });
                        if direction != self.direction {
                            self.set_direction(direction);
                        }
                    });
                });
                close
            })
        });
        !close
    }
}

fn direction_text(direction: NorthSouth) -> &'static str {
    match direction {
        NorthSouth::North => "S -> N",
        NorthSouth::South => "N -> S",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::beep::Beeper;
    use crate::dataq::DataQ;
    use crate::deccalc::{CAL_FILENAME, DecCalc};
    use crate::declinometer::Declinometer;
    use crate::observation::tests::temp_data_dir;

    fn test_core(dir: &Path) -> Core {
        fs::create_dir_all(dir).unwrap();
        let mut core = Core::new(
            0.0,
            Ok(DataQ::simulated()),
            Ok(Declinometer::simulated()),
            Beeper::silent(),
            &dir.join(CAL_FILENAME),
            dir,
        );
        core.sim.dec_auto = false;
        core
    }

    /// Point the simulated telescope somewhere and record the reading
    fn record(dialog: &mut DecDialog, core: &mut Core, reading: i32) {
        core.sim.declination = f64::from(reading);
        core.tick(0.0);
        dialog.handle_record(core, 0.0);
    }

    #[test]
    fn steps_stay_within_bounds() {
        let mut dialog = DecDialog::new();
        dialog.move_step(-STEP);
        assert_eq!(dialog.current_dec, SOUTH_DEC);
        dialog.move_step(STEP);
        assert_eq!(dialog.current_dec, SOUTH_DEC + STEP);

        dialog.set_direction(NorthSouth::South);
        assert_eq!(dialog.current_dec, NORTH_DEC);
        assert_eq!(dialog.dec_range()[0], NORTH_DEC);
        dialog.move_step(dialog.step());
        assert_eq!(dialog.current_dec, NORTH_DEC - STEP);
    }

    #[test]
    fn warns_about_non_monotonic_data() {
        let dir = temp_data_dir("dec-monotonic");
        let mut core = test_core(&dir);
        let mut dialog = DecDialog::new();

        record(&mut dialog, &mut core, -50);
        record(&mut dialog, &mut core, -40);
        assert_eq!(dialog.warning, None);
        assert_eq!(dialog.current_dec, SOUTH_DEC + 2 * STEP);
        record(&mut dialog, &mut core, -45);
        assert_eq!(
            dialog.warning.as_deref(),
            Some("Warning: data is not monotonic")
        );
        assert_eq!(dialog.validate_data(false), Err(InvalidData::Incomplete));

        // Go back and fix it
        dialog.move_step(-dialog.step());
        record(&mut dialog, &mut core, -30);
        assert_eq!(dialog.warning, None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saves_a_calibration_from_north_to_south() {
        let dir = temp_data_dir("dec-save");
        let mut core = test_core(&dir);
        let cal_path = dir.join(CAL_FILENAME);
        fs::write(&cal_path, "old calibration\n").unwrap();

        let mut dialog = DecDialog::new();
        dialog.set_direction(NorthSouth::South);
        // The declinometer reads half the declination, minus one
        for dec in dialog.dec_range() {
            assert_eq!(dialog.current_dec, dec);
            record(&mut dialog, &mut core, dec / 2 - 1);
        }
        assert_eq!(dialog.warning, None);
        assert!(dialog.handle_save(&mut core));

        let backup = fs::read_to_string(dir.join(CAL_BACKUP_FILENAME)).unwrap();
        assert_eq!(backup, "old calibration\n");
        // Saved from south to north, no matter the direction of calibration
        let saved = fs::read_to_string(&cal_path).unwrap();
        let lines: Vec<&str> = saved.lines().collect();
        assert_eq!(lines.len(), 26);
        assert_eq!((lines[0], lines[25]), ("-13", "49"));

        let mut dec_calc = DecCalc::new();
        dec_calc.load_dec_cal(&cal_path).unwrap();
        assert!((dec_calc.calculate_declination(19.0) - 40.0).abs() < 1e-9);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_to_save_non_monotonic_data() {
        let dir = temp_data_dir("dec-refuse");
        let mut core = test_core(&dir);
        let mut dialog = DecDialog::new();
        for dec in dialog.dec_range() {
            record(&mut dialog, &mut core, if dec == 50 { 0 } else { dec });
        }
        assert!(!dialog.handle_save(&mut core));
        assert!(!dir.join(CAL_FILENAME).exists());
        let log = core.console_lines(1);
        assert_eq!(log[0], "[WARNING!] Dec cal failed: non-monotonic data");
        fs::remove_dir_all(&dir).unwrap();
    }
}

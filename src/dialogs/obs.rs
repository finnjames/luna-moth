//! Dialogue box for keying in a new observation

use eframe::egui;

use super::{DARK_ORANGE, button_row, field, message_label, modal, time_edit};
use crate::clock::{self, SECONDS_PER_DAY, SuperClock, hms_to_seconds, sidereal_to_solar};
use crate::core::{Alert, AlertCallback, Core};
use crate::observation::{ObsRecord, ObsType, Observation};

/// New observation dialogue window
#[derive(Debug)]
pub struct ObsDialog {
    /// The observation being set up. `None` when just showing info.
    obs: Option<Observation>,
    obs_type: ObsType,

    start_time: [u32; 3],
    end_time: [u32; 3],
    min_dec: String,
    max_dec: String,
    data_acquisition_rate_value: u32,
    file_name_value: String,
    default_filename: String,

    confirmed: bool,
    read_only: bool,
    error: Option<String>,
    warning: Option<String>,
}

impl ObsDialog {
    pub fn new(obs_type: ObsType, clock: &SuperClock, current_time: f64) -> Self {
        // Set default ra
        let sidereal_time = clock.sidereal_tuple(current_time);
        let mut dialog = Self {
            obs: Some(Observation::new(obs_type)),
            obs_type,
            start_time: sidereal_time,
            end_time: sidereal_time,
            min_dec: String::new(),
            max_dec: String::new(),
            data_acquisition_rate_value: 1,
            file_name_value: String::new(),
            default_filename: clock::time_slug(), // Make default filename
            confirmed: false,
            read_only: false,
            error: None,
            warning: None,
        };

        // If a scan or spectrum, only one dec needed
        if matches!(obs_type, ObsType::Scan | ObsType::Spectrum) {
            dialog.max_dec = "65535.0".to_owned(); // Arbitrary large number
        }
        // All spectra and surveys have data acquisition rates of 6
        if matches!(obs_type, ObsType::Spectrum | ObsType::Survey) {
            dialog.data_acquisition_rate_value = 6;
        }
        // All spectra have a duration of 180 seconds
        if obs_type == ObsType::Spectrum {
            dialog.end_time = [23, 59, 59];
        }
        dialog
    }

    /// Just checking on an observation that's already running
    pub fn info(obs: &Observation) -> Option<Self> {
        let record = obs.input_record.clone()?;
        Some(Self {
            obs: None,
            obs_type: obs.obs_type,
            start_time: record.start_time,
            end_time: record.end_time,
            min_dec: record.min_dec,
            max_dec: record.max_dec,
            data_acquisition_rate_value: record.data_acquisition_rate_value,
            default_filename: record.file_name_value.clone(),
            file_name_value: record.file_name_value,
            confirmed: false,
            read_only: true,
            error: None,
            warning: None,
        })
    }

    pub fn is_info(&self) -> bool {
        self.obs.is_none()
    }

    fn title(&self) -> String {
        let prefix = if self.is_info() { "Current" } else { "New" };
        format!("{prefix} {}", self.obs_type.capitalized())
    }

    fn get_filename(&self) -> &str {
        if self.file_name_value.is_empty() {
            return &self.default_filename;
        }
        &self.file_name_value
    }

    /// Returns whether to keep the dialog open
    pub fn show(&mut self, ctx: &egui::Context, core: &mut Core, current_time: f64) -> bool {
        let title = self.title();
        let (close, _) = modal(ctx, "obs_dialog", &title, 300.0, |ui| {
            let obs_type = self.obs_type;
            ui.add_enabled_ui(!self.read_only, |ui| {
                field(ui, "Starting RA", |ui| time_edit(ui, &mut self.start_time));
                if obs_type != ObsType::Spectrum {
                    field(ui, "Ending RA", |ui| time_edit(ui, &mut self.end_time));
                }
                let single_dec = matches!(obs_type, ObsType::Scan | ObsType::Spectrum);
                let min_dec_label = if single_dec {
                    "Declination"
                } else {
                    "Minimum Declination"
                };
                field(ui, min_dec_label, |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.min_dec).desired_width(100.0));
                });
                if !single_dec {
                    field(ui, "Maximum Declination", |ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.max_dec).desired_width(100.0));
                    });
                }
                if obs_type == ObsType::Scan {
                    field(ui, "Data Acquisition Rate", |ui| {
                        ui.add(
                            egui::DragValue::new(&mut self.data_acquisition_rate_value)
                                .range(1..=100)
                                .speed(0.1),
                        );
                    });
                }
                ui.separator();
                field(ui, "File name", |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.file_name_value)
                            .hint_text(self.default_filename.as_str())
                            .desired_width(140.0),
                    );
                });
            });

            let accept_text = if self.is_info() {
                "Close"
            } else if self.confirmed {
                "Start Observation"
            } else {
                "Next"
            };
            let close = button_row(ui, |ui| {
                if ui.button(accept_text).clicked() {
                    return self.accept(core, current_time);
                }
                !self.is_info() && ui.button("Cancel").clicked()
            });

            if let Some(error) = &self.error {
                message_label(ui, error, egui::Color32::RED);
            }
            if let Some(warning) = &self.warning {
                message_label(ui, warning, DARK_ORANGE);
            }
            close
        });
        !close
    }

    /// Returns whether the dialog should close
    fn accept(&mut self, core: &mut Core, current_time: f64) -> bool {
        if self.is_info() {
            return true;
        }
        self.error = None;
        self.warning = None;

        if !self.confirmed {
            match self.set_observation(core, current_time) {
                Ok(()) => {
                    // Confirm
                    self.read_only = true;
                    let record = self.record();
                    if let Some(obs) = &mut self.obs {
                        obs.input_record = Some(record);
                    }
                    self.confirmed = true;
                }
                Err(err) => self.error = Some(err),
            }
            return false;
        }

        // Already confirmed -> set observation and close
        let Some(obs) = self.obs.take() else {
            return true;
        };
        let target_dec = obs.min_dec
            - if obs.obs_type == ObsType::Survey {
                2.0
            } else {
                0.0
            };
        let mut alerts = vec![
            Alert::new(
                format!("Move the telescope to {target_dec:.1}° declination"),
                "Okay",
            ),
            Alert::new(
                format!("Is the telescope at {target_dec:.1}° declination?"),
                "Yes",
            ),
        ];
        if obs.obs_type == ObsType::Spectrum {
            alerts.push(Alert::new("Set frequency to 1319.5MHz", "Okay"));
            alerts.push(Alert::new("Is the frequency set to 1319.5MHz?", "Yes"));
        }
        core.alert(
            alerts,
            AlertCallback::SetObservation(Box::new(obs)),
            current_time,
        );
        true
    }

    /// Wrap the fields into record
    fn record(&self) -> ObsRecord {
        ObsRecord {
            start_time: self.start_time,
            end_time: self.end_time,
            min_dec: self.min_dec.clone(),
            max_dec: self.max_dec.clone(),
            data_acquisition_rate_value: self.data_acquisition_rate_value,
            file_name_value: self.get_filename().to_owned(),
        }
    }

    /// Attempt to add all necessary info to the encapsulated observation
    fn set_observation(&mut self, core: &Core, current_time: f64) -> Result<(), String> {
        let starting_ra = hms_to_seconds(self.start_time);
        let ending_ra = hms_to_seconds(self.end_time);
        let current_ra = core.clock.sidereal_seconds(current_time);

        // Calculate start and end times
        let (start_time, mut end_time) =
            resolve_times(starting_ra, ending_ra, current_ra, current_time);
        if self.obs_type == ObsType::Spectrum {
            end_time = start_time + 180.0;
        }

        let mut warnings = Vec::new();
        if starting_ra < current_ra.rem_euclid(SECONDS_PER_DAY) {
            warnings.push("Starting RA has passed, assuming the next day");
        }
        if ending_ra < starting_ra && self.obs_type != ObsType::Spectrum {
            warnings.push("Assuming ending RA is the next day");
        }
        if !warnings.is_empty() {
            self.warning = Some(warnings.join("\n"));
        }

        let new_min_dec = parse_dec(&self.min_dec)?;
        let new_max_dec = parse_dec(&self.max_dec)?;

        let filename = self.get_filename().to_owned();
        let Some(obs) = &mut self.obs else {
            return Ok(());
        };

        // Attempt to set observation data
        obs.set_dec(new_min_dec, new_max_dec)?;
        obs.set_start_and_end_times(start_time, end_time);
        obs.set_data_freq(self.data_acquisition_rate_value);
        // Last, because this creates the data files
        obs.set_name(&filename, &core.data_dir)
            .map_err(|err| format!("Couldn't create data files: {err}"))
    }
}

/// Turn starting and ending RAs (in sidereal seconds since sidereal midnight) into solar
/// times. The start is the next time the sky reaches the starting RA, so it's always in
/// the future, and the end is the first time the sky reaches the ending RA after that.
fn resolve_times(
    starting_ra: f64,
    ending_ra: f64,
    current_ra: f64,
    current_time: f64,
) -> (f64, f64) {
    let until_start = (starting_ra - current_ra).rem_euclid(SECONDS_PER_DAY);
    let duration = (ending_ra - starting_ra).rem_euclid(SECONDS_PER_DAY);
    let start_time = current_time + sidereal_to_solar(until_start);
    (start_time, start_time + sidereal_to_solar(duration))
}

/// Parse a declination, dropping any fractional part
fn parse_dec(text: &str) -> Result<f64, String> {
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|dec| dec.is_finite())
        .map(f64::trunc)
        .ok_or_else(|| "Dec vals must be numbers".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::beep::Beeper;
    use crate::dataq::DataQ;
    use crate::declinometer::Declinometer;
    use crate::observation::State;
    use crate::observation::tests::temp_data_dir;
    use std::fs;

    #[test]
    fn parses_declinations() {
        assert_eq!(parse_dec(" 38.9 "), Ok(38.0));
        assert_eq!(parse_dec("-2.5"), Ok(-2.0));
        assert_eq!(parse_dec("0"), Ok(0.0));
        assert!(parse_dec("").is_err());
        assert!(parse_dec("north").is_err());
        assert!(parse_dec("inf").is_err());
    }

    #[test]
    fn times_always_resolve_to_the_future() {
        let hours = |h: f64| h * 3600.0;
        let solar = |sidereal_hours: f64| 5000.0 + hours(sidereal_hours) / clock::SIDEREAL;
        let close =
            |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6;

        // It's 22:00:00: 23:00:00 -> 01:00:00 starts in an hour and crosses midnight
        let times = resolve_times(hours(23.0), hours(1.0), hours(22.0), 5000.0);
        assert!(close(times, (solar(1.0), solar(3.0))), "{times:?}");
        // A starting RA that already went by today means tomorrow
        let times = resolve_times(hours(21.0), hours(21.5), hours(22.0), 5000.0);
        assert!(close(times, (solar(23.0), solar(23.5))), "{times:?}");
        // It doesn't matter how long the clock has been running since it was calibrated
        let times = resolve_times(hours(1.0), hours(2.0), hours(24.0 * 3.0 + 23.5), 5000.0);
        assert!(close(times, (solar(1.5), solar(2.5))), "{times:?}");
    }

    #[test]
    fn sets_up_a_survey() {
        let dir = temp_data_dir("obs-dialog");
        let now = 1_000_000.0;
        let mut core = Core::new(
            now,
            Ok(DataQ::simulated()),
            Ok(Declinometer::simulated()),
            Beeper::silent(),
            &dir.join("dec-cal.txt"),
            &dir,
        );
        core.dismiss_alert(now); // Dec must be calibrated
        core.calibrate_ra(3600.0, now); // It's 01:00:00

        let mut dialog = ObsDialog::new(ObsType::Survey, &core.clock, now);
        assert_eq!(dialog.title(), "New Survey");
        assert_eq!(dialog.start_time, [1, 0, 0]);
        assert_eq!(dialog.data_acquisition_rate_value, 6);

        // Errors don't create any files
        dialog.min_dec = "forty".to_owned();
        assert!(!dialog.accept(&mut core, now));
        assert_eq!(dialog.error.as_deref(), Some("Dec vals must be numbers"));
        dialog.min_dec = "40".to_owned();
        dialog.max_dec = "30".to_owned();
        assert!(!dialog.accept(&mut core, now));
        assert_eq!(
            dialog.error.as_deref(),
            Some("Max dec must be greater min dec")
        );
        assert!(!dir.exists());

        dialog.start_time = [1, 10, 0];
        dialog.end_time = [0, 50, 0];
        dialog.min_dec = "30.5".to_owned();
        dialog.max_dec = "40".to_owned();
        dialog.file_name_value = "m31".to_owned();
        assert!(!dialog.accept(&mut core, now));
        assert_eq!(dialog.error, None);
        assert_eq!(
            dialog.warning.as_deref(),
            Some("Assuming ending RA is the next day")
        );
        assert!(dialog.read_only && dialog.confirmed);
        assert!(dir.join("m31_a.md2").exists());
        assert!(dir.join("m31_b.md2").exists());

        assert!(dialog.accept(&mut core, now));
        assert!(core.obs.is_none());
        assert_eq!(
            core.current_alert().unwrap().text,
            "Move the telescope to 28.0° declination"
        );
        core.dismiss_alert(now);
        assert_eq!(
            core.current_alert().unwrap().text,
            "Is the telescope at 28.0° declination?"
        );
        core.dismiss_alert(now);
        assert_eq!(core.current_alert(), None);

        let obs = core.obs.as_mut().unwrap();
        assert_eq!((obs.min_dec, obs.max_dec), (30.0, 40.0));
        assert_eq!(obs.freq, 1);
        // 10 sidereal minutes from now, minus the 150s of calibration, background, and buffer
        assert_eq!(
            obs.communicate(None, now),
            crate::observation::Comm::NoAction
        );
        let user_start = obs.state_time_interval.1;
        assert!((user_start - (now + 600.0 / clock::SIDEREAL - 150.0)).abs() < 1e-6);
        assert_eq!(obs.state, State::Off);

        let info = ObsDialog::info(obs).unwrap();
        assert_eq!(info.title(), "Current Survey");
        assert_eq!(info.start_time, [1, 10, 0]);
        assert_eq!(info.get_filename(), "m31");
        assert!(info.is_info() && info.read_only);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stays_the_same_size_with_messages() {
        use egui_kittest::Harness;
        use egui_kittest::kittest::Queryable;

        let dir = temp_data_dir("obs-dialog-size");
        let now = clock::now();
        let mut core = Core::new(
            now,
            Ok(DataQ::simulated()),
            Ok(Declinometer::simulated()),
            Beeper::silent(),
            &dir.join("dec-cal.txt"),
            &dir,
        );
        core.dismiss_alert(now); // Dec must be calibrated
        let mut dialog = ObsDialog::new(ObsType::Scan, &core.clock, now);
        let mut harness = Harness::builder().with_size([860.0, 900.0]).build_ui(|ui| {
            let ctx = ui.ctx().clone();
            dialog.show(&ctx, &mut core, now);
        });
        harness.run_steps(3);

        // With no declination, there's an error under the buttons
        harness.get_by_label("Next").click();
        harness.run_steps(3);
        let button = harness.get_by_label("Next").rect();
        let error = harness.get_by_label("Dec vals must be numbers").rect();
        assert!(error.top() - button.bottom() < 10.0, "{button} {error}");
        harness.run_steps(30);
        assert_eq!(
            harness.get_by_label("Dec vals must be numbers").rect(),
            error
        );
        assert_eq!(harness.get_by_label("Next").rect(), button);
        drop(harness);

        // The default starting RA is a moment ago, so confirming comes with a warning
        dialog.min_dec = "40".to_owned();
        let mut harness = Harness::builder().with_size([860.0, 900.0]).build_ui(|ui| {
            let ctx = ui.ctx().clone();
            dialog.show(&ctx, &mut core, now);
        });
        harness.run_steps(3);
        harness.get_by_label("Next").click();
        harness.run_steps(3);
        let warning = "Starting RA has passed, assuming the next day";
        let warning_rect = harness.get_by_label(warning).rect();
        let button = harness.get_by_label("Start Observation").rect();
        assert!(
            warning_rect.top() - button.bottom() < 10.0,
            "{button} {warning_rect}"
        );
        harness.run_steps(30);
        assert_eq!(harness.get_by_label(warning).rect(), warning_rect);
        assert_eq!(harness.get_by_label("Start Observation").rect(), button);
        drop(harness);
        fs::remove_dir_all(&dir).unwrap();
    }
}

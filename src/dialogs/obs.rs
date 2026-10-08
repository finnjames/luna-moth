//! Dialogue box for keying in a new observation

use eframe::egui;

use super::{DARK_ORANGE, button_row, field, message_label, modal, time_edit};
use crate::clock::{
    self, SECONDS_PER_DAY, SuperClock, format_duration, hms_to_seconds, local_time_of_day,
    sidereal_to_solar,
};
use crate::core::Core;
use crate::observation::{ObsRecord, ObsType, Observation, USER_BUFFER, data_file_names};

/// New observation dialogue window
#[derive(Debug)]
pub struct ObsDialog {
    obs_type: ObsType,
    /// Just checking on an observation that's already running
    info: bool,

    start_time: [u32; 3],
    end_time: [u32; 3],
    min_dec: String,
    max_dec: String,
    data_acquisition_rate_value: u32,
    file_name_value: String,
    default_filename: String,

    /// Whether the user agreed to replace data files that already exist
    overwrite: bool,
    error: Option<String>,
}

/// What an observation would do if it were started right now, worked out from what's
/// been keyed in so far
#[derive(Debug, PartialEq)]
struct Plan {
    /// When data collection is scheduled to begin and end
    start_time: f64,
    end_time: f64,
    min_dec: f64,
    max_dec: f64,
    filename: String,

    /// Seconds until the user gets asked to start the calibration
    until_prompt: f64,
    /// Seconds until data collection begins, which is after the scheduled start if
    /// there isn't enough time for the calibration and background first
    until_data: f64,
    /// Seconds that data collection begins after its scheduled start
    late: f64,
    /// Seconds of data collection
    duration: f64,
    /// Whether the starting RA already went by today
    starts_tomorrow: bool,
    /// Data files that are already there and would be replaced
    existing_files: Vec<String>,
}

impl ObsDialog {
    pub fn new(obs_type: ObsType, clock: &SuperClock, current_time: f64) -> Self {
        // Set default ra
        let sidereal_time = clock.sidereal_tuple(current_time);
        let mut dialog = Self {
            obs_type,
            info: false,
            start_time: sidereal_time,
            end_time: sidereal_time,
            min_dec: String::new(),
            max_dec: String::new(),
            data_acquisition_rate_value: 1,
            file_name_value: String::new(),
            default_filename: clock::time_slug(), // Make default filename
            overwrite: false,
            error: None,
        };

        // If a scan or spectrum, only one dec needed
        if dialog.has_single_dec() {
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
            obs_type: obs.obs_type,
            info: true,
            start_time: record.start_time,
            end_time: record.end_time,
            min_dec: record.min_dec,
            max_dec: record.max_dec,
            data_acquisition_rate_value: record.data_acquisition_rate_value,
            default_filename: record.file_name_value.clone(),
            file_name_value: record.file_name_value,
            overwrite: false,
            error: None,
        })
    }

    fn has_single_dec(&self) -> bool {
        matches!(self.obs_type, ObsType::Scan | ObsType::Spectrum)
    }

    fn title(&self) -> String {
        let prefix = if self.info { "Current" } else { "New" };
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
        let (close, _) = modal(ctx, "obs_dialog", &title, 340.0, |ui| {
            ui.add_enabled_ui(!self.info, |ui| self.fields(ui));

            if self.info {
                return button_row(ui, |ui| ui.button("Close").clicked());
            }

            // Show what's going to happen before it's started
            ui.separator();
            let plan = self.plan(core, current_time);
            match &plan {
                Ok(plan) => self.summary(ui, plan, core, current_time),
                Err(reason) => {
                    ui.label(egui::RichText::new(reason).weak());
                }
            }
            if let Some(error) = &self.error {
                message_label(ui, error, egui::Color32::RED);
            }

            let can_start = plan
                .as_ref()
                .is_ok_and(|plan| plan.existing_files.is_empty() || self.overwrite);
            button_row(ui, |ui| {
                let start = egui::Button::new("Start Observation");
                if ui.add_enabled(can_start, start).clicked()
                    && let Ok(plan) = plan
                {
                    return self.start(plan, core, current_time);
                }
                ui.button("Cancel").clicked()
            })
        });
        !close
    }

    fn fields(&mut self, ui: &mut egui::Ui) {
        let obs_type = self.obs_type;
        field(ui, "Starting RA", |ui| time_edit(ui, &mut self.start_time));
        if obs_type != ObsType::Spectrum {
            field(ui, "Ending RA", |ui| time_edit(ui, &mut self.end_time));
        }
        let single_dec = self.has_single_dec();
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
    }

    /// Spell out what the plan is, and anything about it that the user should know
    fn summary(&mut self, ui: &mut egui::Ui, plan: &Plan, core: &Core, current_time: f64) {
        if plan.until_prompt > 0.0 {
            let until_prompt = format_duration(plan.until_prompt);
            ui.label(format!("Calibration prompt in {until_prompt}"));
        } else {
            ui.label("Calibration prompt right away");
        }
        ui.label(format!(
            "Data starts in {}, at {}",
            format_duration(plan.until_data),
            local_time_of_day(current_time + plan.until_data),
        ));
        ui.label(format!("Runs for {}", format_duration(plan.duration)));

        let warning = |ui: &mut egui::Ui, text: &str| {
            ui.label(egui::RichText::new(text).strong().color(DARK_ORANGE));
        };
        if plan.starts_tomorrow {
            warning(ui, "Starting RA has passed, assuming the next day");
        }
        if plan.late > 0.0 {
            let late = format_duration(plan.late);
            warning(
                ui,
                &format!("Not enough time to calibrate first: data will start {late} late"),
            );
        }
        if core.dataq_is_simulated() || core.declinometer_is_simulated() {
            warning(ui, "This will record SIMULATED data");
        }
        if !plan.existing_files.is_empty() {
            let existing_files = plan.existing_files.join(", ");
            warning(ui, &format!("Already in the data folder: {existing_files}"));
            ui.checkbox(&mut self.overwrite, "Overwrite");
        }
    }

    /// Work out what the observation would do, or why it can't be started yet
    fn plan(&self, core: &Core, current_time: f64) -> Result<Plan, String> {
        if self.min_dec.trim().is_empty() {
            return Err("Enter a declination".to_owned());
        }
        let min_dec = parse_dec(&self.min_dec)?;
        let max_dec = parse_dec(&self.max_dec)?;
        if max_dec < min_dec {
            return Err("Max dec must be greater min dec".to_owned());
        }

        // Calculate start and end times
        let starting_ra = hms_to_seconds(self.start_time);
        let ending_ra = hms_to_seconds(self.end_time);
        let current_ra = core.clock.sidereal_seconds(current_time);
        let (start_time, mut end_time) =
            resolve_times(starting_ra, ending_ra, current_ra, current_time);
        if self.obs_type == ObsType::Spectrum {
            end_time = start_time + 180.0;
        }

        // The calibration and background have to come first, however long that takes
        let preparation = Observation::new(self.obs_type).preparation_duration();
        let until_start = start_time - current_time;
        let until_prompt = (until_start - preparation - USER_BUFFER).max(0.0);
        let until_data = until_start.max(preparation);
        let duration = end_time - current_time - until_data;
        if duration <= 0.0 {
            return Err("There's no time to take data before the ending RA".to_owned());
        }

        let filename = self.get_filename().to_owned();
        let existing_files = data_file_names(self.obs_type, &filename)
            .into_iter()
            .filter(|name| core.data_dir.join(name).exists())
            .collect();

        Ok(Plan {
            start_time,
            end_time,
            min_dec,
            max_dec,
            filename,
            until_prompt,
            until_data,
            late: until_data - until_start,
            duration,
            starts_tomorrow: starting_ra < current_ra.rem_euclid(SECONDS_PER_DAY),
            existing_files,
        })
    }

    /// Hand the observation over to be run. Returns whether the dialog should close.
    fn start(&mut self, plan: Plan, core: &mut Core, current_time: f64) -> bool {
        let mut obs = Observation::new(self.obs_type);
        obs.set_start_and_end_times(plan.start_time, plan.end_time);
        obs.set_data_freq(self.data_acquisition_rate_value);
        obs.input_record = Some(self.record());
        // The data files get created last, once nothing else can go wrong
        let result = obs.set_dec(plan.min_dec, plan.max_dec).and_then(|()| {
            obs.set_name(&plan.filename, &core.data_dir)
                .map_err(|err| format!("Couldn't create data files: {err}"))
        });
        match result {
            Ok(()) => {
                core.start_observation(obs, current_time);
                true
            }
            Err(err) => {
                self.error = Some(err);
                false
            }
        }
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
    use crate::observation::tests::temp_data_dir;
    use crate::observation::{Comm, State};
    use std::fs;
    use std::path::Path;

    /// A core where it's 01:00:00 at `now`
    fn test_core(now: f64, dir: &Path) -> Core {
        let mut core = Core::new(
            now,
            Ok(DataQ::simulated()),
            Ok(Declinometer::simulated()),
            Beeper::silent(),
            &dir.join("dec-cal.txt"),
            dir,
        );
        core.calibrate_ra(3600.0, now);
        core
    }

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
    fn plans_a_survey() {
        let dir = temp_data_dir("obs-plan");
        let now = 1_000_000.0;
        let core = test_core(now, &dir);
        let mut dialog = ObsDialog::new(ObsType::Survey, &core.clock, now);
        assert_eq!(dialog.title(), "New Survey");
        assert_eq!(dialog.start_time, [1, 0, 0]);
        assert_eq!(dialog.data_acquisition_rate_value, 6);
        let reason = |dialog: &ObsDialog| dialog.plan(&core, now).unwrap_err();

        // What's missing or wrong, until there's enough to go on
        assert_eq!(reason(&dialog), "Enter a declination");
        dialog.min_dec = "forty".to_owned();
        assert_eq!(reason(&dialog), "Dec vals must be numbers");
        dialog.min_dec = "40".to_owned();
        assert_eq!(reason(&dialog), "Dec vals must be numbers");
        dialog.max_dec = "30".to_owned();
        assert_eq!(reason(&dialog), "Max dec must be greater min dec");
        dialog.max_dec = "50".to_owned();
        // The ending RA is the same as the starting RA by default
        assert_eq!(
            reason(&dialog),
            "There's no time to take data before the ending RA"
        );

        // 01:10:00 -> 00:50:00 is ten minutes from now, for 23 hours and 40 minutes
        dialog.start_time = [1, 10, 0];
        dialog.end_time = [0, 50, 0];
        dialog.min_dec = "30.5".to_owned();
        dialog.file_name_value = "m31".to_owned();
        let plan = dialog.plan(&core, now).unwrap();
        let ten_minutes = 600.0 / clock::SIDEREAL;
        assert!((plan.start_time - (now + ten_minutes)).abs() < 1e-6);
        assert!((plan.until_data - ten_minutes).abs() < 1e-6);
        // The prompt comes 150s (calibration, background, and buffer) before the start
        assert!((plan.until_prompt - (ten_minutes - 150.0)).abs() < 1e-6);
        assert!((plan.duration - (23.0 * 3600.0 + 2400.0) / clock::SIDEREAL).abs() < 1e-6);
        assert_eq!((plan.min_dec, plan.max_dec), (30.0, 50.0));
        assert_eq!(plan.late, 0.0);
        assert!(!plan.starts_tomorrow);
        assert!(plan.existing_files.is_empty());
        assert_eq!(plan.filename, "m31");

        // Nothing gets created until the observation is started
        assert!(!dir.exists());
    }

    #[test]
    fn plans_around_calibration_when_the_start_is_too_soon() {
        let dir = temp_data_dir("obs-plan-late");
        let now = 1_000_000.0;
        let core = test_core(now, &dir);

        // A scan that's supposed to start in one sidereal minute, for five
        let mut dialog = ObsDialog::new(ObsType::Scan, &core.clock, now);
        dialog.start_time = [1, 1, 0];
        dialog.end_time = [1, 6, 0];
        dialog.min_dec = "40".to_owned();
        let plan = dialog.plan(&core, now).unwrap();
        let one_minute = 60.0 / clock::SIDEREAL;
        assert_eq!(plan.until_prompt, 0.0);
        // Calibration and background take two minutes no matter what
        assert_eq!(plan.until_data, 120.0);
        assert!((plan.late - (120.0 - one_minute)).abs() < 1e-6);
        assert!((plan.duration - (6.0 * one_minute - 120.0)).abs() < 1e-6);

        // Too short to get any data at all
        dialog.end_time = [1, 1, 30];
        assert!(dialog.plan(&core, now).is_err());

        // The starting RA that's filled in by default is already in the past by the
        // time anyone can do anything about it
        let mut dialog = ObsDialog::new(ObsType::Spectrum, &core.clock, now);
        dialog.min_dec = "40".to_owned();
        let plan = dialog.plan(&core, now + 5.0).unwrap();
        assert!(plan.starts_tomorrow);
        assert!(plan.until_data > 23.9 * 3600.0);
        assert!((plan.duration - 180.0).abs() < 1e-6);
    }

    #[test]
    fn starts_a_survey() {
        let dir = temp_data_dir("obs-start");
        let now = 1_000_000.0;
        let mut core = test_core(now, &dir);
        let mut dialog = ObsDialog::new(ObsType::Survey, &core.clock, now);
        dialog.start_time = [1, 10, 0];
        dialog.end_time = [2, 0, 0];
        dialog.min_dec = "30".to_owned();
        dialog.max_dec = "40".to_owned();
        dialog.file_name_value = "m31".to_owned();

        let plan = dialog.plan(&core, now).unwrap();
        assert!(dialog.start(plan, &mut core, now));
        assert!(dir.join("m31_a.md2").exists());
        assert!(dir.join("m31_b.md2").exists());
        assert_eq!(core.message, "Survey scheduled");

        let obs = core.obs.as_mut().unwrap();
        assert_eq!((obs.min_dec, obs.max_dec), (30.0, 40.0));
        assert_eq!(obs.target_dec(), 28.0);
        assert_eq!(obs.freq, 1);
        assert_eq!(obs.communicate(None, now), Comm::NoAction);
        let user_start = obs.state_time_interval.1;
        assert!((user_start - (now + 600.0 / clock::SIDEREAL - 150.0)).abs() < 1e-6);
        assert_eq!(obs.state, State::Off);

        let info = ObsDialog::info(obs).unwrap();
        assert_eq!(info.title(), "Current Survey");
        assert_eq!(info.start_time, [1, 10, 0]);
        assert_eq!(info.get_filename(), "m31");
        assert!(info.info);

        // Using the same name again would replace the files that were just created
        let plan = dialog.plan(&core, now).unwrap();
        assert_eq!(
            plan.existing_files,
            ["m31_a.md2", "m31_b.md2", "m31_comp.md2"]
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn guards_against_overwriting_and_stays_the_same_size() {
        use egui_kittest::Harness;
        use egui_kittest::kittest::Queryable;

        let dir = temp_data_dir("obs-dialog-ui");
        let now = clock::now();
        let mut core = test_core(now, &dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("m31_a.md1"), "existing data\n").unwrap();

        let mut dialog = ObsDialog::new(ObsType::Scan, &core.clock, now);
        dialog.start_time = [2, 0, 0];
        dialog.end_time = [3, 0, 0];
        let mut closed = false;
        let mut harness = Harness::builder().with_size([860.0, 900.0]).build_ui(|ui| {
            let ctx = ui.ctx().clone();
            closed = closed || !dialog.show(&ctx, &mut core, now);
        });
        harness.run_steps(3);

        // Can't be started until there's a declination
        harness.get_by_label("Enter a declination");
        let start = harness.get_by_label("Start Observation").rect();
        harness.get_by_label("Start Observation").click();
        harness.run_steps(30);
        // The dialog doesn't creep
        assert_eq!(harness.get_by_label("Start Observation").rect(), start);
        drop(harness);
        assert!(!closed);

        dialog.min_dec = "40".to_owned();
        dialog.file_name_value = "m31".to_owned();
        let mut harness = Harness::builder().with_size([860.0, 900.0]).build_ui(|ui| {
            let ctx = ui.ctx().clone();
            if !closed {
                closed = !dialog.show(&ctx, &mut core, now);
            }
        });
        harness.run_steps(3);
        harness.get_by_label_contains("Calibration prompt in 5");
        harness.get_by_label_contains("Data starts in 59m");
        harness.get_by_label("Runs for 59m 50s");
        harness.get_by_label("This will record SIMULATED data");
        harness.get_by_label("Already in the data folder: m31_a.md1");
        let start = harness.get_by_label("Start Observation").rect();
        harness.run_steps(30);
        assert_eq!(harness.get_by_label("Start Observation").rect(), start);

        // Can't be started until the user agrees to overwrite
        harness.get_by_label("Start Observation").click();
        harness.run_steps(3);
        harness.get_by_label("Overwrite").click();
        harness.run_steps(3);
        let existing = fs::read_to_string(dir.join("m31_a.md1")).unwrap();
        assert_eq!(existing, "existing data\n");
        harness.get_by_label("Start Observation").click();
        harness.run_steps(3);
        assert!(harness.query_by_label("Start Observation").is_none());
        drop(harness);

        assert!(closed);
        assert_eq!(fs::read_to_string(dir.join("m31_a.md1")).unwrap(), "");
        assert_eq!(core.obs.as_ref().unwrap().name, "m31");
        fs::remove_dir_all(&dir).unwrap();
    }
}

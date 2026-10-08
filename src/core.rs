//! Everything Luna Moth does that isn't drawing: reading the instruments, keeping time,
//! and walking the user through observations. Runs on its own thread so that data
//! acquisition never depends on the window being repainted.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::beep::Beeper;
use crate::clock::{SuperClock, TimerId};
use crate::dataq::{DataQ, SimParams};
use crate::deccalc::{DecCalError, DecCalc};
use crate::declinometer::Declinometer;
use crate::logtask::{LogTask, Status};
use crate::observation::{Comm, DataPoint, ObsType, Observation};

// Basic time
pub const BASE_PERIOD: f64 = 10.0; // ms = 100Hz
const GUI_UPDATE_PERIOD: f64 = 1000.0; // ms = 1Hz

/// The longest stretch of time, in sidereal seconds, that the stripchart can display
pub const MAX_STRIPCHART_SECONDS: f64 = 120.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    pub text: String,
    pub button: String,
}

impl Alert {
    pub fn new(text: impl Into<String>, button: &str) -> Self {
        Self {
            text: text.into(),
            button: button.to_owned(),
        }
    }
}

/// What to do once the user has acknowledged every alert in a sequence
#[derive(Debug)]
pub enum AlertCallback {
    None,
    StartCalibration,
    StartBackground,
    SetObservation(Box<Observation>),
}

#[derive(Debug)]
struct AlertSequence {
    alerts: VecDeque<Alert>,
    callback: AlertCallback,
    /// Whether the alert at the front has been logged and beeped about yet
    announced: bool,
}

/// The values on display, which are refreshed at 1Hz so that they're readable
#[derive(Clone, Debug)]
pub struct Readout {
    pub ra: String,
    pub dec: String,
    pub channel_a: String,
    pub channel_b: String,
    pub sweep: String,
    pub refresh: String,
    /// From 0 to 1
    pub progress: f32,
    pub progress_label: String,
}

pub struct Core {
    pub clock: SuperClock,
    gui_timer: TimerId,
    data_timer: TimerId,

    // Instruments
    dataq: DataQ,
    declinometer: Declinometer,
    pub sim: SimParams,

    // Observation
    pub obs: Option<Observation>,
    pub completed_one_calibration: bool,
    /// Where observations write their data files
    pub data_dir: PathBuf,

    // Most recent data
    dec_calc: DecCalc,
    pub dec_cal_path: PathBuf,
    pub current_dec: f64,
    current_data_point: Option<DataPoint>,
    /// Most recent raw (uncalibrated) declinometer reading
    pub latest_declinometer_reading: Option<f64>,
    /// Recent data points, oldest first
    pub stripchart: VecDeque<DataPoint>,

    // DataQ communication interpretation
    previous_transmission: Option<Comm>,

    // "Console" output
    message_log: Vec<LogTask>,
    pub message: String,
    pub readout: Readout,

    // Alerts
    alerts: VecDeque<AlertSequence>,
    /// How many alerts have been raised so far
    pub alerts_announced: u64,

    // Bleeps and bloops
    beeper: Beeper,
    pub legacy_mode: bool,

    // Measure refresh rate
    time_of_last_fps_update: Instant,
    ticks_since_last_fps_update: u32,
}

impl Core {
    pub fn new(
        current_time: f64,
        dataq: Result<DataQ, String>,
        declinometer: Result<Declinometer, String>,
        beeper: Beeper,
        dec_cal_path: &Path,
        data_dir: &Path,
    ) -> Self {
        let mut message_log = vec![LogTask::new(">>> LUNA MOTH")];
        println!("{}", message_log[0].get_message());
        message_log.push(LogTask::new(">>> Initializing..."));
        println!("{}", message_log[1].get_message());
        let initializing_log_task = message_log.len() - 1;

        let mut clock = SuperClock::new(current_time);
        // Assign timers to functions meant to fire periodically
        let gui_timer = clock.add_timer(GUI_UPDATE_PERIOD, current_time);
        let data_timer = clock.add_timer(1000.0, current_time);

        let mut device_warnings = Vec::new();
        let mut dataq = dataq.unwrap_or_else(|err| {
            device_warnings.push(format!("DataQ failed to open ({err}), simulating data"));
            DataQ::simulated()
        });
        let mut declinometer = declinometer.unwrap_or_else(|err| {
            device_warnings.push(format!(
                "Declinometer failed to open ({err}), simulating data"
            ));
            Declinometer::simulated()
        });
        if let Err(err) = dataq.start() {
            device_warnings.push(format!("DataQ failed to start ({err})"));
        }
        declinometer.start();

        let mut core = Self {
            clock,
            gui_timer,
            data_timer,
            dataq,
            declinometer,
            sim: SimParams::default(),
            obs: None,
            completed_one_calibration: false,
            data_dir: data_dir.to_owned(),
            dec_calc: DecCalc::new(),
            dec_cal_path: dec_cal_path.to_owned(),
            current_dec: 0.0,
            current_data_point: None,
            latest_declinometer_reading: None,
            stripchart: VecDeque::new(),
            previous_transmission: None,
            message_log,
            message: "...".to_owned(),
            readout: Readout {
                ra: "00:00:00".to_owned(),
                dec: "0.0000°".to_owned(),
                channel_a: "0.0000V".to_owned(),
                channel_b: "0.0000V".to_owned(),
                sweep: "n/a".to_owned(),
                refresh: "0.00Hz".to_owned(),
                progress: 0.0,
                progress_label: "n/a".to_owned(),
            },
            alerts: VecDeque::new(),
            alerts_announced: 0,
            beeper,
            legacy_mode: false,
            time_of_last_fps_update: Instant::now(),
            ticks_since_last_fps_update: 0,
        };

        if core.dataq.is_simulated() && device_warnings.iter().all(|w| !w.starts_with("DataQ")) {
            core.log("DataQ not found, simulating data", current_time);
        }
        if core.declinometer.is_simulated()
            && device_warnings
                .iter()
                .all(|w| !w.starts_with("Declinometer"))
        {
            core.log("Declinometer not found, simulating data", current_time);
        }
        for warning in device_warnings {
            core.log_warning(&warning);
        }

        // Initial dec calibration
        match core.dec_calc.load_dec_cal(&core.dec_cal_path) {
            Ok(()) => {}
            Err(DecCalError::NotFound) => {
                let alert = Alert::new("Dec must be calibrated", "Got it");
                core.alert(vec![alert], AlertCallback::None, current_time);
            }
            Err(err) => {
                core.log_warning(&format!("Dec cal is unusable: {err}"));
                let alert = Alert::new("Dec must be calibrated", "Got it");
                core.alert(vec![alert], AlertCallback::None, current_time);
            }
        }

        // Alert user that Luna Moth is done initializing
        core.message_log[initializing_log_task].set_status(Status::Success);
        core.message("Ready!!!", true, true, current_time);
        core
    }

    /// Primary controller for each clock tick. Fires at 100Hz. Anything meant to update
    /// as often as possible should be placed here. Everything else should be assigned
    /// to a timer.
    pub fn tick(&mut self, current_time: f64) {
        // Attempt to grab latest data point; it won't always be written to the data file
        let dataq_datum = self.dataq.read_latest(current_time, &self.sim); // Get data from DAQ
        let declinometer_datum = self.declinometer.read_latest(current_time, &self.sim); // Get data from declinometer
        let sidereal_timestamp = self.clock.sidereal_seconds(current_time);
        if declinometer_datum.is_some() {
            self.latest_declinometer_reading = declinometer_datum;
        }

        // If data was available above, save it
        if let (Some(dataq_datum), Some(declinometer_datum)) = (dataq_datum, declinometer_datum) {
            self.current_dec = self.dec_calc.calculate_declination(declinometer_datum);
            let data_point = DataPoint {
                timestamp: sidereal_timestamp, // RA
                dec: self.current_dec,         // Dec
                a: dataq_datum.a,              // Channel A
                b: dataq_datum.b,              // Channel B
            };
            self.current_data_point = Some(data_point);
            self.stripchart.push_back(data_point);
        }
        // Remove the trailing end of the stripchart
        let oldest = sidereal_timestamp - MAX_STRIPCHART_SECONDS;
        while self
            .stripchart
            .front()
            .is_some_and(|point| point.timestamp < oldest)
        {
            self.stripchart.pop_front();
        }

        // Run all timers that are due
        if self.clock.timer_is_due(self.gui_timer, current_time) {
            self.update_readout(current_time);
        }
        if self.clock.timer_is_due(self.data_timer, current_time) {
            self.update_data(current_time);
        }

        self.ticks_since_last_fps_update += 1; // For measuring fps
    }

    fn update_data(&mut self, current_time: f64) {
        let Some(obs) = &mut self.obs else {
            return;
        };

        let period = 1000.0 / f64::from(obs.freq); // Hz -> ms
        self.clock
            .set_timer_period(self.data_timer, period, current_time);

        let transmission = obs.communicate(self.current_data_point.as_ref(), current_time);
        let obs_type = obs.obs_type;
        if let Some(err) = obs.take_io_error() {
            self.log_warning(&format!("Couldn't write data: {err}"));
        }

        if Some(transmission) != self.previous_transmission {
            if transmission == Comm::StartCal {
                let mut alerts = Vec::new();
                if self.completed_one_calibration {
                    if obs_type == ObsType::Survey {
                        alerts.push(Alert::new("STOP the telescope", "Okay"));
                        alerts.push(Alert::new("Has the telescope been stopped?", "Yes"));
                    } else if obs_type == ObsType::Spectrum {
                        alerts.push(Alert::new("Set frequency to 1319.5MHz", "Okay"));
                        alerts.push(Alert::new("Is the frequency set to 1319.5MHz?", "Yes"));
                    }
                }
                alerts.push(Alert::new("Turn the calibration switches ON", "Okay"));
                alerts.push(Alert::new("Are the calibration switches ON?", "Yes"));
                self.alert(alerts, AlertCallback::StartCalibration, current_time);
                self.completed_one_calibration = true; // Only alert on second cal
            } else if transmission == Comm::StartBg {
                self.alert(
                    vec![
                        Alert::new("Turn the calibration switches OFF", "Okay"),
                        Alert::new("Are the calibration switches OFF?", "Yes"),
                    ],
                    AlertCallback::StartBackground,
                    current_time,
                );
            }
        }

        let lower = obs_type.lower();
        match transmission {
            Comm::StartWait => {
                self.next_obs_state(current_time);
                self.message(
                    &format!("Waiting for {lower} to begin..."),
                    true,
                    true,
                    current_time,
                );
            }
            Comm::StartData => {
                self.next_obs_state(current_time);
                self.message(&format!("Taking {lower} data!!!"), true, true, current_time);
            }
            Comm::Finished => {
                self.next_obs_state(current_time);
                self.message(
                    &format!("{} complete!!!", obs_type.capitalized()),
                    true,
                    true,
                    current_time,
                );
                self.obs = None;
            }
            Comm::SendTelNorth => {
                self.message(
                    "Send telescope NORTH at max speed!!!",
                    false,
                    false,
                    current_time,
                );
                self.beep(current_time);
            }
            Comm::SendTelSouth => {
                self.message(
                    "Send telescope SOUTH at max speed!!!",
                    false,
                    false,
                    current_time,
                );
                self.beep(current_time);
            }
            Comm::EndSendTel => {
                self.message(
                    &format!("Taking {lower} data!!!"),
                    false,
                    false,
                    current_time,
                );
            }
            Comm::FinishingSweep => {
                self.message("Finishing last sweep!!!", false, true, current_time);
            }
            Comm::Beep => self.beep(current_time),
            Comm::StartCal | Comm::StartBg | Comm::NoAction => {}
        }

        self.previous_transmission = Some(transmission);
    }

    /// Move the observation to its next state, reporting any trouble with its files
    fn next_obs_state(&mut self, current_time: f64) {
        let Some(obs) = &mut self.obs else {
            return;
        };
        obs.next(current_time);
        if let Some(err) = obs.take_io_error() {
            self.log_warning(&format!("Couldn't write data: {err}"));
        }
    }

    fn update_readout(&mut self, current_time: f64) {
        self.readout.ra = self.clock.formatted_sidereal_time(current_time); // RA
        self.readout.dec = format!("{:.4}°", self.current_dec); // Dec
        if let Some(obs) = &self.obs {
            // Sweep number
            self.readout.sweep = obs.sweep_number.map_or("n/a".to_owned(), |n| n.to_string());
        }

        self.update_progress(current_time);
        self.update_fps();
        if let Some(point) = &self.current_data_point {
            self.readout.channel_a = format!("{:.4}V", point.a);
            self.readout.channel_b = format!("{:.4}V", point.b);
        }
    }

    fn update_progress(&mut self, current_time: f64) {
        let Some(obs) = &self.obs else {
            self.readout.progress_label = "n/a".to_owned();
            self.readout.progress = 0.0;
            return;
        };
        let (start_time, end_time) = obs.state_time_interval;

        if end_time > 0.0 {
            self.readout.progress =
                if end_time > current_time && current_time > start_time && start_time > 0.0 {
                    ((current_time - start_time) / (end_time - start_time)) as f32
                } else {
                    0.0
                };
            self.readout.progress_label = countdown_label(end_time - current_time);
        }
    }

    /// Updates the fps counter to display current refresh rate
    fn update_fps(&mut self) {
        let current_time = Instant::now();
        let time_since_last_fps_update =
            (current_time - self.time_of_last_fps_update).as_secs_f64();

        self.readout.refresh = if time_since_last_fps_update > 0.0 {
            let fps = f64::from(self.ticks_since_last_fps_update) / time_since_last_fps_update;
            format!("{fps:.2}Hz")
        } else {
            "-1.0".to_owned()
        };
        self.time_of_last_fps_update = current_time;
        self.ticks_since_last_fps_update = 0;
    }

    pub fn clear_stripchart(&mut self) {
        self.stripchart.clear();
    }

    /// Set the sidereal clock and start the stripchart over
    pub fn calibrate_ra(&mut self, sidereal_seconds: f64, current_time: f64) {
        self.clock
            .calibrate_sidereal_time(sidereal_seconds, current_time);
        self.clear_stripchart();
    }

    /// Read the dec calibration from file again, after it was (maybe) changed
    pub fn reload_dec_cal(&mut self) {
        match self.dec_calc.load_dec_cal(&self.dec_cal_path) {
            Ok(()) | Err(DecCalError::NotFound) => {}
            Err(err) => self.log_warning(&format!("Dec cal is unusable: {err}")),
        }
    }

    pub fn message(&mut self, message: &str, beep: bool, log: bool, current_time: f64) {
        if log {
            self.log(message, current_time);
        }
        if beep {
            self.beep(current_time);
        }
        self.message = message.to_owned();
    }

    pub fn log(&mut self, message: &str, current_time: f64) {
        let leading_str = self.clock.formatted_sidereal_time(current_time);
        self.push_log(message, &leading_str);
    }

    pub fn log_warning(&mut self, message: &str) {
        self.push_log(message, "WARNING!");
    }

    fn push_log(&mut self, message: &str, leading_str: &str) {
        if self
            .message_log
            .last()
            .is_some_and(|last| last.message == message)
        {
            return; // No duplicates
        }
        let mut new_log_task = LogTask::new(message);
        new_log_task.set_leading_str(leading_str);
        println!("{}", new_log_task.get_message());
        self.message_log.push(new_log_task);
    }

    /// The latest statuses and last `number_of_logs` logs, oldest first
    pub fn console_lines(&self, number_of_logs: usize) -> Vec<String> {
        let start = self.message_log.len().saturating_sub(number_of_logs);
        self.message_log[start..]
            .iter()
            .map(LogTask::get_message)
            .collect()
    }

    /// Show the user a sequence of alerts, one at a time, then run the callback
    pub fn alert(&mut self, alerts: Vec<Alert>, callback: AlertCallback, current_time: f64) {
        self.alerts.push_back(AlertSequence {
            alerts: alerts.into(),
            callback,
            announced: false,
        });
        self.announce_alert(current_time);
    }

    /// The alert that the user needs to acknowledge, if any
    pub fn current_alert(&self) -> Option<&Alert> {
        self.alerts
            .front()
            .and_then(|sequence| sequence.alerts.front())
    }

    /// The user acknowledged the current alert
    pub fn dismiss_alert(&mut self, current_time: f64) {
        let Some(sequence) = self.alerts.front_mut() else {
            return;
        };
        sequence.alerts.pop_front();
        sequence.announced = false;
        if sequence.alerts.is_empty()
            && let Some(sequence) = self.alerts.pop_front()
        {
            self.run_alert_callback(sequence.callback, current_time);
        }
        self.announce_alert(current_time);
    }

    /// Log and beep when a new alert comes up to the front
    fn announce_alert(&mut self, current_time: f64) {
        // Skip over sequences that have nothing to show
        while self
            .alerts
            .front()
            .is_some_and(|sequence| sequence.alerts.is_empty())
        {
            if let Some(sequence) = self.alerts.pop_front() {
                self.run_alert_callback(sequence.callback, current_time);
            }
        }
        let Some(sequence) = self.alerts.front_mut() else {
            return;
        };
        if sequence.announced {
            return;
        }
        sequence.announced = true;
        let text = sequence.alerts.front().map(|alert| alert.text.clone());
        if let Some(text) = text {
            self.alerts_announced += 1;
            self.log(&text, current_time);
            self.beep(current_time);
        }
    }

    fn run_alert_callback(&mut self, callback: AlertCallback, current_time: f64) {
        match callback {
            AlertCallback::None => {}
            AlertCallback::StartCalibration => {
                self.clock.reset_all_timer_anchors(current_time);
                self.next_obs_state(current_time);
                self.message("Taking calibration data!!!", true, true, current_time);
            }
            AlertCallback::StartBackground => {
                self.clock.reset_all_timer_anchors(current_time);
                self.next_obs_state(current_time);
                self.message("Taking background data!!!", true, true, current_time);
            }
            AlertCallback::SetObservation(obs) => self.obs = Some(*obs),
        }
    }

    /// Make beep play for user
    pub fn beep(&mut self, current_time: f64) {
        self.beeper.beep(current_time, self.legacy_mode);
    }
}

/// Format the time until (or since) the next step as e.g. "T-01:30"
fn countdown_label(time_until_next_step: f64) -> String {
    let total = time_until_next_step.abs().round() as u64;
    let (hours, minutes, seconds) = (total / 3600, total / 60 % 60, total % 60);
    let mut label = format!("T{}", if time_until_next_step > 0.0 { '-' } else { '+' });
    if hours > 0 {
        label += &format!("{hours:02}:");
    }
    if hours > 0 || minutes > 0 {
        label += &format!("{minutes:02}:");
    }
    label + &format!("{seconds:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::State;
    use crate::observation::tests::temp_data_dir;
    use std::fs;

    fn test_core(current_time: f64, dir: &Path) -> Core {
        let mut core = Core::new(
            current_time,
            Ok(DataQ::simulated()),
            Ok(Declinometer::simulated()),
            Beeper::silent(),
            &dir.join("dec-cal.txt"),
            dir,
        );
        // Hold the simulated telescope still
        core.sim.dec_auto = false;
        core
    }

    /// Tick at 100Hz until `until`, acknowledging every alert as soon as it comes up.
    /// Returns the text of each alert that was acknowledged.
    fn run(core: &mut Core, current_time: &mut f64, until: f64) -> Vec<String> {
        let mut acknowledged = Vec::new();
        while *current_time < until {
            core.tick(*current_time);
            if let Some(alert) = core.current_alert() {
                acknowledged.push(alert.text.clone());
                core.dismiss_alert(*current_time);
            }
            *current_time += BASE_PERIOD / 1000.0;
        }
        acknowledged
    }

    #[test]
    fn formats_countdowns() {
        assert_eq!(countdown_label(5.2), "T-05");
        assert_eq!(countdown_label(90.0), "T-01:30");
        assert_eq!(countdown_label(3605.0), "T-01:00:05");
        assert_eq!(countdown_label(-59.6), "T+01:00");
    }

    #[test]
    fn starts_up_by_asking_for_a_dec_calibration() {
        let dir = temp_data_dir("startup");
        let mut now = 1_000_000.0;
        let mut core = test_core(now, &dir);
        assert_eq!(core.message, "Ready!!!");
        assert_eq!(core.current_alert().unwrap().text, "Dec must be calibrated");
        assert_eq!(core.alerts_announced, 1);
        let lines = core.console_lines(100);
        assert_eq!(lines[0], ">>> LUNA MOTH");
        assert_eq!(lines[1], ">>> Initializing... done!");
        assert!(lines[2].ends_with("] DataQ not found, simulating data"));
        assert!(lines[3].ends_with("] Declinometer not found, simulating data"));
        assert!(lines[4].ends_with("] Dec must be calibrated"));
        assert!(lines[5].ends_with("] Ready!!!"));
        assert_eq!(core.console_lines(2).len(), 2);

        assert_eq!(
            run(&mut core, &mut now, 1_000_002.0),
            ["Dec must be calibrated"]
        );
        assert_eq!(core.current_alert(), None);
        assert!(core.stripchart.len() > 100);
        assert_ne!(core.readout.ra, "00:00:00");
        assert_eq!(core.readout.progress_label, "n/a");
    }

    #[test]
    fn runs_a_whole_spectrum() {
        let dir = temp_data_dir("core-spectrum");
        let mut now = 1_000_000.0;
        let mut core = test_core(now, &dir);
        run(&mut core, &mut now, 1_000_001.0);

        // Start in 80s; the user gets prompted 70s (cal + bg + 30s buffer) before then
        let mut obs = Observation::new(ObsType::Spectrum);
        obs.set_name("spectrum", &dir).unwrap();
        obs.set_start_and_end_times(now + 80.0, now + 260.0);
        obs.set_dec(0.0, 65535.0).unwrap();
        obs.set_data_freq(6);
        core.alert(
            vec![Alert::new("Move the telescope", "Okay")],
            AlertCallback::SetObservation(Box::new(obs)),
            now,
        );
        assert!(core.obs.is_none());
        assert_eq!(
            run(&mut core, &mut now, 1_000_009.0),
            ["Move the telescope"]
        );
        assert_eq!(core.obs.as_ref().unwrap().state, State::Off);
        assert!(core.readout.progress_label.starts_with("T-"));

        let acknowledged = run(&mut core, &mut now, 1_000_030.0);
        assert_eq!(
            acknowledged,
            [
                "Turn the calibration switches ON",
                "Are the calibration switches ON?"
            ]
        );
        assert_eq!(core.obs.as_ref().unwrap().state, State::Cal1);
        assert_eq!(core.message, "Taking calibration data!!!");

        let acknowledged = run(&mut core, &mut now, 1_000_045.0);
        assert_eq!(
            acknowledged,
            [
                "Turn the calibration switches OFF",
                "Are the calibration switches OFF?"
            ]
        );
        assert_eq!(core.obs.as_ref().unwrap().state, State::Bg1);
        assert_eq!(core.message, "Taking background data!!!");

        // Runs unattended through the data, then asks for the second calibration
        let acknowledged = run(&mut core, &mut now, 1_000_300.0);
        assert_eq!(
            acknowledged,
            [
                "Set frequency to 1319.5MHz",
                "Is the frequency set to 1319.5MHz?",
                "Turn the calibration switches ON",
                "Are the calibration switches ON?",
                "Turn the calibration switches OFF",
                "Are the calibration switches OFF?",
            ]
        );
        run(&mut core, &mut now, 1_000_330.0);
        assert!(core.obs.is_none());
        assert_eq!(core.message, "Spectrum complete!!!");
        assert_eq!(core.readout.progress_label, "n/a");

        let contents = fs::read_to_string(dir.join("spectrum_a.md1")).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.iter().filter(|line| **line == "*").count(), 6);
        assert_eq!(lines[lines.len() - 5], "TELESCOPE: The Mighty Forty");
        // 3Hz for 20s of calibration, then the separator
        let first_separator = lines.iter().position(|line| *line == "*").unwrap();
        assert!(
            (57..=63).contains(&(first_separator / 3)),
            "{first_separator}"
        );
        // Roughly 6Hz for the 180s from the start time until the end time
        let separators: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| **l == "*")
            .map(|(i, _)| i)
            .collect();
        let data_points = (separators[2] - separators[1] - 1) / 3;
        assert!((1050..=1110).contains(&data_points), "{data_points}");
        fs::remove_dir_all(&dir).unwrap();
    }
}

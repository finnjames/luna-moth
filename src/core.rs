//! Everything Luna Moth does that isn't drawing: reading the instruments, keeping time,
//! and walking the user through observations. Runs on its own thread so that data
//! acquisition never depends on the window being repainted.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::beep::Beeper;
use crate::clock::{SuperClock, TimerId};
use crate::dataq::{DataQ, SignalDatum, SimParams};
use crate::deccalc::{DecCalError, DecCalc};
use crate::declinometer::Declinometer;
use crate::logtask::{LogTask, Status};
use crate::observation::{Comm, DataPoint, ObsType, Observation, State};

// Basic time
pub const BASE_PERIOD: f64 = 10.0; // ms = 100Hz
const GUI_UPDATE_PERIOD: f64 = 1000.0; // ms = 1Hz

/// The longest stretch of time, in sidereal seconds, that the stripchart can display
pub const MAX_STRIPCHART_SECONDS: f64 = 120.0;

/// How long an instrument can go quiet before it counts as not responding
const STALE_AFTER: f64 = 2.0; // s

/// The readings coming in from one instrument
#[derive(Debug)]
struct Feed<T> {
    latest: Option<T>,
    last_reading_time: f64,
    last_error: Option<String>,
    /// Whether the instrument has stopped responding, so `latest` can't be trusted
    is_stale: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum FeedChange {
    /// Stopped responding, with the error that it gave, if any
    WentStale(Option<String>),
    Recovered,
}

impl<T: Copy> Feed<T> {
    fn new(current_time: f64) -> Self {
        Self {
            latest: None,
            last_reading_time: current_time,
            last_error: None,
            is_stale: false,
        }
    }

    /// Take in whatever the instrument had this tick. Returns whether that was a new
    /// reading, and whether the instrument just stopped or started responding.
    fn update(
        &mut self,
        reading: io::Result<Option<T>>,
        current_time: f64,
    ) -> (bool, Option<FeedChange>) {
        let failed = match reading {
            Ok(Some(reading)) => {
                self.latest = Some(reading);
                self.last_reading_time = current_time;
                self.last_error = None;
                let was_stale = std::mem::replace(&mut self.is_stale, false);
                return (true, was_stale.then_some(FeedChange::Recovered));
            }
            Ok(None) => false,
            Err(err) => {
                self.last_error = Some(err.to_string());
                true
            }
        };
        // An error means it's gone right now; silence takes a while to be sure about
        let quiet_for_too_long = current_time - self.last_reading_time > STALE_AFTER;
        if !self.is_stale && (failed || quiet_for_too_long) {
            self.is_stale = true;
            return (false, Some(FeedChange::WentStale(self.last_error.clone())));
        }
        (false, None)
    }

    /// The latest reading, as long as the instrument is still responding
    fn fresh(&self) -> Option<T> {
        self.latest.filter(|_| !self.is_stale)
    }
}

/// How often to beep while the user has something to do
const PROMPT_BEEP_PERIOD: f64 = 10.0; // s

/// Something the user has to do before the observation can go on
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub instructions: Vec<String>,
    /// What the button that confirms it's all been done says
    pub button: String,
    action: PromptAction,
}

/// What to do once the user has confirmed a prompt
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromptAction {
    StartCalibration,
    StartBackground,
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
    /// Where observations write their data files
    pub data_dir: PathBuf,

    // Most recent data
    dec_calc: DecCalc,
    dec_is_calibrated: bool,
    pub dec_cal_path: PathBuf,
    pub current_dec: f64,
    dataq_feed: Feed<SignalDatum>,
    /// Raw (uncalibrated) declinometer readings
    declinometer_feed: Feed<f64>,
    /// The latest from both instruments. `None` if either one isn't responding, so
    /// that old readings never get recorded as if they were new.
    current_data_point: Option<DataPoint>,
    /// Recent data points, oldest first
    pub stripchart: VecDeque<DataPoint>,

    // DataQ communication interpretation
    previous_transmission: Option<Comm>,

    // "Console" output
    message_log: Vec<LogTask>,
    pub message: String,
    pub readout: Readout,

    // Prompts
    prompt: Option<Prompt>,
    /// How many prompts have been raised so far
    pub prompts_raised: u64,
    last_prompt_beep_time: f64,

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
            data_dir: data_dir.to_owned(),
            dec_calc: DecCalc::new(),
            dec_is_calibrated: false,
            dec_cal_path: dec_cal_path.to_owned(),
            current_dec: 0.0,
            dataq_feed: Feed::new(current_time),
            declinometer_feed: Feed::new(current_time),
            current_data_point: None,
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
            },
            prompt: None,
            prompts_raised: 0,
            last_prompt_beep_time: 0.0,
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
        core.reload_dec_cal();
        if !core.dec_is_calibrated {
            core.log_warning("Dec must be calibrated");
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
        let dataq_reading = self.dataq.read_latest(current_time, &self.sim); // Get data from DAQ
        let declinometer_reading = self.declinometer.read_latest(current_time, &self.sim); // Get data from declinometer
        self.tick_with(dataq_reading, declinometer_reading, current_time);
    }

    /// A tick, given whatever the instruments had to say for themselves
    fn tick_with(
        &mut self,
        dataq_reading: io::Result<Option<SignalDatum>>,
        declinometer_reading: io::Result<Option<f64>>,
        current_time: f64,
    ) {
        let sidereal_timestamp = self.clock.sidereal_seconds(current_time);
        let (new_signal, change) = self.dataq_feed.update(dataq_reading, current_time);
        self.report_feed_change("DataQ", change, current_time);
        let (new_dec, change) = self
            .declinometer_feed
            .update(declinometer_reading, current_time);
        self.report_feed_change("Declinometer", change, current_time);
        if let (true, Some(reading)) = (new_dec, self.declinometer_feed.latest) {
            self.current_dec = self.dec_calc.calculate_declination(reading);
        }

        // A data point is the latest from each instrument. They report on their own
        // schedules, so there's a new one whenever either of them has something new.
        match (self.dataq_feed.fresh(), self.declinometer_feed.fresh()) {
            (Some(signal), Some(_)) => {
                if new_signal || new_dec {
                    let data_point = DataPoint {
                        timestamp: sidereal_timestamp, // RA
                        dec: self.current_dec,         // Dec
                        a: signal.a,                   // Channel A
                        b: signal.b,                   // Channel B
                    };
                    self.current_data_point = Some(data_point);
                    self.stripchart.push_back(data_point);
                }
            }
            _ => self.current_data_point = None,
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

        // Keep reminding the user that there's something to do
        if self.prompt.is_some() && current_time - self.last_prompt_beep_time >= PROMPT_BEEP_PERIOD
        {
            self.last_prompt_beep_time = current_time;
            self.beep(current_time);
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

        // The first calibration comes before any data has been taken
        let is_first_calibration = obs.state == State::Off;
        let target_dec = obs.target_dec();
        let transmission = obs.communicate(self.current_data_point.as_ref(), current_time);
        let obs_type = obs.obs_type;
        if let Some(err) = obs.take_io_error() {
            self.log_warning(&format!("Couldn't write data: {err}"));
        }

        if Some(transmission) != self.previous_transmission {
            if transmission == Comm::StartCal {
                let mut instructions = Vec::new();
                if is_first_calibration {
                    instructions.push(format!(
                        "Move the telescope to {target_dec:.1}° declination"
                    ));
                } else if obs_type == ObsType::Survey {
                    instructions.push("STOP the telescope".to_owned());
                }
                if obs_type == ObsType::Spectrum {
                    instructions.push("Set frequency to 1319.5MHz".to_owned());
                }
                instructions.push("Turn the calibration switches ON".to_owned());
                self.raise_prompt(
                    Prompt {
                        instructions,
                        button: "Start calibration".to_owned(),
                        action: PromptAction::StartCalibration,
                    },
                    current_time,
                );
            } else if transmission == Comm::StartBg {
                self.raise_prompt(
                    Prompt {
                        instructions: vec!["Turn the calibration switches OFF".to_owned()],
                        button: "Start background".to_owned(),
                        action: PromptAction::StartBackground,
                    },
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

        self.update_fps();
        if let Some(point) = &self.current_data_point {
            self.readout.channel_a = format!("{:.4}V", point.a);
            self.readout.channel_b = format!("{:.4}V", point.b);
        }
    }

    /// How far along the observation's current state is, from 0 to 1, and a countdown
    /// until (or count up since) the state is due to end, e.g. "T-01:30"
    pub fn obs_progress(&self, current_time: f64) -> Option<(f32, String)> {
        let (start_time, end_time) = self.obs.as_ref()?.state_time_interval;
        if end_time <= 0.0 {
            return None;
        }
        let progress = if current_time >= end_time {
            1.0
        } else if current_time > start_time && start_time > 0.0 {
            ((current_time - start_time) / (end_time - start_time)) as f32
        } else {
            0.0
        };
        Some((progress, countdown_label(end_time - current_time)))
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
        let result = self.dec_calc.load_dec_cal(&self.dec_cal_path);
        self.dec_is_calibrated = result.is_ok();
        match result {
            Ok(()) | Err(DecCalError::NotFound) => {}
            Err(err) => self.log_warning(&format!("Dec cal is unusable: {err}")),
        }
    }

    /// Whether declinations come from a real calibration, as opposed to the placeholder
    pub fn dec_is_calibrated(&self) -> bool {
        self.dec_is_calibrated
    }

    /// Tell the user when an instrument stops or starts responding
    fn report_feed_change(
        &mut self,
        instrument: &str,
        change: Option<FeedChange>,
        current_time: f64,
    ) {
        match change {
            Some(FeedChange::WentStale(error)) => {
                let error = error.map_or(String::new(), |error| format!(" ({error})"));
                self.log_warning(&format!("{instrument} stopped responding{error}"));
                let message = format!("{instrument} stopped responding!!!");
                self.message(&message, true, false, current_time);
            }
            Some(FeedChange::Recovered) => {
                let message = format!("{instrument} is responding again");
                self.message(&message, true, true, current_time);
            }
            None => {}
        }
    }

    /// The instruments that have stopped responding. Nothing gets recorded until
    /// they're all back.
    pub fn instruments_not_responding(&self) -> Vec<&'static str> {
        let mut instruments = Vec::new();
        if self.dataq_feed.is_stale {
            instruments.push("DataQ");
        }
        if self.declinometer_feed.is_stale {
            instruments.push("Declinometer");
        }
        instruments
    }

    /// Most recent raw (uncalibrated) declinometer reading, if it's still responding
    pub fn latest_declinometer_reading(&self) -> Option<f64> {
        self.declinometer_feed.fresh()
    }

    /// Whether the channel voltages are made up because there's no DataQ
    pub fn dataq_is_simulated(&self) -> bool {
        self.dataq.is_simulated()
    }

    /// Whether the declination is made up because there's no declinometer
    pub fn declinometer_is_simulated(&self) -> bool {
        self.declinometer.is_simulated()
    }

    /// Put an observation on the schedule. It takes it from there, prompting the user
    /// when it needs something.
    pub fn start_observation(&mut self, obs: Observation, current_time: f64) {
        let message = format!("{} scheduled", obs.obs_type.capitalized());
        self.obs = Some(obs);
        self.prompt = None;
        self.previous_transmission = None;
        self.message(&message, true, true, current_time);
    }

    /// End the observation early, keeping whatever data it has taken so far
    pub fn stop_observation(&mut self, current_time: f64) {
        let Some(mut obs) = self.obs.take() else {
            return;
        };
        obs.stop(current_time);
        if let Some(err) = obs.take_io_error() {
            self.log_warning(&format!("Couldn't write data: {err}"));
        }
        self.prompt = None;
        self.previous_transmission = None;
        let message = format!("{} stopped early", obs.obs_type.capitalized());
        self.message(&message, true, true, current_time);
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

    /// What the user needs to do right now, if anything
    pub fn prompt(&self) -> Option<&Prompt> {
        self.prompt.as_ref()
    }

    /// Ask the user to do something
    fn raise_prompt(&mut self, prompt: Prompt, current_time: f64) {
        self.log(&prompt.instructions.join(", "), current_time);
        self.beep(current_time);
        self.last_prompt_beep_time = current_time;
        self.prompts_raised += 1;
        self.prompt = Some(prompt);
    }

    /// The user did everything that the prompt asked for
    pub fn confirm_prompt(&mut self, current_time: f64) {
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        self.clock.reset_all_timer_anchors(current_time);
        self.next_obs_state(current_time);
        match prompt.action {
            PromptAction::StartCalibration => {
                self.message("Taking calibration data!!!", true, true, current_time);
            }
            PromptAction::StartBackground => {
                self.message("Taking background data!!!", true, true, current_time);
            }
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

    /// Tick at 100Hz until `until`, confirming every prompt as soon as it comes up.
    /// Returns the instructions of each prompt that was confirmed.
    fn run(core: &mut Core, current_time: &mut f64, until: f64) -> Vec<Vec<String>> {
        let mut confirmed = Vec::new();
        while *current_time < until {
            core.tick(*current_time);
            if let Some(prompt) = core.prompt() {
                confirmed.push(prompt.instructions.clone());
                core.confirm_prompt(*current_time);
            }
            *current_time += BASE_PERIOD / 1000.0;
        }
        confirmed
    }

    fn spectrum(dir: &Path, start_time: f64) -> Observation {
        let mut obs = Observation::new(ObsType::Spectrum);
        obs.set_name("spectrum", dir).unwrap();
        obs.set_start_and_end_times(start_time, start_time + 180.0);
        obs.set_dec(12.0, 65535.0).unwrap();
        obs.set_data_freq(6);
        obs
    }

    #[test]
    fn formats_countdowns() {
        assert_eq!(countdown_label(5.2), "T-05");
        assert_eq!(countdown_label(90.0), "T-01:30");
        assert_eq!(countdown_label(3605.0), "T-01:00:05");
        assert_eq!(countdown_label(-59.6), "T+01:00");
    }

    #[test]
    fn starts_up_with_simulated_instruments_and_no_dec_calibration() {
        let dir = temp_data_dir("startup");
        let mut now = 1_000_000.0;
        let mut core = test_core(now, &dir);
        assert_eq!(core.message, "Ready!!!");
        assert!(core.dataq_is_simulated() && core.declinometer_is_simulated());
        assert!(!core.dec_is_calibrated());
        assert_eq!(core.prompt(), None);
        let lines = core.console_lines(100);
        assert_eq!(lines[0], ">>> LUNA MOTH");
        assert_eq!(lines[1], ">>> Initializing... done!");
        assert!(lines[2].ends_with("] DataQ not found, simulating data"));
        assert!(lines[3].ends_with("] Declinometer not found, simulating data"));
        assert_eq!(lines[4], "[WARNING!] Dec must be calibrated");
        assert!(lines[5].ends_with("] Ready!!!"));
        assert_eq!(core.console_lines(2).len(), 2);

        assert!(run(&mut core, &mut now, 1_000_002.0).is_empty());
        assert!(core.stripchart.len() > 100);
        assert_ne!(core.readout.ra, "00:00:00");
        assert_eq!(core.obs_progress(now), None);

        // A calibration that's saved later gets picked up
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("dec-cal.txt"), "-1\n1\n").unwrap();
        core.reload_dec_cal();
        assert!(core.dec_is_calibrated());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn runs_a_whole_spectrum() {
        let dir = temp_data_dir("core-spectrum");
        let mut now = 1_000_000.0;
        let mut core = test_core(now, &dir);
        run(&mut core, &mut now, 1_000_001.0);

        // Start in 80s; the user gets prompted 70s (cal + bg + 30s buffer) before then
        core.start_observation(spectrum(&dir, now + 80.0), now);
        assert_eq!(core.message, "Spectrum scheduled");
        assert!(run(&mut core, &mut now, 1_000_009.0).is_empty());
        assert_eq!(core.obs.as_ref().unwrap().state, State::Off);
        let (progress, countdown) = core.obs_progress(now).unwrap();
        assert_eq!(progress, 0.0);
        assert!(countdown.starts_with("T-"), "{countdown}");

        let confirmed = run(&mut core, &mut now, 1_000_030.0);
        assert_eq!(
            confirmed,
            [[
                "Move the telescope to 12.0° declination",
                "Set frequency to 1319.5MHz",
                "Turn the calibration switches ON"
            ]]
        );
        assert_eq!(core.obs.as_ref().unwrap().state, State::Cal1);
        assert_eq!(core.message, "Taking calibration data!!!");
        let (progress, _) = core.obs_progress(now).unwrap();
        assert!(progress > 0.5 && progress < 1.0, "{progress}");

        let confirmed = run(&mut core, &mut now, 1_000_045.0);
        assert_eq!(confirmed, [["Turn the calibration switches OFF"]]);
        assert_eq!(core.obs.as_ref().unwrap().state, State::Bg1);
        assert_eq!(core.message, "Taking background data!!!");

        // Waits for the start time, runs unattended through the data, then asks for the
        // second calibration
        run(&mut core, &mut now, 1_000_060.0);
        assert_eq!(core.obs.as_ref().unwrap().state, State::Waiting);
        let confirmed = run(&mut core, &mut now, 1_000_300.0);
        assert_eq!(
            confirmed,
            [
                vec![
                    "Set frequency to 1319.5MHz",
                    "Turn the calibration switches ON"
                ],
                vec!["Turn the calibration switches OFF"],
            ]
        );
        run(&mut core, &mut now, 1_000_330.0);
        assert!(core.obs.is_none());
        assert_eq!(core.message, "Spectrum complete!!!");
        assert_eq!(core.obs_progress(now), None);

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

    #[test]
    fn waits_for_the_user_and_keeps_beeping() {
        let dir = temp_data_dir("core-prompt");
        let mut now = 1_000_000.0;
        let mut core = test_core(now, &dir);
        core.start_observation(spectrum(&dir, now + 10.0), now);

        // The start is so soon that the prompt comes up right away
        while core.prompt().is_none() {
            core.tick(now);
            now += 0.01;
        }
        assert_eq!(core.prompts_raised, 1);
        assert_eq!(core.prompt().unwrap().button, "Start calibration");
        let raised = now;
        let first_beep = core.last_prompt_beep_time;
        // Nothing happens until the user confirms, however late that is
        while now < raised + 60.0 {
            core.tick(now);
            now += 0.01;
        }
        assert_eq!(core.obs.as_ref().unwrap().state, State::Off);
        assert_eq!(core.prompts_raised, 1);
        assert!(core.last_prompt_beep_time > first_beep + 45.0);
        let (progress, countdown) = core.obs_progress(now).unwrap();
        assert_eq!(progress, 1.0);
        assert!(countdown.starts_with("T+"), "{countdown}");

        core.confirm_prompt(now);
        assert_eq!(core.prompt(), None);
        assert_eq!(core.obs.as_ref().unwrap().state, State::Cal1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stops_an_observation_early() {
        let dir = temp_data_dir("core-stop");
        let mut now = 1_000_000.0;
        let mut core = test_core(now, &dir);
        core.start_observation(spectrum(&dir, now + 10.0), now);
        run(&mut core, &mut now, 1_000_010.0);
        assert_eq!(core.obs.as_ref().unwrap().state, State::Cal1);

        core.stop_observation(now);
        assert!(core.obs.is_none());
        assert_eq!(core.prompt(), None);
        assert_eq!(core.message, "Spectrum stopped early");
        // The data so far is still there, and the file is finished off properly
        let contents = fs::read_to_string(dir.join("spectrum_a.md1")).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert!(lines.len() > 30);
        assert_eq!(lines[lines.len() - 5], "TELESCOPE: The Mighty Forty");
        assert_eq!(lines[lines.len() - 6], "*");

        // Stopping while there's a prompt up doesn't get the next observation stuck
        core.start_observation(spectrum(&dir, now + 10.0), now);
        while core.prompt().is_none() {
            core.tick(now);
            now += 0.01;
        }
        core.stop_observation(now);
        core.start_observation(spectrum(&dir, now + 10.0), now);
        let confirmed = run(&mut core, &mut now, 1_000_025.0);
        assert_eq!(confirmed.len(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn feeds_go_stale_on_errors_and_long_silences() {
        let mut feed = Feed::new(100.0);
        assert_eq!(feed.fresh(), None);
        assert_eq!(feed.update(Ok(Some(1.5)), 100.0), (true, None));
        assert_eq!(feed.fresh(), Some(1.5));

        // Instruments don't have something new every tick
        assert_eq!(feed.update(Ok(None), 101.9), (false, None));
        assert_eq!(feed.fresh(), Some(1.5));
        // ...but they can't stay quiet forever
        let change = FeedChange::WentStale(None);
        assert_eq!(feed.update(Ok(None), 102.1), (false, Some(change)));
        assert_eq!(feed.fresh(), None);
        assert_eq!(feed.update(Ok(None), 103.0), (false, None));
        assert_eq!(
            feed.update(Ok(Some(2.5)), 104.0),
            (true, Some(FeedChange::Recovered))
        );
        assert_eq!(feed.fresh(), Some(2.5));

        // An error counts right away
        let change = FeedChange::WentStale(Some("unplugged".to_owned()));
        let unplugged = || Err(io::Error::other("unplugged"));
        assert_eq!(feed.update(unplugged(), 104.01), (false, Some(change)));
        assert_eq!(feed.update(unplugged(), 104.02), (false, None));
        assert_eq!(feed.fresh(), None);
    }

    #[test]
    fn holds_the_latest_reading_from_each_instrument() {
        let dir = temp_data_dir("core-hold");
        let mut now = 1_000_000.0;
        let mut core = test_core(now, &dir);
        core.clear_stripchart();

        // The instruments never have something new on the same tick
        for tick in 0..200 {
            let signal = SignalDatum {
                a: f64::from(tick),
                b: 0.0,
            };
            if tick % 2 == 0 {
                core.tick_with(Ok(Some(signal)), Ok(None), now);
            } else {
                core.tick_with(Ok(None), Ok(Some(0.3)), now);
            }
            now += 0.01;
        }
        // There's a data point for every tick but the first, when only one had reported
        assert_eq!(core.stripchart.len(), 199);
        let latest = core.current_data_point.unwrap();
        assert_eq!(latest.a, 198.0);
        assert_eq!(latest.dec, core.current_dec);
        assert_eq!(core.latest_declinometer_reading(), Some(0.3));
        assert!(core.instruments_not_responding().is_empty());
    }

    #[test]
    fn stops_recording_when_an_instrument_stops_responding() {
        let dir = temp_data_dir("core-stale");
        let mut now = 1_000_000.0;
        let mut core = test_core(now, &dir);
        core.start_observation(spectrum(&dir, now + 60.0), now);
        run(&mut core, &mut now, 1_000_005.0);
        assert_eq!(core.obs.as_ref().unwrap().state, State::Cal1);
        let lines_written = || {
            let contents = fs::read_to_string(dir.join("spectrum_a.md1")).unwrap();
            contents.lines().count()
        };
        let before = lines_written();
        assert!(before > 0);

        // The DataQ gets unplugged for five seconds
        let unplugged = now;
        while now < unplugged + 5.0 {
            core.tick_with(Err(io::Error::other("unplugged")), Ok(Some(0.3)), now);
            now += 0.01;
        }
        assert_eq!(core.instruments_not_responding(), ["DataQ"]);
        assert_eq!(core.message, "DataQ stopped responding!!!");
        let warning = "[WARNING!] DataQ stopped responding (unplugged)";
        assert!(core.console_lines(100).contains(&warning.to_owned()));
        // Nothing was written in the meantime, and the dec isn't being passed off as new
        assert_eq!(lines_written(), before);
        assert_eq!(core.current_data_point, None);
        assert_eq!(core.latest_declinometer_reading(), Some(0.3));
        // The observation itself carries on
        assert_eq!(core.obs.as_ref().unwrap().state, State::Cal1);

        run(&mut core, &mut now, unplugged + 7.0);
        assert!(core.instruments_not_responding().is_empty());
        assert_eq!(core.message, "DataQ is responding again");
        assert!(lines_written() > before);
        fs::remove_dir_all(&dir).unwrap();
    }
}

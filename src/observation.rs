use std::io;
use std::path::Path;

use chrono::{DateTime, Local};

use crate::data_file::DataFile;

/// Each data point taken (timestamp, dec, a, b)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DataPoint {
    pub timestamp: f64,
    pub dec: f64,
    pub a: f64,
    pub b: f64,
}

/// What the observation needs from whoever is running it
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Comm {
    NoAction,
    Beep,
    StartCal,
    StartBg,
    Finished,
    // Extra
    StartWait,
    StartData,
    SendTelNorth,
    SendTelSouth,
    EndSendTel,
    FinishingSweep,
}

/// Observation type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObsType {
    /// A 'drift scan' across a source.
    Scan,
    /// 2D 'nodding' map of a region of the radio sky.
    Survey,
    /// Spectrums are similar to Scans, except that they measure many frequencies.
    Spectrum,
}

impl ObsType {
    pub fn lower(self) -> &'static str {
        match self {
            Self::Scan => "scan",
            Self::Survey => "survey",
            Self::Spectrum => "spectrum",
        }
    }

    pub fn capitalized(self) -> &'static str {
        match self {
            Self::Scan => "Scan",
            Self::Survey => "Survey",
            Self::Spectrum => "Spectrum",
        }
    }

    fn file_extension(self) -> &'static str {
        match self {
            Self::Scan | Self::Spectrum => "md1",
            Self::Survey => "md2",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Off,
    Cal1,
    Bg1,
    Data,
    Cal2,
    Bg2,
    Done,
    Waiting, // Extra
}

/// What the user keyed in to create an observation, kept for later display
#[derive(Clone, Debug, PartialEq)]
pub struct ObsRecord {
    pub start_time: [u32; 3],
    pub end_time: [u32; 3],
    pub min_dec: String,
    pub max_dec: String,
    pub data_acquisition_rate_value: u32,
    pub file_name_value: String,
}

#[derive(Debug)]
struct Files {
    a: DataFile,
    b: DataFile,
    comp: DataFile,
}

/// Seconds set aside before the first calibration for the user to get things ready
pub const USER_BUFFER: f64 = 30.0;

/// The names of the data files (.md1 or .md2, based on observation type) of an
/// observation called `name`
pub fn data_file_names(obs_type: ObsType, name: &str) -> [String; 3] {
    let extension = obs_type.file_extension();
    ["a", "b", "comp"].map(|channel| format!("{name}_{channel}.{extension}"))
}

/// One of the three types of observation: Scan, Survey, or Spectrum.
///
/// To interact with an observation, first set its properties using the `set_xxx()`
/// API. Then, call `communicate()` every `1 / freq` seconds. When that returns a
/// message other than `Comm::NoAction`, prompt the user for the appropriate action and
/// call `next()` once to proceed into the next stage. An observation is finished when
/// `communicate()` returns `Comm::Finished`.
#[derive(Debug)]
pub struct Observation {
    pub name: String,
    pub obs_type: ObsType,
    composite: bool,

    // Parameters
    bg_dur: f64,
    cal_dur: f64,

    // Calibration and BG share the same sampling frequency, while the data has its own
    // sampling frequency. Only the data frequency is user-editable. When running the
    // program, the sampling frequency is automatically set by the current state and is
    // stored in `freq`.
    cal_freq: u32,
    data_freq: u32,

    /// Sampling frequency in Hz. Set accordingly in each state.
    pub freq: u32,

    pub state: State,

    /// Solar start and end of the current state, for showing progress. Negative means
    /// not applicable.
    pub state_time_interval: (f64, f64),

    // Info
    /// When data collection is scheduled to begin
    start_time: f64,
    /// When data collection is scheduled to end, then when the observation ended
    end_time: f64,
    /// When the observation (i.e. the first calibration) actually began
    obs_start: f64,
    pub min_dec: f64, // If only one dec, this is it
    pub max_dec: f64,

    /// Only surveys have sweeps
    pub sweep_number: Option<u32>,

    // File interface
    files: Option<Files>,
    io_error: Option<io::Error>,

    /// Record keeping for later display
    pub input_record: Option<ObsRecord>,

    // Temporary bookkeeping
    cal_start: f64,
    bg_start: f64,

    // Survey: whether the telescope is outside the declination range
    outside: bool,

    // Spectrum: how often to remind the user to change the radio frequency (e.g.
    // 1319.5 MHz). This is not the sampling frequency!
    interval: f64,
    freq_time: Option<f64>,
    timing_margin: f64,
}

impl Observation {
    pub fn new(obs_type: ObsType) -> Self {
        let (cal_dur, bg_dur, cal_freq, data_freq) = match obs_type {
            ObsType::Scan | ObsType::Survey => (60.0, 60.0, 1, 6),
            ObsType::Spectrum => (20.0, 20.0, 3, 10),
        };
        Self {
            name: "Untitled".to_owned(),
            obs_type,
            composite: false,
            bg_dur,
            cal_dur,
            cal_freq,
            data_freq,
            freq: cal_freq,
            state: State::Off,
            state_time_interval: (-1.0, -1.0),
            start_time: 0.0,
            end_time: 0.0,
            obs_start: 0.0,
            min_dec: 0.0,
            max_dec: 0.0,
            sweep_number: (obs_type == ObsType::Survey).then_some(1),
            files: None,
            io_error: None,
            input_record: None,
            cal_start: 0.0,
            bg_start: 0.0,
            outside: true,
            interval: 1.0,
            freq_time: None,
            timing_margin: 0.97,
        }
    }

    // Settings API
    pub fn set_start_and_end_times(&mut self, start_time: f64, end_time: f64) {
        self.start_time = start_time;
        self.end_time = end_time;
    }

    pub fn set_dec(&mut self, min_dec: f64, max_dec: f64) -> Result<(), String> {
        if max_dec < min_dec {
            return Err("Max dec must be greater min dec".to_owned());
        }
        self.min_dec = min_dec;
        self.max_dec = max_dec;
        Ok(())
    }

    /// Name the observation and create its data files (.md1 or .md2, based on
    /// observation type) in `dir`
    pub fn set_name(&mut self, name: &str, dir: &Path) -> io::Result<()> {
        let [a, b, comp] = data_file_names(self.obs_type, name);
        self.files = Some(Files {
            a: DataFile::new(dir, &a)?,
            b: DataFile::new(dir, &b)?,
            comp: DataFile::new(dir, &comp)?,
        });
        self.name = name.to_owned();
        Ok(())
    }

    /// How long the calibration and background before the data take
    pub fn preparation_duration(&self) -> f64 {
        self.cal_dur + self.bg_dur
    }

    /// Where the telescope needs to be pointing when the observation begins. Surveys
    /// begin below their range so that the first sweep covers all of it.
    pub fn target_dec(&self) -> f64 {
        match self.obs_type {
            ObsType::Survey => self.min_dec - 2.0,
            ObsType::Scan | ObsType::Spectrum => self.min_dec,
        }
    }

    pub fn set_data_freq(&mut self, data_freq: u32) {
        self.data_freq = data_freq;
    }

    // Communication API
    pub fn communicate(&mut self, data_point: Option<&DataPoint>, timestamp: f64) -> Comm {
        let user_start_time = self.start_time - self.preparation_duration() - USER_BUFFER;

        match self.state {
            State::Off => {
                self.state_time_interval = (-1.0, user_start_time);
                if timestamp < user_start_time {
                    // A 30-second buffer for user actions
                    Comm::NoAction
                } else {
                    Comm::StartCal
                }
            }
            State::Cal1 | State::Cal2 => {
                if (timestamp - self.cal_start).floor() < self.cal_dur {
                    self.write_data(data_point);
                    Comm::NoAction
                } else {
                    Comm::StartBg
                }
            }
            State::Bg1 | State::Bg2 => {
                if (timestamp - self.bg_start).floor() < self.bg_dur {
                    self.write_data(data_point);
                    Comm::NoAction
                } else if self.state == State::Bg1 {
                    Comm::StartWait
                } else {
                    Comm::Finished
                }
            }
            State::Waiting => {
                if timestamp < self.start_time {
                    Comm::NoAction
                } else {
                    Comm::StartData
                }
            }
            State::Data => {
                if timestamp < self.start_time {
                    Comm::NoAction
                } else if timestamp < self.end_time {
                    self.data_logic(data_point, timestamp)
                } else if self.obs_type == ObsType::Survey
                    && !matches!(
                        self.data_logic(data_point, timestamp),
                        Comm::SendTelNorth | Comm::SendTelSouth
                    )
                {
                    // Surveys keep going until the telescope leaves the dec range
                    Comm::FinishingSweep
                } else {
                    Comm::StartCal
                }
            }
            State::Done => Comm::Finished,
        }
    }

    /// This is the action API
    pub fn next(&mut self, current_time: f64) -> State {
        match self.state {
            State::Off => self.start_calibration_1(current_time),
            State::Cal1 => self.end_calibration_1(current_time),
            State::Bg1 => self.end_background_1(),
            State::Waiting => self.start_data(),
            State::Data => self.start_calibration_2(current_time),
            State::Cal2 => self.end_calibration_2(current_time),
            State::Bg2 => self.stop(current_time),
            State::Done => {}
        }
        self.state
    }

    fn start_calibration_1(&mut self, current_time: f64) {
        self.state = State::Cal1;
        self.obs_start = current_time;
        self.cal_start = current_time;
        self.freq = self.cal_freq;
        self.state_time_interval = (self.cal_start, self.cal_start + self.cal_dur);
    }

    fn end_calibration_1(&mut self, current_time: f64) {
        self.state = State::Bg1;
        self.write("*");
        self.bg_start = current_time;
        self.state_time_interval = (self.bg_start, self.bg_start + self.bg_dur);
    }

    fn end_background_1(&mut self) {
        self.state = State::Waiting;
        // Data collection doesn't begin until the starting RA comes around
        self.state_time_interval = (-1.0, self.start_time);
    }

    fn start_data(&mut self) {
        self.state = State::Data;
        self.write("*");
        self.freq = self.data_freq;
        self.state_time_interval = (self.start_time, self.end_time);
    }

    fn start_calibration_2(&mut self, current_time: f64) {
        self.state = State::Cal2;
        self.write("*");
        self.cal_start = current_time;
        self.freq = self.cal_freq;
        self.state_time_interval = (self.cal_start, self.cal_start + self.cal_dur);
    }

    fn end_calibration_2(&mut self, current_time: f64) {
        self.state = State::Bg2;
        self.write("*");
        self.bg_start = current_time;
        self.state_time_interval = (self.bg_start, self.bg_start + self.bg_dur);
    }

    /// End the observation and finish off its files. This is how every observation ends,
    /// whether it got through all of its states or not.
    pub fn stop(&mut self, current_time: f64) {
        if self.state == State::Done {
            return;
        }
        if self.state == State::Off {
            self.obs_start = current_time; // Stopped before it ever began
        }
        self.state = State::Done;
        self.end_time = current_time;
        self.state_time_interval = (-1.0, self.end_time);
        self.write("*");
        self.write("*");
        self.write_meta();
        self.files = None; // Close the files
    }

    /// This function defines the behavior of observation during the main data
    /// collection period. For example, in Survey this method is responsible for
    /// tracking if the dec is too high or too low. In Spectrum, this method tells
    /// the UI to beep to remind the user to change frequency.
    fn data_logic(&mut self, data_point: Option<&DataPoint>, current_time: f64) -> Comm {
        match self.obs_type {
            ObsType::Scan => {
                self.write_data(data_point);
                Comm::NoAction
            }
            ObsType::Survey => {
                let Some(point) = data_point else {
                    return Comm::NoAction;
                };
                if point.dec < self.min_dec || point.dec > self.max_dec {
                    if !self.outside {
                        self.write("*");
                    }
                    self.outside = true;
                    if point.dec < self.min_dec {
                        return Comm::SendTelNorth;
                    }
                    return Comm::SendTelSouth;
                } else if self.outside {
                    self.outside = false;
                    self.sweep_number = self.sweep_number.map(|n| n + 1);
                    return Comm::EndSendTel;
                }
                self.write_data(data_point);
                Comm::NoAction
            }
            ObsType::Spectrum => {
                self.write_data(data_point);
                match self.freq_time {
                    None => {
                        self.freq_time = Some(current_time);
                        Comm::NoAction
                    }
                    Some(freq_time)
                        if current_time - freq_time < self.timing_margin * self.interval =>
                    {
                        Comm::NoAction
                    }
                    Some(_) => {
                        self.freq_time = Some(current_time);
                        Comm::Beep
                    }
                }
            }
        }
    }

    /// The first file error since this was last called, if any
    pub fn take_io_error(&mut self) -> Option<io::Error> {
        self.io_error.take()
    }

    // Helpers

    fn write_to_files(&mut self, a: &str, b: &str) {
        let Some(files) = &mut self.files else {
            return;
        };
        let result = if self.composite {
            files.comp.write(a).and_then(|()| files.comp.write(b))
        } else {
            files.a.write(a).and_then(|()| files.b.write(b))
        };
        if let Err(err) = result {
            self.io_error.get_or_insert(err);
        }
    }

    fn write(&mut self, string: &str) {
        if self.composite {
            if let Some(files) = &mut self.files
                && let Err(err) = files.comp.write(string)
            {
                self.io_error.get_or_insert(err);
            }
        } else {
            self.write_to_files(string, string);
        }
    }

    fn write_data(&mut self, point: Option<&DataPoint>) {
        let Some(point) = point else {
            return;
        };
        self.write(&format!("{:.2}", point.timestamp));
        self.write(&format!("{:.4}", point.dec));
        self.write_to_files(&format!("{:.4}", point.a), &format!("{:.4}", point.b));
    }

    fn write_meta(&mut self) {
        self.write("TELESCOPE: The Mighty Forty");
        self.write(&format!("LOCAL START DATE: {}", get_date(self.obs_start)));
        self.write(&format!("LOCAL START TIME: {}", get_time(self.obs_start)));
        self.write(&format!("LOCAL STOP DATE: {}", get_date(self.end_time)));
        self.write(&format!("LOCAL STOP TIME: {}", get_time(self.end_time)));
    }
}

fn local_time(epoch_time: f64) -> DateTime<Local> {
    DateTime::from_timestamp(epoch_time as i64, 0)
        .unwrap_or_default()
        .with_timezone(&Local)
}

fn get_date(epoch_time: f64) -> String {
    local_time(epoch_time).format("%m/%d/%Y").to_string()
}

fn get_time(epoch_time: f64) -> String {
    local_time(epoch_time).format("%I:%M:%S %p").to_string()
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// A fresh, empty directory for a test to write data files into
    pub fn temp_data_dir(test_name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("luna-moth-{}-{test_name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn read_lines(dir: &Path, filename: &str) -> Vec<String> {
        let contents = fs::read_to_string(dir.join(filename)).unwrap();
        contents.lines().map(str::to_owned).collect()
    }

    fn point(dec: f64) -> DataPoint {
        DataPoint {
            timestamp: 12345.678,
            dec,
            a: 1.5,
            b: 2.25,
        }
    }

    fn new_obs(obs_type: ObsType, dir: &Path, start: f64, end: f64) -> Observation {
        let mut obs = Observation::new(obs_type);
        obs.set_name("test", dir).unwrap();
        obs.set_start_and_end_times(start, end);
        obs.set_dec(30.0, 40.0).unwrap();
        obs.set_data_freq(6);
        obs
    }

    #[test]
    fn rejects_inverted_dec_range() {
        let mut obs = Observation::new(ObsType::Survey);
        assert!(obs.set_dec(40.0, 30.0).is_err());
        assert!(obs.set_dec(0.0, 0.0).is_ok());
    }

    #[test]
    fn scan_walks_through_every_state() {
        let dir = temp_data_dir("scan");
        let mut obs = new_obs(ObsType::Scan, &dir, 1000.0, 1100.0);
        let p = point(35.0);

        // Nothing happens until 150s (cal + bg + 30s buffer) before the start
        assert_eq!(obs.communicate(Some(&p), 849.0), Comm::NoAction);
        assert_eq!(obs.state_time_interval, (-1.0, 850.0));
        assert_eq!(obs.communicate(Some(&p), 850.0), Comm::StartCal);
        assert_eq!(obs.next(860.0), State::Cal1);
        assert_eq!(obs.freq, 1);
        assert_eq!(obs.state_time_interval, (860.0, 920.0));

        assert_eq!(obs.communicate(Some(&p), 861.0), Comm::NoAction);
        assert_eq!(obs.communicate(Some(&p), 919.9), Comm::NoAction);
        assert_eq!(obs.communicate(Some(&p), 920.0), Comm::StartBg);
        assert_eq!(obs.next(925.0), State::Bg1);

        assert_eq!(obs.communicate(Some(&p), 926.0), Comm::NoAction);
        assert_eq!(obs.communicate(Some(&p), 985.0), Comm::StartWait);
        assert_eq!(obs.next(985.0), State::Waiting);

        // Even though calibration and background are done, wait for the starting RA
        assert_eq!(obs.state_time_interval, (-1.0, 1000.0));
        assert_eq!(obs.communicate(Some(&p), 986.0), Comm::NoAction);
        assert_eq!(obs.communicate(Some(&p), 999.9), Comm::NoAction);
        assert_eq!(obs.communicate(Some(&p), 1000.0), Comm::StartData);
        assert_eq!(obs.next(1000.0), State::Data);
        assert_eq!(obs.state_time_interval, (1000.0, 1100.0));
        assert_eq!(obs.freq, 6);

        assert_eq!(obs.communicate(Some(&p), 1099.0), Comm::NoAction);
        assert_eq!(obs.communicate(Some(&p), 1100.0), Comm::StartCal);
        assert_eq!(obs.next(1105.0), State::Cal2);
        assert_eq!(obs.freq, 1);

        assert_eq!(obs.communicate(Some(&p), 1106.0), Comm::NoAction);
        assert_eq!(obs.communicate(Some(&p), 1165.0), Comm::StartBg);
        assert_eq!(obs.next(1170.0), State::Bg2);

        assert_eq!(obs.communicate(Some(&p), 1171.0), Comm::NoAction);
        assert_eq!(obs.communicate(Some(&p), 1230.0), Comm::Finished);
        assert_eq!(obs.next(1230.0), State::Done);
        assert_eq!(obs.communicate(Some(&p), 1231.0), Comm::Finished);
        assert!(obs.take_io_error().is_none());

        let a = read_lines(&dir, "test_a.md1");
        let b = read_lines(&dir, "test_b.md1");
        // Six data points (timestamp, dec, value), six separators, five lines of metadata
        assert_eq!(a.len(), 6 * 3 + 6 + 5);
        assert_eq!(&a[..3], ["12345.68", "35.0000", "1.5000"]);
        assert_eq!(&b[..3], ["12345.68", "35.0000", "2.2500"]);
        assert_eq!(a[6], "*"); // After the two calibration points
        assert_eq!(a.iter().filter(|line| *line == "*").count(), 6);
        assert_eq!(a[a.len() - 5], "TELESCOPE: The Mighty Forty");
        assert!(a[a.len() - 4].starts_with("LOCAL START DATE: "));
        assert!(a[a.len() - 1].starts_with("LOCAL STOP TIME: "));
        assert!(read_lines(&dir, "test_comp.md1").is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    fn skip_to_data(obs: &mut Observation, start: f64) {
        obs.next(start - 150.0); // -> Cal1
        obs.next(start - 90.0); // -> Bg1
        obs.next(start - 30.0); // -> Waiting
        obs.next(start - 30.0); // -> Data
        assert_eq!(obs.state, State::Data);
    }

    #[test]
    fn survey_counts_sweeps_and_finishes_the_last_one() {
        let dir = temp_data_dir("survey");
        let mut obs = new_obs(ObsType::Survey, &dir, 1000.0, 1100.0);
        assert_eq!(obs.sweep_number, Some(1));
        skip_to_data(&mut obs, 1000.0);

        // Starts outside (below) the range
        assert_eq!(
            obs.communicate(Some(&point(28.0)), 1000.0),
            Comm::SendTelNorth
        );
        assert_eq!(
            obs.communicate(Some(&point(31.0)), 1001.0),
            Comm::EndSendTel
        );
        assert_eq!(obs.sweep_number, Some(2));
        assert_eq!(obs.communicate(Some(&point(35.0)), 1002.0), Comm::NoAction);
        assert_eq!(
            obs.communicate(Some(&point(41.0)), 1003.0),
            Comm::SendTelSouth
        );
        assert_eq!(
            obs.communicate(Some(&point(42.0)), 1004.0),
            Comm::SendTelSouth
        );
        assert_eq!(
            obs.communicate(Some(&point(39.0)), 1005.0),
            Comm::EndSendTel
        );
        assert_eq!(obs.sweep_number, Some(3));

        // Past the end time, the sweep in progress gets finished first
        assert_eq!(
            obs.communicate(Some(&point(35.0)), 1100.0),
            Comm::FinishingSweep
        );
        assert_eq!(obs.communicate(Some(&point(29.0)), 1101.0), Comm::StartCal);
        assert_eq!(obs.next(1101.0), State::Cal2);

        let a = read_lines(&dir, "test_a.md2");
        // "*" at end of cal 1, start of data, leaving the range twice, and start of cal 2
        assert_eq!(a.iter().filter(|line| *line == "*").count(), 5);
        assert_eq!(a.iter().filter(|line| *line == "35.0000").count(), 2);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn spectrum_beeps_every_second() {
        let dir = temp_data_dir("spectrum");
        let mut obs = new_obs(ObsType::Spectrum, &dir, 1000.0, 1180.0);
        assert_eq!(obs.freq, 3);
        assert_eq!(obs.sweep_number, None);

        // Calibration and background are only 20s each
        assert_eq!(obs.communicate(None, 929.0), Comm::NoAction);
        assert_eq!(obs.communicate(None, 930.0), Comm::StartCal);
        skip_to_data(&mut obs, 1000.0);
        assert_eq!(obs.freq, 6);

        let p = point(35.0);
        assert_eq!(obs.communicate(Some(&p), 1000.0), Comm::NoAction);
        assert_eq!(obs.communicate(Some(&p), 1000.5), Comm::NoAction);
        assert_eq!(obs.communicate(Some(&p), 1000.98), Comm::Beep);
        assert_eq!(obs.communicate(Some(&p), 1001.5), Comm::NoAction);
        assert_eq!(obs.communicate(Some(&p), 1002.0), Comm::Beep);
        assert_eq!(obs.communicate(Some(&p), 1180.0), Comm::StartCal);

        // Every data point is written, including the ones that beep
        let a = read_lines(&dir, "test_a.md1");
        assert_eq!(a.iter().filter(|line| *line == "1.5000").count(), 5);
        fs::remove_dir_all(&dir).unwrap();
    }
}

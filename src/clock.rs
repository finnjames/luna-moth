//! Clock for keeping track of the time and running things at different intervals

use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Local;

pub const SIDEREAL: f64 = 1.00273790935; // The number of sidereal seconds per second
pub const GB_LATITUDE: f64 = 38.437235; // North
pub const GB_LONGITUDE: f64 = -79.839835; // West (so negative)

pub const SECONDS_PER_DAY: f64 = 86400.0;

/// Current solar time in seconds since the Unix epoch
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Greenwich mean sidereal time, in degrees (Meeus, Astronomical Algorithms, eq. 12.4)
fn greenwich_mean_sidereal_degrees(unix_time: f64) -> f64 {
    let d = unix_time / SECONDS_PER_DAY + 2440587.5 - 2451545.0; // Days since J2000.0
    let t = d / 36525.0;
    280.46061837 + 360.98564736629 * d + 0.000387933 * t * t - t * t * t / 38710000.0
}

/// The equation of the equinoxes (apparent minus mean sidereal time), in degrees.
/// Low-precision USNO formula; good to roughly a hundredth of a second of time.
fn equation_of_equinoxes_degrees(unix_time: f64) -> f64 {
    let d = unix_time / SECONDS_PER_DAY + 2440587.5 - 2451545.0;
    let omega = (125.04 - 0.052954 * d).to_radians(); // Longitude of the Moon's node
    let l = (280.47 + 0.98565 * d).to_radians(); // Mean longitude of the Sun
    let epsilon = (23.4393 - 0.0000004 * d).to_radians(); // Obliquity
    let delta_psi_hours = -0.000319 * omega.sin() - 0.000024 * (2.0 * l).sin();
    delta_psi_hours * epsilon.cos() * 15.0
}

/// Local mean sidereal time in seconds since sidereal midnight
#[cfg(test)]
pub fn mean_sidereal_seconds(unix_time: f64, longitude: f64) -> f64 {
    (greenwich_mean_sidereal_degrees(unix_time) + longitude).rem_euclid(360.0) * 240.0
}

/// Local apparent sidereal time in seconds since sidereal midnight.
///
/// Treats the system clock (UTC) as UT1, so this can be off by up to 0.9s.
pub fn apparent_sidereal_seconds(unix_time: f64, longitude: f64) -> f64 {
    let degrees = greenwich_mean_sidereal_degrees(unix_time)
        + equation_of_equinoxes_degrees(unix_time)
        + longitude;
    degrees.rem_euclid(360.0) * 240.0
}

/// Convert hours, minutes, and seconds to a float of seconds
pub fn hms_to_seconds(hms: [u32; 3]) -> f64 {
    f64::from(hms[0]) * 3600.0 + f64::from(hms[1]) * 60.0 + f64::from(hms[2])
}

pub fn sidereal_to_solar(sidereal_seconds: f64) -> f64 {
    sidereal_seconds / SIDEREAL
}

/// Get timestamp suitable for file naming
pub fn time_slug() -> String {
    Local::now().format("%Y.%m.%d-%H.%M").to_string()
}

/// The local time of day of a solar time, as HH:MM:SS
pub fn local_time_of_day(epoch_time: f64) -> String {
    chrono::DateTime::from_timestamp(epoch_time as i64, 0)
        .unwrap_or_default()
        .with_timezone(&Local)
        .format("%H:%M:%S")
        .to_string()
}

/// Format a length of time in seconds as e.g. "45s", "12m 05s", or "1h 02m"
pub fn format_duration(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let (hours, minutes, seconds) = (total / 3600, total / 60 % 60, total % 60);
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

/// Handle to a timer owned by a [`SuperClock`]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimerId(usize);

/// A timer for syncing things that run at different, variable rates
#[derive(Debug)]
struct Timer {
    period: f64, // ms
    anchor_time: f64,
}

impl Timer {
    /// Whether at least one period has elapsed. Leftover time carries over to the next
    /// period, so the timer doesn't drift.
    fn run_if_appropriate(&mut self, current_time: f64) -> bool {
        if self.period <= 0.0 {
            return false;
        }

        let period = self.period / 1000.0;
        let elapsed = current_time - self.anchor_time;
        let elapsed_periods = (elapsed / period).floor();
        if elapsed_periods > 0.0 {
            let extra_time = elapsed - elapsed_periods * period;
            self.anchor_time = current_time - extra_time;
            return true;
        }
        false
    }

    fn set_period(&mut self, new_period: f64, current_time: f64) {
        if self.period != new_period {
            self.anchor_time = current_time;
        }
        self.period = new_period;
    }
}

/// Clock object for encapsulation; keeps track of the time(tm)
#[derive(Debug)]
pub struct SuperClock {
    timers: Vec<Timer>,
    /// Solar time of last calibration as epoch date
    starting_epoch_time: f64,
    /// Time, in seconds, since the sidereal midnight before last calibration
    starting_sidereal_time: f64,
}

impl SuperClock {
    pub fn new(current_time: f64) -> Self {
        let mut clock = Self {
            timers: Vec::new(),
            starting_epoch_time: 0.0,
            starting_sidereal_time: 0.0,
        };
        let current_ra = apparent_sidereal_seconds(current_time, GB_LONGITUDE);
        clock.calibrate_sidereal_time(current_ra, current_time);
        clock
    }

    pub fn calibrate_sidereal_time(&mut self, starting_sidereal_time: f64, current_time: f64) {
        self.starting_epoch_time = current_time;
        self.reset_all_timer_anchors(current_time);
        self.starting_sidereal_time = starting_sidereal_time.rem_euclid(SECONDS_PER_DAY);
    }

    /// Set a timer that comes due periodically. `period` is in milliseconds.
    pub fn add_timer(&mut self, period: f64, current_time: f64) -> TimerId {
        self.timers.push(Timer {
            period,
            anchor_time: current_time,
        });
        TimerId(self.timers.len() - 1)
    }

    /// Whether the timer is due to run. Returns true at most once per period.
    pub fn timer_is_due(&mut self, timer: TimerId, current_time: f64) -> bool {
        self.timers[timer.0].run_if_appropriate(current_time)
    }

    /// `period` is in milliseconds
    pub fn set_timer_period(&mut self, timer: TimerId, period: f64, current_time: f64) {
        self.timers[timer.0].set_period(period, current_time);
    }

    pub fn reset_all_timer_anchors(&mut self, current_time: f64) {
        for timer in &mut self.timers {
            timer.anchor_time = current_time;
        }
    }

    /// Sidereal seconds since the sidereal midnight before calibration
    pub fn sidereal_seconds(&self, current_time: f64) -> f64 {
        self.starting_sidereal_time + SIDEREAL * (current_time - self.starting_epoch_time)
    }

    /// Return an hours, minutes, seconds tuple of local sidereal time
    pub fn sidereal_tuple(&self, current_time: f64) -> [u32; 3] {
        let total = self
            .sidereal_seconds(current_time)
            .rem_euclid(SECONDS_PER_DAY) as u32;
        [total / 3600, total / 60 % 60, total % 60]
    }

    /// Return a string of HH:MM:SS formatted local sidereal time
    pub fn formatted_sidereal_time(&self, current_time: f64) -> String {
        let [hours, minutes, seconds] = self.sidereal_tuple(current_time);
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn unix(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> f64 {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s)
            .unwrap()
            .timestamp() as f64
    }

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(44.6), "45s");
        assert_eq!(format_duration(725.0), "12m 05s");
        assert_eq!(format_duration(3720.0), "1h 02m");
        assert_eq!(format_duration(-5.0), "0s");
    }

    #[test]
    fn mean_sidereal_time_matches_meeus() {
        // Meeus example 12.b: 1987 April 10, 19h21m00s UT -> 8h34m57.0896s
        let expected = 8.0 * 3600.0 + 34.0 * 60.0 + 57.0896;
        let actual = mean_sidereal_seconds(unix(1987, 4, 10, 19, 21, 0), 0.0);
        assert!((actual - expected).abs() < 0.001, "{actual} vs {expected}");
    }

    #[test]
    fn apparent_sidereal_time_matches_meeus() {
        // Meeus example 12.a: 1987 April 10, 0h UT -> 13h10m46.1351s apparent
        let expected = 13.0 * 3600.0 + 10.0 * 60.0 + 46.1351;
        let actual = apparent_sidereal_seconds(unix(1987, 4, 10, 0, 0, 0), 0.0);
        assert!((actual - expected).abs() < 0.02, "{actual} vs {expected}");
    }

    #[test]
    fn longitude_shifts_sidereal_time() {
        let t = unix(2025, 8, 14, 0, 47, 35);
        let greenwich = apparent_sidereal_seconds(t, 0.0);
        let green_bank = apparent_sidereal_seconds(t, GB_LONGITUDE);
        let shift = (greenwich - green_bank).rem_euclid(SECONDS_PER_DAY);
        assert!((shift - -GB_LONGITUDE * 240.0).abs() < 1e-6);
    }

    #[test]
    fn sidereal_clock_runs_fast() {
        let mut clock = SuperClock::new(1000.0);
        clock.calibrate_sidereal_time(3600.0, 1000.0);
        assert_eq!(clock.formatted_sidereal_time(1000.0), "01:00:00");
        assert!(
            (clock.sidereal_seconds(1000.0 + 86400.0) - (3600.0 + 86400.0 * SIDEREAL)).abs() < 1e-6
        );
        // Wraps around at sidereal midnight
        assert_eq!(clock.sidereal_tuple(1000.0 + 23.0 * 3600.0)[0], 0);
    }

    #[test]
    fn timer_fires_once_per_period_without_drift() {
        let mut clock = SuperClock::new(0.0);
        let timer = clock.add_timer(1000.0, 0.0);
        assert!(!clock.timer_is_due(timer, 0.5));
        assert!(clock.timer_is_due(timer, 1.25));
        assert!(!clock.timer_is_due(timer, 1.5));
        // The extra 0.25s carried over, so the next period ends at 2.0, not 2.25
        assert!(clock.timer_is_due(timer, 2.0));

        clock.set_timer_period(timer, 100.0, 2.0);
        assert!(!clock.timer_is_due(timer, 2.05));
        assert!(clock.timer_is_due(timer, 2.1));

        clock.set_timer_period(timer, 0.0, 2.1);
        assert!(!clock.timer_is_due(timer, 100.0));
    }
}

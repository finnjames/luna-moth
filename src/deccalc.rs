use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

pub const SOUTH_DEC: i32 = -25;
pub const NORTH_DEC: i32 = 100;
pub const STEP: i32 = 5;

pub const CAL_FILENAME: &str = "dec-cal.txt";
pub const CAL_BACKUP_FILENAME: &str = "dec-cal-backup.txt";

/// Every declination that gets calibrated, from south to north
pub fn dec_list() -> Vec<i32> {
    (SOUTH_DEC..=NORTH_DEC).step_by(STEP as usize).collect()
}

#[derive(Debug)]
pub enum DecCalError {
    NotFound,
    Invalid(String),
    Io(io::Error),
}

impl fmt::Display for DecCalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(f, "dec calibration file not found"),
            Self::Invalid(reason) => write!(f, "{reason}"),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

/// X and Y value pair
#[derive(Clone, Copy, Debug, PartialEq)]
struct Xy {
    x: f64,
    y: f64,
}

#[derive(Debug)]
pub struct DecCalc {
    fx: Vec<Xy>,
}

impl DecCalc {
    /// A calculator with the placeholder calibration that's used until a real one is saved
    pub fn new() -> Self {
        let mut dec_calc = Self { fx: Vec::new() };
        dec_calc.set_default();
        dec_calc
    }

    fn set_default(&mut self) {
        let xs = (-90..=90).step_by(15).map(|a| f64::from(a) * 0.01);
        self.fx = xs
            .zip(dec_list())
            .map(|(x, y)| Xy { x, y: f64::from(y) })
            .collect();
    }

    /// Read the dec calibration from file and store it in memory. Falls back to the
    /// placeholder calibration if the file can't be used.
    pub fn load_dec_cal(&mut self, path: &Path) -> Result<(), DecCalError> {
        let result = fs::read_to_string(path)
            .map_err(|err| match err.kind() {
                io::ErrorKind::NotFound => DecCalError::NotFound,
                _ => DecCalError::Io(err),
            })
            .and_then(|contents| parse_dec_cal(&contents));
        match result {
            Ok(fx) => {
                self.fx = fx;
                Ok(())
            }
            Err(err) => {
                self.set_default();
                Err(err)
            }
        }
    }

    /// Calculate the true dec from declinometer input and calibration data
    pub fn calculate_declination(&self, input_dec: f64) -> f64 {
        let fx = &self.fx;
        let first = fx[0];
        let last = fx[fx.len() - 1];

        // (dy/dx)x + y_0
        let interpolate = |a: Xy, b: Xy, origin: Xy| {
            (b.y - a.y) / (b.x - a.x) * (input_dec - origin.x) + origin.y
        };

        // Input is within data
        if first.x <= input_dec && input_dec <= last.x {
            for pair in fx.windows(2) {
                if pair[0].x <= input_dec && input_dec <= pair[1].x {
                    return interpolate(pair[0], pair[1], pair[0]);
                }
            }
        }

        // Input is below data
        if input_dec < first.x {
            return interpolate(first, fx[1], first);
        }

        // Input is above data
        interpolate(fx[fx.len() - 2], last, last)
    }
}

fn parse_dec_cal(contents: &str) -> Result<Vec<Xy>, DecCalError> {
    let fx = contents
        .lines()
        .zip(dec_list())
        .map(|(line, y)| {
            let x = line.trim().parse::<f64>().ok().filter(|x| x.is_finite());
            x.map(|x| Xy { x, y: f64::from(y) })
                .ok_or_else(|| DecCalError::Invalid("values must be numbers".to_owned()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if fx.len() < 2 {
        return Err(DecCalError::Invalid("not enough values".to_owned()));
    }
    Ok(fx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linear_cal() -> DecCalc {
        // Declinometer reads exactly twice the declination
        let contents: String = dec_list()
            .iter()
            .map(|dec| format!("{}\n", dec * 2))
            .collect();
        DecCalc {
            fx: parse_dec_cal(&contents).unwrap(),
        }
    }

    #[test]
    fn dec_list_covers_south_to_north() {
        let decs = dec_list();
        assert_eq!(decs.len(), 26);
        assert_eq!(decs[0], -25);
        assert_eq!(decs[25], 100);
    }

    #[test]
    fn interpolates_within_data() {
        let cal = linear_cal();
        assert!((cal.calculate_declination(77.0) - 38.5).abs() < 1e-9);
        assert!((cal.calculate_declination(-50.0) - -25.0).abs() < 1e-9);
        assert!((cal.calculate_declination(200.0) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn extrapolates_outside_data() {
        let cal = linear_cal();
        assert!((cal.calculate_declination(-60.0) - -30.0).abs() < 1e-9);
        assert!((cal.calculate_declination(210.0) - 105.0).abs() < 1e-9);
    }

    #[test]
    fn default_calibration_matches_placeholder() {
        let cal = DecCalc::new();
        assert_eq!(cal.fx.len(), 13);
        assert!((cal.calculate_declination(-0.9) - -25.0).abs() < 1e-9);
        assert!((cal.calculate_declination(0.9) - 35.0).abs() < 1e-9);
    }

    #[test]
    fn rejects_bad_files() {
        assert!(matches!(
            parse_dec_cal("1.0\nabc\n"),
            Err(DecCalError::Invalid(_))
        ));
        assert!(matches!(
            parse_dec_cal("1.0\n"),
            Err(DecCalError::Invalid(_))
        ));
        let mut cal = linear_cal();
        let missing = Path::new("this-file-does-not-exist.txt");
        assert!(matches!(
            cal.load_dec_cal(missing),
            Err(DecCalError::NotFound)
        ));
        assert_eq!(cal.fx.len(), 13); // Fell back to the placeholder
    }
}

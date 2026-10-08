//! Reads angles from the declinometer over its serial connection.

use std::io::{self, Read};

use serialport::{ClearBuffer, SerialPort};

use crate::dataq::{SimParams, open_port};

pub struct Declinometer {
    /// `None` when there's no device, in which case data is simulated
    ser: Option<Box<dyn SerialPort>>,
    /// Bytes received since the last carriage return
    line: Vec<u8>,
    pub acquiring: bool,
}

impl Declinometer {
    /// A Declinometer that simulates its data
    pub fn simulated() -> Self {
        Self {
            ser: None,
            line: Vec::new(),
            acquiring: false,
        }
    }

    pub fn new(device: &str) -> io::Result<Self> {
        Ok(Self {
            ser: Some(open_port(device, 38400)?),
            line: Vec::new(),
            acquiring: false,
        })
    }

    pub fn is_simulated(&self) -> bool {
        self.ser.is_none()
    }

    pub fn start(&mut self) {
        if self.ser.is_some() {
            self.acquiring = true;
        }
    }

    #[allow(dead_code)]
    pub fn stop(&mut self) -> io::Result<()> {
        if let Some(ser) = &mut self.ser {
            ser.clear(ClearBuffer::Input)?;
            self.line.clear();
            self.acquiring = false;
        }
        Ok(())
    }

    /// This function reads the last angle from the buffer and clears the buffer.
    /// Use this as a real-time sampling method.
    pub fn read_latest(&mut self, current_time: f64, sim: &SimParams) -> io::Result<Option<f64>> {
        let Some(ser) = &mut self.ser else {
            return Ok(Some(random_data(current_time, sim)));
        };

        let available = ser.bytes_to_read()? as usize;
        if available == 0 {
            return Ok(None);
        }
        let mut buffer = vec![0; available];
        ser.read_exact(&mut buffer)?;
        Ok(latest_angle(&mut self.line, &buffer))
    }
}

/// Add newly received bytes to the pending line and parse the last complete
/// (carriage return-terminated) line that's a number, if any
fn latest_angle(line: &mut Vec<u8>, received: &[u8]) -> Option<f64> {
    let mut latest = None;
    for &byte in received {
        if byte == b'\r' {
            let angle = std::str::from_utf8(line)
                .ok()
                .and_then(|s| s.trim().parse::<f64>().ok())
                .filter(|angle| angle.is_finite());
            latest = angle.or(latest);
            line.clear();
        } else {
            line.push(byte);
        }
    }
    latest
}

// Testing

/// For testing
fn random_data(current_time: f64, sim: &SimParams) -> f64 {
    if sim.dec_auto {
        return (current_time / 2.0).sin();
    }
    sim.declination
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_last_complete_line() {
        let mut line = Vec::new();
        assert_eq!(latest_angle(&mut line, b"12.5\r-3.25\r7."), Some(-3.25));
        assert_eq!(line, b"7.");
        // The rest of the partial line arrives later
        assert_eq!(latest_angle(&mut line, b"75\r"), Some(7.75));
        assert!(line.is_empty());
    }

    #[test]
    fn skips_garbage() {
        let mut line = Vec::new();
        assert_eq!(latest_angle(&mut line, b"1.5\rxyz\r"), Some(1.5));
        assert_eq!(latest_angle(&mut line, b"\xff\xfe\r"), None);
        assert_eq!(latest_angle(&mut line, b"no terminator"), None);
    }

    #[test]
    fn simulates_when_there_is_no_device() {
        let mut declinometer = Declinometer::simulated();
        assert!(declinometer.is_simulated());
        let manual = SimParams {
            dec_auto: false,
            declination: 0.42,
            ..SimParams::default()
        };
        assert_eq!(declinometer.read_latest(0.0, &manual).unwrap(), Some(0.42));
        let auto = SimParams::default();
        assert_eq!(
            declinometer
                .read_latest(std::f64::consts::PI, &auto)
                .unwrap(),
            Some(1.0)
        );
    }
}

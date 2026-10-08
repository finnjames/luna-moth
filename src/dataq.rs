//! This module includes a DataQ struct that encapsulates a serial port and provides
//! simple methods for initializing, starting, stopping, and reading from the DATAQ
//! device connected via a USB serial connection. The module also includes a few helper
//! functions relevant to the tasks.

use std::io::{self, Read, Write};
use std::time::Duration;

use serialport::{ClearBuffer, SerialPort, SerialPortType};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SignalDatum {
    pub a: f64,
    pub b: f64,
}

/// Knobs for the simulated devices, which stand in for any hardware that isn't found
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SimParams {
    pub variance: u32,
    pub noise: u32,
    pub polarization: u32,
    pub calibration: bool,
    pub dec_auto: bool,
    /// What the declinometer reads when it isn't on auto, from -1 to 1
    pub declination: f64,
}

impl Default for SimParams {
    fn default() -> Self {
        Self {
            variance: 4,
            noise: 4,
            polarization: 4,
            calibration: false,
            dec_auto: true,
            declination: 0.0,
        }
    }
}

/// Get a list of active com ports and scan for the DATAQ and the declinometer. Returns
/// the port names of `(dataq, declinometer)`.
pub fn discovery() -> (Option<String>, Option<String>) {
    let mut dataq = None;
    let mut declinometer = None;
    for p in serialport::available_ports().unwrap_or_default() {
        if let SerialPortType::UsbPort(usb) = &p.port_type {
            match (usb.vid, usb.pid) {
                (0x0683, 0x4109) => dataq = Some(p.port_name),
                (0x0403, 0x6001) => declinometer = Some(p.port_name),
                _ => {}
            }
        }
    }
    (dataq, declinometer)
}

/// Open a serial port for polling: reads never wait around for data
pub fn open_port(device: &str, baud_rate: u32) -> serialport::Result<Box<dyn SerialPort>> {
    serialport::new(device, baud_rate)
        .timeout(Duration::from_millis(50))
        .open()
}

const RANGE_VOLT: [f64; 6] = [10.0, 5.0, 2.0, 1.0, 0.5, 0.2];

const CHANNELS: [u16; 2] = [
    0x0100, // Channel 0, telescope channel A, ±5 V range
    0x0101, // Channel 1, telescope channel B, ±5 V range
];

/// Bytes in one data point: a 16-bit sample for each channel
const DATUM_SIZE: usize = 2 * CHANNELS.len();

/// A wrapper of a serial port which supports functionalities specifically useful for
/// DATAQ instruments.
pub struct DataQ {
    /// `None` when there's no device, in which case data is simulated
    ser: Option<Box<dyn SerialPort>>,
    pub acquiring: bool,
}

impl DataQ {
    /// A DataQ that simulates its data
    pub fn simulated() -> Self {
        Self {
            ser: None,
            acquiring: false,
        }
    }

    pub fn new(device: &str) -> io::Result<Self> {
        let mut dataq = Self {
            ser: Some(open_port(device, 9600)?),
            acquiring: false,
        };
        dataq.setup()?;
        Ok(dataq)
    }

    pub fn is_simulated(&self) -> bool {
        self.ser.is_none()
    }

    pub fn start(&mut self) -> io::Result<()> {
        if self.ser.is_some() {
            self.send("start")?;
            self.acquiring = true;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn stop(&mut self) -> io::Result<()> {
        if self.ser.is_none() {
            return Ok(());
        }
        self.send("stop")?;
        if let Some(ser) = &mut self.ser {
            ser.clear(ClearBuffer::Input)?;
        }
        self.acquiring = false;
        Ok(())
    }

    /// This function reads the last datapoint from the buffer and clears the buffer.
    /// Use this as a real-time sampling method. Each datapoint has two channels:
    /// channel 0: telescope channel A
    /// channel 1: telescope channel B
    pub fn read_latest(
        &mut self,
        current_time: f64,
        sim: &SimParams,
    ) -> io::Result<Option<SignalDatum>> {
        let Some(ser) = &mut self.ser else {
            return Ok(Some(random_data(current_time, sim)));
        };

        // Data is always read in whole points; otherwise there's no way to tell the
        // channels apart.
        let available = ser.bytes_to_read()? as usize;
        let mut buffer = vec![0; available / DATUM_SIZE * DATUM_SIZE];
        if buffer.is_empty() {
            return Ok(None);
        }
        ser.read_exact(&mut buffer)?;
        let latest = &buffer[buffer.len() - DATUM_SIZE..];
        Ok(Some(SignalDatum {
            a: convert([latest[0], latest[1]], CHANNELS[0]),
            b: convert([latest[2], latest[3]], CHANNELS[1]),
        }))
    }

    // Helpers

    fn send(&mut self, command: &str) -> io::Result<()> {
        if let Some(ser) = &mut self.ser {
            ser.write_all(format!("{command}\r").as_bytes())?;
        }
        Ok(())
    }

    fn setup(&mut self) -> io::Result<()> {
        self.send("stop")?;
        self.send("encode 0")?; // 0 = binary, 1 = ascii
        self.send("ps 0")?; // Small pocketsize for responsiveness

        for (i, channel) in CHANNELS.iter().enumerate() {
            self.send(&format!("slist {i} {channel}"))?;
        }

        // Define sample rate = 100 Hz:
        // 60,000,000/(srate * dec) = 60,000,000/(1171 * 512) = 100 Hz
        self.send("dec 512")?;
        self.send("srate 1171")
    }
}

/// Convert one little-endian sample to volts
fn convert(buffer: [u8; 2], channel: u16) -> f64 {
    5.0 + RANGE_VOLT[usize::from(channel >> 8)] * f64::from(i16::from_le_bytes(buffer)) / 32768.0
}

// - MARK: Testing

/// This gives something that kind of looks like real data, for UI testing.
fn random_data(current_time: f64, sim: &SimParams) -> SignalDatum {
    let x = current_time / 8.0;

    let mut n = if fastrand::bool() { -0.2 } else { 1.0 } / (64.0 * (fastrand::f64() + 0.02));
    n *= 0.08 * f64::from(sim.noise).powi(2);

    let v = f64::from(sim.variance);

    let c = if sim.calibration { 1.0 } else { 0.0 };

    let g = 2.6 / ((2.0 * x).sin() + 1.4) + 0.4 * (8.0 * x).sin() - 0.8 * (4.0 * x).sin()
        + (1.0 / ((8.0 * x).sin() + 1.4));

    let a = g * v + n + c;
    let b = a - 0.1 * f64::from(sim.polarization) * g * (v / 2.0 + 1.0);

    // Normalize, kinda
    SignalDatum {
        a: a / 272.0 + c + 1.0,
        b: b / 272.0 + c + 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_samples_to_volts() {
        assert_eq!(convert([0x00, 0x00], CHANNELS[0]), 5.0);
        assert_eq!(convert([0x00, 0x80], CHANNELS[0]), 0.0); // i16::MIN
        assert_eq!(convert([0x00, 0x40], CHANNELS[1]), 7.5); // Half of full scale
    }

    #[test]
    fn simulates_when_there_is_no_device() {
        let mut dataq = DataQ::simulated();
        assert!(dataq.is_simulated());
        dataq.start().unwrap();
        assert!(!dataq.acquiring);

        let quiet = SimParams {
            noise: 0,
            polarization: 0,
            ..SimParams::default()
        };
        let datum = dataq.read_latest(1000.0, &quiet).unwrap().unwrap();
        assert_eq!(datum.a, datum.b); // No polarization means identical channels
        let calibrating = SimParams {
            calibration: true,
            ..quiet
        };
        let raised = dataq.read_latest(1000.0, &calibrating).unwrap().unwrap();
        assert!(raised.a > datum.a + 1.0);
    }
}

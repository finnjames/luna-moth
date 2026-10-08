//! Bleeps and bloops

use std::io::Cursor;
use std::sync::mpsc::{self, Sender};
use std::thread;

const BEEP: &[u8] = include_bytes!("../assets/beep3.wav");
const BEEP_LEGACY: &[u8] = include_bytes!("../assets/beep-legacy.wav");

const VOLUME: f32 = 0.5;
/// Seconds that must pass between beeps
const MIN_INTERVAL: f64 = 0.1;

/// Plays beeps on a dedicated audio thread
pub struct Beeper {
    sender: Option<Sender<&'static [u8]>>,
    last_beep_time: f64,
}

impl Beeper {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel::<&'static [u8]>();
        thread::spawn(move || {
            // The sink has to stay alive on this thread for as long as there are beeps
            let sink = match rodio::DeviceSinkBuilder::open_default_sink() {
                Ok(sink) => Some(sink),
                Err(err) => {
                    eprintln!("No audio output, beeps will be silent: {err}");
                    None
                }
            };
            for sound in receiver {
                let Some(sink) = &sink else {
                    continue;
                };
                match rodio::play(sink.mixer(), Cursor::new(sound)) {
                    Ok(player) => {
                        player.set_volume(VOLUME);
                        player.detach();
                    }
                    Err(err) => eprintln!("Couldn't play beep: {err}"),
                }
            }
        });
        Self {
            sender: Some(sender),
            last_beep_time: 0.0,
        }
    }

    /// A beeper that never makes a sound
    #[cfg(test)]
    pub fn silent() -> Self {
        Self {
            sender: None,
            last_beep_time: 0.0,
        }
    }

    /// Make beep play for user
    pub fn beep(&mut self, current_time: f64, legacy: bool) {
        if current_time - self.last_beep_time > MIN_INTERVAL {
            self.last_beep_time = current_time;
            if let Some(sender) = &self.sender {
                let _ = sender.send(if legacy { BEEP_LEGACY } else { BEEP });
            }
        }
    }
}

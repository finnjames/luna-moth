# Luna Moth 🔭

The data acquisition system for the 40-foot radio telescope at the [Green Bank Observatory](https://greenbankobservatory.org/). Built with Rust and [egui](https://github.com/emilk/egui). This software is part of the [ERIRA](https://www.danreichart.com/erira) program.

It talks to a [SOLAR-360-2-RS232](https://www.leveldevelopments.com/products/inclinometers/inclinometer-sensors/single-axis-inclinometer-sensors/solar-360-series/solar-360-2-rs232-inclinometer-sensor-single-axis-180-rs232-with-tc/) inclinometer and a DataQ A2D card, both over USB serial. Any device that isn't found is simulated, so the app runs anywhere.

## Running

Install [Rust](https://rustup.rs/), clone this repo, and `cd` into it. Then:

```
$ cargo run --release
```

On Linux, the audio and serial libraries need ALSA and udev headers first (`libasound2-dev` and `libudev-dev` on Debian/Ubuntu).

Run it from the directory where the data should go. It reads and writes these relative to the working directory:

| Path | Contents |
| --- | --- |
| `data/` | Observation files (`.md1` for scans and spectra, `.md2` for surveys) |
| `dec-cal.txt` | Declination calibration, one declinometer reading per line from -25° to 100° |
| `dec-cal-backup.txt` | The calibration that the last one replaced |

## Testing

```
$ cargo test
```

The UI tests drive the real app headlessly. To also save screenshots of what they see (this needs a GPU):

```
$ LUNA_MOTH_SCREENSHOTS=/some/directory cargo test
```

## Layout

| File | What it does |
| --- | --- |
| `src/app.rs` | The main window |
| `src/core.rs` | Reads the instruments and runs observations at 100Hz, on its own thread |
| `src/dialogs/` | Alert, credits, quit, RA and dec calibration, and observation dialogs |
| `src/observation.rs` | The observation state machine |
| `src/data_file.rs` | Observation data files |
| `src/clock.rs` | Sidereal time and timers |
| `src/deccalc.rs` | Declinometer reading to declination |
| `src/dataq.rs`, `src/declinometer.rs` | The DataQ and the declinometer |

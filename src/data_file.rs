//! Functionalities needed for working with files.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

/// A data file. Every value written goes on its own line and is handed straight to the
/// OS, so a crash loses nothing.
#[derive(Debug)]
pub struct DataFile {
    file: File,
}

impl DataFile {
    /// Create (or clear) `filename` in `dir`, creating `dir` if necessary
    pub fn new(dir: &Path, filename: &str) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let file = File::create(dir.join(filename))?;
        Ok(Self { file })
    }

    pub fn write(&mut self, val: &str) -> io::Result<()> {
        self.file.write_all(format!("{val}\n").as_bytes())
    }
}

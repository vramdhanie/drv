use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};

use crate::drive::DriveFile;

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// "2026-09-14T18:00:00.000Z" -> "2026-09-14 18:00"
pub fn short_time(rfc3339: &str) -> String {
    let date = rfc3339.get(0..10).unwrap_or(rfc3339);
    let time = rfc3339.get(11..16).unwrap_or("");
    format!("{date} {time}")
}

/// Colorized name by kind: folders blue, Google-native docs cyan, rest plain.
pub fn painted_name(file: &DriveFile) -> String {
    if file.is_folder() {
        format!("{}/", file.name).blue().bold().to_string()
    } else if file.mime_type.starts_with("application/vnd.google-apps.") {
        file.name.cyan().to_string()
    } else {
        file.name.clone()
    }
}

pub fn transfer_bar(len: u64, label: &str) -> ProgressBar {
    let bar = ProgressBar::new(len);
    bar.set_style(
        ProgressStyle::with_template(
            "{msg:20!} [{bar:30.green}] {bytes}/{total_bytes} {bytes_per_sec:>12}",
        )
        .unwrap()
        .progress_chars("=> "),
    );
    bar.set_message(label.to_string());
    bar
}

/// A reader wrapper that advances a progress bar as it is consumed.
pub struct ProgressReader<R> {
    inner: R,
    bar: ProgressBar,
}

impl<R> ProgressReader<R> {
    pub fn new(inner: R, bar: ProgressBar) -> Self {
        Self { inner, bar }
    }
}

impl<R: std::io::Read> std::io::Read for ProgressReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bar.inc(n as u64);
        Ok(n)
    }
}

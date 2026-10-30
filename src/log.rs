use crate::{store, time};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// One file per UTC day under `$DIFFY_HOME/logs`, so a busy install does not
/// grow a single unbounded file.
pub fn info(message: impl AsRef<str>) {
    write("INFO", message.as_ref());
}

pub fn error(message: impl AsRef<str>) {
    write("ERROR", message.as_ref());
}

// shortcut: a directory that cannot be created is never retried, so logging
// stays off for the rest of the run. Enough for a broken home directory.
fn logs_dir() -> Option<&'static PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = store::base_dir().ok()?.join("logs");
        fs::create_dir_all(&dir).ok()?;
        Some(dir)
    })
    .as_ref()
}

fn write(level: &str, message: &str) {
    if let Some(dir) = logs_dir() {
        append(dir, level, message);
    }
}

fn append(dir: &Path, level: &str, message: &str) {
    let stamp = time::utc_iso(time::now());
    let path = dir.join(format!("diffy-{}.log", &stamp[..10]));
    let line = format!("{stamp} {level:<5} {message}\n");
    // Logging must never take a review or the app down, so failures are dropped.
    let _ = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(line.as_bytes()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_timestamped_lines_and_creates_the_file() {
        let dir = std::env::temp_dir().join(format!("diffy-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        append(&dir, "INFO", "review started");
        append(&dir, "ERROR", "gh failed");

        let mut files: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(files.len(), 1, "one file per day");
        let lines = fs::read_to_string(files.remove(0)).unwrap();
        let lines: Vec<&str> = lines.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with(" INFO  review started"), "{lines:?}");
        assert!(lines[1].ends_with(" ERROR gh failed"), "{lines:?}");
        fs::remove_dir_all(&dir).unwrap();
    }
}

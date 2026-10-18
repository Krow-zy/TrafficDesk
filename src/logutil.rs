//! Minimal size-rotating file logger (`trafficdesk.log`, 2 MB x 3 files).

use log::{LevelFilter, Log, Metadata, Record};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MAX_BYTES: u64 = 2 * 1024 * 1024;
const KEEP: usize = 3;

struct Logger {
    path: PathBuf,
    file: Mutex<File>,
    level: LevelFilter,
}

impl Logger {
    fn write_line(&self, line: &str) {
        let Ok(mut file) = self.file.lock() else {
            return;
        };
        if file.write_all(line.as_bytes()).is_err() {
            return;
        }
        let _ = file.flush();
        if file.metadata().map(|m| m.len()).unwrap_or(0) > MAX_BYTES {
            drop(file);
            self.rotate();
        }
    }

    fn rotate(&self) {
        let Ok(mut file) = self.file.lock() else {
            return;
        };
        // log.2 <- log.1 <- log
        let _ = std::fs::remove_file(with_suffix(&self.path, KEEP - 1));
        for i in (1..KEEP - 1).rev() {
            let _ = std::fs::rename(with_suffix(&self.path, i), with_suffix(&self.path, i + 1));
        }
        let _ = std::fs::rename(&self.path, with_suffix(&self.path, 1));
        if let Ok(f) = open(&self.path) {
            *file = f;
        }
    }
}

fn with_suffix(path: &Path, n: usize) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(format!(".{n}"));
    PathBuf::from(s)
}

fn open(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} {:<5} {} {}\n",
            crate::ffi::local_now_string(),
            record.level().as_str(),
            record.target(),
            record.args()
        );
        self.write_line(&line);
    }

    fn flush(&self) {
        if let Ok(mut f) = self.file.lock() {
            let _ = f.flush();
        }
    }
}

pub fn init(dir: &Path) -> Result<(), String> {
    let path = dir.join("trafficdesk.log");
    let file = open(&path).map_err(|e| format!("cannot open log file: {e}"))?;
    let level = if std::env::var_os("TRAFFICDESK_DEBUG").is_some() {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    };
    let logger = Logger {
        path,
        file: Mutex::new(file),
        level,
    };
    let logger: &'static Logger = Box::leak(Box::new(logger));
    log::set_logger(logger).map_err(|e| e.to_string())?;
    log::set_max_level(level);
    Ok(())
}

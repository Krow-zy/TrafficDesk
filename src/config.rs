//! config.toml handling: defaults, clamping, hot reload.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub poll_interval_ms: u64,
    pub retention_days: u32,
    pub ignore_processes: Vec<String>,
    pub mode: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            poll_interval_ms: 1000,
            retention_days: 14,
            ignore_processes: Vec::new(),
            mode: "panel".to_string(),
        }
    }
}

impl Config {
    pub fn clamp(&mut self) {
        self.poll_interval_ms = self.poll_interval_ms.clamp(500, 5000);
        self.retention_days = self.retention_days.clamp(1, 365);
        if !matches!(self.mode.as_str(), "mini" | "panel" | "expanded") {
            self.mode = "panel".to_string();
        }
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    /// True when `name` should be filtered out of the aggregate.
    pub fn ignores(&self, process: &str) -> bool {
        let p = process.to_ascii_lowercase();
        self.ignore_processes
            .iter()
            .any(|pat| !pat.trim().is_empty() && p.contains(&pat.trim().to_ascii_lowercase()))
    }
}

#[derive(Clone)]
pub struct Paths {
    pub dir: PathBuf,
    pub config: PathBuf,
    pub db: PathBuf,
    pub log: PathBuf,
}

impl Paths {
    /// Runtime data stays out of the program directory: `TRAFFICDESK_DATA_DIR`
    /// when set, otherwise a folder in the user's temp directory. Nothing is
    /// left next to the sources, and deleting the folder at any time is fine
    /// (it costs the stored history and nothing else).
    pub fn resolve() -> Self {
        let dir = std::env::var_os("TRAFFICDESK_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("TrafficDesk"));
        let _ = std::fs::create_dir_all(&dir);
        Self {
            config: dir.join("config.toml"),
            db: dir.join("trafficdesk.db"),
            log: dir.join("trafficdesk.log"),
            dir,
        }
    }
}

/// Load config.toml, creating it with defaults on first run.
pub fn load_or_create(path: &Path) -> Config {
    if !path.exists() {
        let cfg = Config::default();
        if let Err(e) = std::fs::write(path, cfg.to_toml()) {
            log::warn!("could not write default config.toml: {e}");
        } else {
            log::info!("wrote default config to {}", path.display());
        }
        return cfg;
    }
    match std::fs::read_to_string(path) {
        Ok(text) => match toml::from_str::<Config>(&text) {
            Ok(mut cfg) => {
                cfg.clamp();
                cfg
            }
            Err(e) => {
                log::warn!("config.toml is invalid ({e}); using defaults");
                Config::default()
            }
        },
        Err(e) => {
            log::warn!("could not read config.toml: {e}; using defaults");
            Config::default()
        }
    }
}

/// Cheap mtime-based change detector for hot reload.
#[derive(Default)]
pub struct ConfigWatcher {
    last: Option<std::time::SystemTime>,
}

impl ConfigWatcher {
    /// Returns the new config when the file changed and parsed successfully.
    pub fn poll(&mut self, path: &Path) -> Option<(Config, Result<(), String>)> {
        let meta = std::fs::metadata(path).ok()?;
        let mtime = meta.modified().ok()?;
        if self.last == Some(mtime) {
            return None;
        }
        self.last = Some(mtime);
        match toml::from_str::<Config>(&std::fs::read_to_string(path).unwrap_or_default()) {
            Ok(mut cfg) => {
                cfg.clamp();
                Some((cfg, Ok(())))
            }
            Err(e) => Some((Config::default(), Err(e.to_string()))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_data_never_lands_next_to_the_sources() {
        // Without an override the data folder sits inside the temp directory.
        // SAFETY: single-threaded test that removes the variable before use.
        unsafe { std::env::remove_var("TRAFFICDESK_DATA_DIR") };
        let paths = Paths::resolve();
        assert!(
            paths.dir.starts_with(std::env::temp_dir()),
            "dir was {}",
            paths.dir.display()
        );
        assert_eq!(paths.db.parent(), Some(paths.dir.as_path()));
        assert_eq!(
            paths.log.file_name().and_then(|n| n.to_str()),
            Some("trafficdesk.log")
        );

        // The environment override wins, and the folder is created on demand.
        let custom = std::env::temp_dir().join("trafficdesk-test-data-dir");
        unsafe { std::env::set_var("TRAFFICDESK_DATA_DIR", &custom) };
        let paths = Paths::resolve();
        assert_eq!(paths.dir, custom);
        assert!(paths.dir.is_dir());
        unsafe { std::env::remove_var("TRAFFICDESK_DATA_DIR") };
        let _ = std::fs::remove_dir_all(&custom);
    }

    #[test]
    fn values_are_clamped_and_bad_modes_reset() {
        let mut cfg = Config {
            poll_interval_ms: 1,
            retention_days: 9999,
            ignore_processes: vec![],
            mode: "weird".into(),
        };
        cfg.clamp();
        assert_eq!(cfg.poll_interval_ms, 500);
        assert_eq!(cfg.retention_days, 365);
        assert_eq!(cfg.mode, "panel");
    }
}

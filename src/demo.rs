//! Sample data for screenshots and for trying the interface without touching
//! real traffic. Enabled only by `TRAFFICDESK_DEMO=1`: the collector is
//! replaced by a deterministic generator, so nothing is captured, nothing is
//! counted and nothing is written to the database.

use crate::collect::Shared;
use crate::model::{
    Category, ConnEvent, DestAgg, NameSource, ProcAgg, Snapshot, now_ms, sort_dests,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// True when the process was started with `TRAFFICDESK_DEMO` set to anything
/// other than `0`.
pub fn enabled() -> bool {
    std::env::var_os("TRAFFICDESK_DEMO").is_some_and(|v| v != "0")
}

/// A row of the sample destination table.
struct Row {
    /// Hostname, or empty for a destination that only ever had an address.
    name: &'static str,
    ip: &'static str,
    named: NameSource,
    process: &'static str,
    pid: u32,
    path: &'static str,
    live: u32,
    events: u64,
    /// Milliseconds before "now" that the destination was last active.
    age_ms: i64,
    /// (process, live connections) pairs, most active first.
    procs: &'static [(&'static str, u32)],
}

const CHROME_PATH: &str = r"C:\Program Files\Google\Chrome\Application\chrome.exe";
const EDGE_PATH: &str = r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe";
const NODE_PATH: &str = r"C:\Program Files\nodejs\node.exe";
const PYTHON_PATH: &str = r"C:\Python313\python.exe";
const CARGO_PATH: &str = r"C:\Users\user\.cargo\bin\cargo.exe";
const SVCHOST_PATH: &str = r"C:\Windows\System32\svchost.exe";

const ROWS: &[Row] = &[
    Row {
        name: "github.com",
        ip: "140.82.121.4",
        named: NameSource::Cache,
        process: "chrome.exe",
        pid: 7312,
        path: CHROME_PATH,
        live: 3,
        events: 412,
        age_ms: 0,
        procs: &[("chrome.exe", 3), ("msedge.exe", 1)],
    },
    Row {
        name: "registry.npmjs.org",
        ip: "104.16.7.34",
        named: NameSource::Cache,
        process: "node.exe",
        pid: 9204,
        path: NODE_PATH,
        live: 2,
        events: 240,
        age_ms: 0,
        procs: &[("node.exe", 2)],
    },
    Row {
        name: "objects.githubusercontent.com",
        ip: "185.199.108.133",
        named: NameSource::Remembered,
        process: "msedge.exe",
        pid: 4418,
        path: EDGE_PATH,
        live: 1,
        events: 96,
        age_ms: 4_000,
        procs: &[("msedge.exe", 1)],
    },
    Row {
        name: "api.github.com",
        ip: "140.82.113.6",
        named: NameSource::Cache,
        process: "chrome.exe",
        pid: 7312,
        path: CHROME_PATH,
        live: 1,
        events: 158,
        age_ms: 2_000,
        procs: &[("chrome.exe", 1)],
    },
    Row {
        name: "cdn.jsdelivr.net",
        ip: "151.101.1.229",
        named: NameSource::Remembered,
        process: "chrome.exe",
        pid: 7312,
        path: CHROME_PATH,
        live: 1,
        events: 77,
        age_ms: 9_000,
        procs: &[("chrome.exe", 1), ("msedge.exe", 1)],
    },
    Row {
        name: "speed.cloudflare.com",
        ip: "104.16.132.229",
        named: NameSource::Cache,
        process: "chrome.exe",
        pid: 7312,
        path: CHROME_PATH,
        live: 1,
        events: 29,
        age_ms: 1_000,
        procs: &[("chrome.exe", 1)],
    },
    Row {
        name: "pypi.org",
        ip: "151.101.64.223",
        named: NameSource::Remembered,
        process: "python.exe",
        pid: 5108,
        path: PYTHON_PATH,
        live: 1,
        events: 63,
        age_ms: 6_000,
        procs: &[("python.exe", 1)],
    },
    Row {
        name: "time.windows.com",
        ip: "20.112.250.133",
        named: NameSource::Remembered,
        process: "svchost.exe",
        pid: 1180,
        path: SVCHOST_PATH,
        live: 1,
        events: 22,
        age_ms: 12_000,
        procs: &[("svchost.exe", 1)],
    },
    Row {
        name: "",
        ip: "52.97.146.162",
        named: NameSource::None,
        process: "svchost.exe",
        pid: 1180,
        path: SVCHOST_PATH,
        live: 1,
        events: 41,
        age_ms: 3_000,
        procs: &[("svchost.exe", 1)],
    },
    Row {
        name: "crates.io",
        ip: "146.75.78.137",
        named: NameSource::Cache,
        process: "cargo.exe",
        pid: 6620,
        path: CARGO_PATH,
        live: 0,
        events: 51,
        age_ms: 96_000,
        procs: &[("cargo.exe", 0)],
    },
    Row {
        name: "docs.rs",
        ip: "151.101.129.140",
        named: NameSource::Remembered,
        process: "chrome.exe",
        pid: 7312,
        path: CHROME_PATH,
        live: 0,
        events: 34,
        age_ms: 240_000,
        procs: &[("chrome.exe", 0)],
    },
    Row {
        name: "",
        ip: "13.107.42.12",
        named: NameSource::None,
        process: "System",
        pid: 4,
        path: "",
        live: 0,
        events: 18,
        age_ms: 420_000,
        procs: &[("System", 0)],
    },
];

/// The picture for a given tick. Depends on nothing but `tick`, so two runs
/// render byte-identical frames.
pub fn snapshot(tick: u64) -> Snapshot {
    let now = now_ms();
    let dests = ROWS
        .iter()
        .map(|row| {
            let dest = if row.name.is_empty() {
                row.ip.to_string()
            } else {
                row.name.to_string()
            };
            DestAgg {
                dest,
                is_domain: !row.name.is_empty(),
                named: row.named,
                named_at: if row.named == NameSource::Remembered {
                    now - 45 * 60 * 1000
                } else {
                    0
                },
                ip: row.ip.to_string(),
                live: row.live,
                events: row.events,
                first_seen: now - 3 * 60 * 60 * 1000,
                last_seen: now - row.age_ms,
                top_process: row.process.to_string(),
                top_pid: row.pid,
                top_path: row.path.to_string(),
                category: row.category(),
                procs: row
                    .procs
                    .iter()
                    .map(|(name, live)| ((*name).to_string(), *live))
                    .collect(),
            }
        })
        .collect();

    let procs = vec![
        proc(
            7312,
            "chrome.exe",
            CHROME_PATH,
            6,
            640,
            8,
            Category::Browser,
            now,
        ),
        proc(
            4418,
            "msedge.exe",
            EDGE_PATH,
            2,
            210,
            4,
            Category::Browser,
            now,
        ),
        proc(9204, "node.exe", NODE_PATH, 2, 240, 3, Category::Other, now),
        proc(
            5108,
            "python.exe",
            PYTHON_PATH,
            1,
            63,
            2,
            Category::Other,
            now,
        ),
        proc(
            6620,
            "cargo.exe",
            CARGO_PATH,
            0,
            51,
            2,
            Category::Other,
            now - 96_000,
        ),
        proc(
            1180,
            "svchost.exe",
            SVCHOST_PATH,
            2,
            63,
            4,
            Category::System,
            now,
        ),
        proc(4, "System", "", 0, 18, 3, Category::System, now - 420_000),
    ];

    let spark = sparkline();
    let (up_bps, down_bps) = spark.last().copied().unwrap_or((0.0, 0.0));

    Snapshot {
        tick,
        up_bps,
        down_bps,
        dests,
        procs,
        spark,
        errors: Vec::new(),
        dns_entries: 58,
        names_remembered: 103,
        dns_error: None,
        connections_seen: 47,
        interval_ms: 1000,
    }
}

impl Row {
    fn category(&self) -> Category {
        Category::classify(self.process, self.path, self.pid)
    }
}

#[allow(clippy::too_many_arguments)]
fn proc(
    pid: u32,
    process: &str,
    path: &str,
    live: u32,
    events: u64,
    dests: u32,
    category: Category,
    last_seen: i64,
) -> ProcAgg {
    ProcAgg {
        pid,
        process: process.to_string(),
        path: path.to_string(),
        live,
        events,
        last_seen,
        dests,
        category,
    }
}

/// Fixed 60-sample shape for the sparkline: busy but calm enough to read.
fn sparkline() -> Vec<(f64, f64)> {
    (0..crate::collect::SPARK_LEN)
        .map(|i| {
            let t = i as f64;
            let down =
                210_000.0 + 150_000.0 * (t * 0.31).sin().abs() + 45_000.0 * (t * 1.07).sin().abs();
            let up =
                34_000.0 + 28_000.0 * (t * 0.19).cos().abs() + 9_000.0 * (t * 0.83).sin().abs();
            (up, down)
        })
        .collect()
}

/// Stand-in event log for the detail pane, so the demo does not need a database.
pub fn timeline(dest: &str) -> Vec<ConnEvent> {
    let now = now_ms();
    let base = ROWS.iter().find(|row| row.name == dest || row.ip == dest);
    let (ip, port, pid, process, path) = match base {
        Some(row) => (
            row.ip.to_string(),
            443u16,
            row.pid,
            row.process.to_string(),
            row.path.to_string(),
        ),
        None => (
            "203.0.113.1".to_string(),
            443,
            4,
            "System".to_string(),
            String::new(),
        ),
    };
    const STATES: [&str; 5] = ["ESTAB", "ESTAB", "CLOSE_WAIT", "TIME_WAIT", "ESTAB"];
    (0..6)
        .map(|i| ConnEvent {
            ts: now - (i as i64 + 1) * 47_000,
            dest: dest.to_string(),
            is_domain: !ip.is_empty(),
            ip: ip.clone(),
            port,
            pid,
            process: process.clone(),
            path: path.clone(),
            state: STATES[i % STATES.len()].to_string(),
        })
        .collect()
}

/// Publish the sample picture once a second, the way the collector would.
pub fn spawn(shared: Arc<Mutex<Shared>>, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("trafficdesk-demo".into())
        .spawn(move || {
            log::info!("running with TRAFFICDESK_DEMO: sample data, no capture");
            let mut tick = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let mut snap = snapshot(tick);
                sort_dests(&mut snap.dests);
                if let Ok(mut s) = shared.lock() {
                    s.snap = snap;
                }
                tick += 1;
                std::thread::sleep(Duration::from_millis(1000));
            }
            log::info!("demo generator stopped");
        })
        .expect("demo thread")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_is_consistent() {
        let a = snapshot(7);
        let b = snapshot(7);
        assert_eq!(
            a.dests.len(),
            b.dests.len(),
            "same tick must give the same picture"
        );
        assert!(!a.dests.is_empty());
        assert!(!a.procs.is_empty());
        assert_eq!(a.spark.len(), crate::collect::SPARK_LEN);
        assert!(a.up_bps > 0.0 && a.down_bps > 0.0);
        assert_eq!(a.connections_seen, 47);
        for d in &a.dests {
            assert!(!d.dest.is_empty());
            assert_eq!(d.is_domain, d.named.is_named());
            assert!(d.procs.iter().map(|(_, live)| *live).sum::<u32>() >= d.live);
            assert!(
                d.procs.iter().any(|(name, _)| name == &d.top_process),
                "{} has no owning process",
                d.dest
            );
            assert!(d.last_seen <= now_ms());
        }
        // The sample set has to show every name source, or the legend lies.
        for source in [NameSource::Cache, NameSource::Remembered, NameSource::None] {
            assert!(
                a.dests.iter().any(|d| d.named == source),
                "no {source:?} row in the sample"
            );
        }
    }

    #[test]
    fn sorting_puts_live_rows_first() {
        let mut snap = snapshot(1);
        sort_dests(&mut snap.dests);
        assert!(snap.dests[0].live > 0);
        assert_eq!(snap.dests[snap.dests.len() - 1].live, 0);
    }

    #[test]
    fn timeline_never_mentions_a_missing_destination() {
        let events = timeline("github.com");
        assert_eq!(events.len(), 6);
        assert!(events.iter().all(|e| e.dest == "github.com"));
        // An unknown name still produces a plausible, non-empty timeline.
        let other = timeline("203.0.113.9");
        assert_eq!(other.len(), 6);
        assert!(other.iter().all(|e| !e.state.is_empty()));
    }
}

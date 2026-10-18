//! Shared data shapes between the collector thread and the UI.

use std::net::IpAddr;

pub type Ts = i64;

pub fn now_ms() -> Ts {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Category {
    System,
    Browser,
    #[default]
    Other,
}

impl Category {
    pub fn classify(name: &str, path: &str, pid: u32) -> Self {
        let lower_path = path.to_ascii_lowercase();
        if pid == 0
            || pid == 4
            || name.eq_ignore_ascii_case("system")
            || name.eq_ignore_ascii_case("idle")
            || name.eq_ignore_ascii_case("registry")
            || lower_path.starts_with("c:\\windows")
        {
            return Self::System;
        }
        let n = name.to_ascii_lowercase();
        const BROWSERS: [&str; 9] = [
            "chrome",
            "msedge",
            "firefox",
            "brave",
            "opera",
            "vivaldi",
            "iexplore",
            "zen",
            "librewolf",
        ];
        if BROWSERS.iter().any(|b| n.contains(b)) {
            return Self::Browser;
        }
        Self::Other
    }
}

#[derive(Clone, Debug)]
pub struct Conn {
    pub pid: u32,
    pub remote: IpAddr,
    pub remote_port: u16,
    pub state: u32,
}

impl Conn {
    pub fn state_name(&self) -> &'static str {
        state_name(self.state)
    }
}

/// MIB_TCP_STATE_* values.
pub fn state_name(state: u32) -> &'static str {
    match state {
        1 => "CLOSED",
        2 => "LISTEN",
        3 => "SYN_SENT",
        4 => "SYN_RCVD",
        5 => "ESTAB",
        6 => "FIN_WAIT1",
        7 => "FIN_WAIT2",
        8 => "CLOSE_WAIT",
        9 => "CLOSING",
        10 => "LAST_ACK",
        11 => "TIME_WAIT",
        12 => "DELETE_TCB",
        _ => "UNKNOWN",
    }
}

/// Established-ish states count as a live connection. LISTEN, TIME_WAIT and
/// DELETE_TCB are socket bookkeeping, not conversations with a peer.
pub fn is_live_state(state: u32) -> bool {
    matches!(state, 3..=10)
}

/// Where a destination's display name came from. Naming uses the Windows DNS
/// client cache only, never a lookup of our own.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum NameSource {
    /// Unnamed: the panel shows the literal IP.
    #[default]
    None,
    /// Named by the live DNS client cache right now.
    Cache,
    /// Named earlier from the cache and remembered, so the name outlives the
    /// cache entry that taught it to us.
    Remembered,
}

impl NameSource {
    pub fn is_named(self) -> bool {
        !matches!(self, Self::None)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "ip only",
            Self::Cache => "dns cache",
            Self::Remembered => "remembered",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DestAgg {
    /// Domain when a name is known, otherwise the literal IP.
    pub dest: String,
    pub is_domain: bool,
    /// How the name was obtained, for the detail pane and the row dot.
    pub named: NameSource,
    /// When the name was learned (0 while it comes straight from the cache).
    pub named_at: Ts,
    pub ip: String,
    pub live: u32,
    /// Connection events observed for this destination (persisted total).
    pub events: u64,
    pub first_seen: Ts,
    pub last_seen: Ts,
    pub top_process: String,
    pub top_pid: u32,
    pub top_path: String,
    pub category: Category,
    /// (process, live conns) pairs, most active first.
    pub procs: Vec<(String, u32)>,
}

#[derive(Clone, Debug, Default)]
pub struct ProcAgg {
    pub pid: u32,
    pub process: String,
    pub path: String,
    pub live: u32,
    pub events: u64,
    pub last_seen: Ts,
    pub dests: u32,
    pub category: Category,
}

/// One connection transition, persisted so the detail view has a timeline.
#[derive(Clone, Debug, Default)]
pub struct ConnEvent {
    pub ts: Ts,
    pub dest: String,
    pub is_domain: bool,
    pub ip: String,
    pub port: u16,
    pub pid: u32,
    pub process: String,
    pub path: String,
    pub state: String,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub tick: u64,
    pub up_bps: f64,
    pub down_bps: f64,
    pub dests: Vec<DestAgg>,
    pub procs: Vec<ProcAgg>,
    /// Rolling 60 samples of (up, down) bytes/s, oldest first.
    pub spark: Vec<(f64, f64)>,
    pub errors: Vec<String>,
    pub dns_entries: usize,
    /// Names kept in the name memory, which outlive their DNS cache entries.
    pub names_remembered: usize,
    pub dns_error: Option<String>,
    pub connections_seen: usize,
    pub interval_ms: u64,
}

/// Sorted most-interesting-first: live conversations, then recency, then volume.
pub fn sort_dests(dests: &mut [DestAgg]) {
    dests.sort_by(|a, b| {
        b.live
            .cmp(&a.live)
            .then(b.last_seen.cmp(&a.last_seen))
            .then(b.events.cmp(&a.events))
            .then(a.dest.cmp(&b.dest))
    });
}

pub fn fmt_rate(bytes_per_sec: f64) -> String {
    let v = bytes_per_sec.max(0.0);
    if v >= 1_000_000.0 {
        format!("{:.1}M/s", v / 1_000_000.0)
    } else if v >= 1_000.0 {
        format!("{:.0}k/s", v / 1_000.0)
    } else {
        format!("{v:.0}/s")
    }
}

pub fn fmt_ago(ts: Ts) -> String {
    let now = now_ms();
    let d = (now - ts).max(0) / 1000;
    match d {
        0..=2 => "now".to_string(),
        3..=59 => format!("{d}s"),
        60..=3599 => format!("{}m", d / 60),
        3600..=86399 => format!("{}h", d / 3600),
        _ => format!("{}d", d / 86400),
    }
}

pub fn fmt_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories() {
        assert_eq!(
            Category::classify("svchost.exe", "C:\\Windows\\System32\\svchost.exe", 900),
            Category::System
        );
        assert_eq!(Category::classify("System", "", 4), Category::System);
        assert_eq!(
            Category::classify(
                "chrome.exe",
                "C:\\Program Files\\Google\\Chrome\\chrome.exe",
                1234
            ),
            Category::Browser
        );
        assert_eq!(
            Category::classify("slack.exe", "C:\\Users\\x\\AppData\\slack.exe", 1234),
            Category::Other
        );
    }

    #[test]
    fn live_states() {
        assert!(is_live_state(5));
        assert!(!is_live_state(2));
        assert!(!is_live_state(11));
    }

    #[test]
    fn name_sources() {
        assert!(!NameSource::None.is_named());
        assert!(NameSource::Cache.is_named());
        assert!(NameSource::Remembered.is_named());
        assert_eq!(NameSource::default(), NameSource::None);
    }

    #[test]
    fn sorting_prefers_live_then_recent() {
        let mut v = vec![
            DestAgg {
                dest: "a".into(),
                live: 0,
                last_seen: 100,
                ..Default::default()
            },
            DestAgg {
                dest: "b".into(),
                live: 2,
                last_seen: 5,
                ..Default::default()
            },
            DestAgg {
                dest: "c".into(),
                live: 0,
                last_seen: 900,
                ..Default::default()
            },
        ];
        sort_dests(&mut v);
        assert_eq!(v[0].dest, "b");
        assert_eq!(v[1].dest, "c");
        assert_eq!(v[2].dest, "a");
    }

    #[test]
    fn rate_formatting() {
        assert_eq!(fmt_rate(0.0), "0/s");
        assert_eq!(fmt_rate(999.0), "999/s");
        assert_eq!(fmt_rate(1500.0), "2k/s");
        assert_eq!(fmt_rate(2_500_000.0), "2.5M/s");
    }
}

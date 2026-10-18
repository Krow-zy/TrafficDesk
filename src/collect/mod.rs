//! The collector thread: one deterministic census per tick, aggregated and
//! persisted. Every Win32 failure is logged and degraded, never fatal.

pub mod alias;
pub mod dns;
pub mod dnscache;
pub mod net;
pub mod proc;
pub mod tcp;

use crate::config::{Config, ConfigWatcher, Paths};
use crate::model::{
    Category, ConnEvent, DestAgg, NameSource, ProcAgg, Snapshot, is_live_state, now_ms, sort_dests,
};
use crate::store::{Store, TickDest};
use alias::AliasBook;
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const SPARK_LEN: usize = 60;
/// Reading the DNS cache means one WMI round trip (~130 ms measured on a busy
/// cache), so it is polled rather than streamed: slow enough to stay cheap,
/// often enough that a new peer is named within a minute. Names are remembered
/// after that, so repeat traffic shows up immediately.
const DNS_REFRESH: Duration = Duration::from_secs(60);
/// Refresh cadence while some active peer still has no cached name.
const DNS_BUSY_REFRESH: Duration = Duration::from_secs(20);
const PRUNE_EVERY: Duration = Duration::from_secs(600);
/// Remembered names loaded at startup. Anything beyond this is ancient history.
const ALIAS_LOAD: usize = 4096;
/// A destination stays listed after its last connection closes for this long,
/// so short-lived conversations are visible rather than flashing past.
const RECENT_WINDOW: i64 = 10 * 60 * 1000;
/// How long those rows are kept around in memory.
const RECENT_KEEP: i64 = 60 * 60 * 1000;

pub struct Shared {
    pub snap: Snapshot,
    pub cfg: Config,
    pub note: Option<String>,
}

/// Last known state of a destination, kept briefly after it goes idle.
struct RecentDest {
    is_domain: bool,
    named: NameSource,
    named_at: i64,
    ip: String,
    pid: u32,
    process: String,
    path: String,
    events: u64,
    first_seen: i64,
    last_seen: i64,
}

/// Same, per process.
struct RecentProc {
    process: String,
    path: String,
    events: u64,
    dests: u32,
    last_seen: i64,
}

pub fn spawn(
    shared: Arc<Mutex<Shared>>,
    paths: Paths,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("trafficdesk-collector".into())
        .spawn(move || run(shared, paths, stop))
        .expect("spawn collector")
}

fn run(shared: Arc<Mutex<Shared>>, paths: Paths, stop: Arc<AtomicBool>) {
    let mut store = match Store::open(&paths.db) {
        Ok(s) => s,
        Err(e) => {
            log::error!("cannot open database: {e}");
            note(&shared, format!("database unavailable: {e}"));
            return;
        }
    };

    let mut cfg = match shared.lock() {
        Ok(s) => s.cfg.clone(),
        Err(_) => return,
    };
    let mut watcher = ConfigWatcher::default();
    // Prime the watcher so an unchanged file is not "reloaded" on the first tick.
    let _ = watcher.poll(&paths.config);

    let now = now_ms();
    let history = store
        .load_history(now - 86_400_000, 300)
        .unwrap_or_default();
    // Names learned in earlier runs, applied to history below so a row stored
    // while its peer was still anonymous shows its domain straight away.
    let mut aliases = AliasBook::from_rows(store.load_aliases(ALIAS_LOAD).unwrap_or_default());
    let mut dest_events: HashMap<String, u64> = HashMap::new();
    let mut proc_events: HashMap<u32, u64> = HashMap::new();
    let mut recent_dests: HashMap<String, RecentDest> = HashMap::new();
    let mut recent_procs: HashMap<u32, RecentProc> = HashMap::new();
    for d in history.dests {
        let mut dest = d.dest.clone();
        let mut named = if d.is_domain {
            NameSource::Cache
        } else {
            NameSource::None
        };
        let mut named_at = 0;
        if !d.is_domain
            && let Ok(ip) = d.ip.parse::<IpAddr>()
            && let Some((name, learned)) = aliases.get(&ip)
        {
            dest = name.to_string();
            named = NameSource::Remembered;
            named_at = learned;
        }
        *dest_events.entry(dest.clone()).or_default() += d.events;
        recent_dests.insert(
            dest.clone(),
            RecentDest {
                is_domain: named.is_named(),
                named,
                named_at,
                ip: d.ip,
                pid: d.pid,
                process: d.process,
                path: d.path,
                events: d.events,
                first_seen: d.first_seen,
                last_seen: d.last_seen,
            },
        );
    }
    for p in history.procs {
        *proc_events.entry(p.pid).or_default() += p.events;
        recent_procs.insert(
            p.pid,
            RecentProc {
                process: p.process,
                path: p.path,
                events: p.events,
                dests: p.dests,
                last_seen: p.last_seen,
            },
        );
    }

    let mut spark: VecDeque<(f64, f64)> =
        store.load_throughput(SPARK_LEN).unwrap_or_default().into();
    log::info!(
        "restored {} destinations, {} processes, {} remembered names and {} throughput samples from the last 24 h",
        dest_events.len(),
        proc_events.len(),
        aliases.len(),
        spark.len()
    );
    let mut prev_pairs: HashSet<(String, u32, IpAddr, u16)> = HashSet::new();
    let mut prev_counters: Option<(u64, u64, Instant)> = None;
    let mut dns_cache = dns::DnsCache::default();
    let dns_started = Instant::now();
    dns_cache.refresh();
    let mut pids = proc::PidMap::new();
    let mut dns_due = Instant::now();
    let mut last_prune = Instant::now();
    let self_pid = std::process::id();
    let mut tick = 0u64;
    // Set when the previous tick saw connections whose peer had no cached name.
    let mut unmapped_last_tick = false;
    // Names learned during the current tick, queued for persistence.
    let mut fresh_names: Vec<crate::store::NewAlias> = Vec::new();

    log::info!(
        "collector started: interval {} ms, retention {} d, {} cache entries in {:?} maps {} addresses",
        cfg.poll_interval_ms,
        cfg.retention_days,
        dns_cache.entries,
        dns_started.elapsed(),
        dns_cache.len()
    );

    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        tick += 1;
        let ts = now_ms();
        let mut errors: Vec<String> = Vec::new();
        fresh_names.clear();

        // --- config hot reload -------------------------------------------------
        if let Some((new_cfg, res)) = watcher.poll(&paths.config) {
            match res {
                Ok(()) => {
                    log::info!(
                        "config reloaded: interval {} ms, retention {} d, {} ignore rules",
                        new_cfg.poll_interval_ms,
                        new_cfg.retention_days,
                        new_cfg.ignore_processes.len()
                    );
                    cfg = new_cfg;
                    note(&shared, "config reloaded".to_string());
                }
                Err(e) => {
                    log::warn!("config reload failed: {e}");
                    note(&shared, format!("config error: {e}"));
                }
            }
        }

        // --- DNS cache --------------------------------------------------------
        // One WMI round trip (~130 ms), so poll it: sooner while peers are
        // still unnamed, slower once everything the cache holds is mapped.
        if Instant::now() >= dns_due {
            dns_cache.refresh();
            // Everything the cache knows right now is worth remembering: the
            // entry usually expires long before the endpoint stops being used.
            fresh_names = aliases.learn(dns_cache.iter().map(|(ip, name)| (*ip, name.clone())), ts);
            let next = if unmapped_last_tick {
                DNS_BUSY_REFRESH
            } else {
                DNS_REFRESH
            };
            dns_due = Instant::now() + next;
        }
        if let Some(e) = &dns_cache.error {
            errors.push(format!("dns: {e}"));
        }

        // --- throughput -------------------------------------------------------
        let counters = net::counters();
        let (up_bps, down_bps) = match (&counters, prev_counters) {
            (Ok((recv, sent)), Some((prev_recv, prev_sent, at))) => {
                let secs = at.elapsed().as_secs_f64().max(0.2);
                (
                    sent.saturating_sub(prev_sent) as f64 / secs,
                    recv.saturating_sub(prev_recv) as f64 / secs,
                )
            }
            _ => (0.0, 0.0),
        };
        if let Err(e) = &counters {
            errors.push(format!("iface: {e}"));
        }
        prev_counters = counters.ok().map(|(r, s)| (r, s, Instant::now()));

        // --- connections ------------------------------------------------------
        let conns = match tcp::enumerate() {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!("tcp: {e}"));
                Vec::new()
            }
        };
        let seen = conns.len();
        pids.refresh_if_due();

        let mut dests: HashMap<String, DestAgg> = HashMap::new();
        let mut procs: HashMap<u32, ProcAgg> = HashMap::new();
        let mut pairs_now: HashSet<(String, u32, IpAddr, u16)> = HashSet::new();
        let mut tick_pairs: HashMap<(String, u32), TickDest> = HashMap::new();
        let mut new_events: Vec<ConnEvent> = Vec::new();
        let mut unmapped = false;

        for c in &conns {
            if !is_live_state(c.state) || c.pid == self_pid {
                continue;
            }
            let info = pids.get(c.pid);
            if cfg.ignores(&info.name) {
                continue;
            }
            let ip = c.remote.to_string();
            let (dest, is_domain, named, named_at) = match dns_cache.get(&c.remote) {
                Some(name) => (name.to_string(), true, NameSource::Cache, 0),
                None => match aliases.get(&c.remote) {
                    Some((name, learned)) => {
                        (name.to_string(), true, NameSource::Remembered, learned)
                    }
                    None => {
                        unmapped = true;
                        (ip.clone(), false, NameSource::None, 0)
                    }
                },
            };
            let category = Category::classify(&info.name, &info.path, c.pid);

            let d = dests.entry(dest.clone()).or_insert_with(|| DestAgg {
                dest: dest.clone(),
                is_domain,
                named,
                named_at,
                ip: ip.clone(),
                first_seen: ts,
                last_seen: ts,
                top_process: info.name.clone(),
                top_pid: c.pid,
                top_path: info.path.clone(),
                category,
                ..Default::default()
            });
            d.live += 1;
            d.last_seen = ts;
            if is_domain {
                d.ip = ip.clone();
                d.named = named;
                d.named_at = named_at;
            }
            match d.procs.iter_mut().find(|(p, _)| p == &info.name) {
                Some(slot) => slot.1 += 1,
                None => d.procs.push((info.name.clone(), 1)),
            }

            let p = procs.entry(c.pid).or_insert_with(|| ProcAgg {
                pid: c.pid,
                process: info.name.clone(),
                path: info.path.clone(),
                last_seen: ts,
                category,
                ..Default::default()
            });
            p.live += 1;
            p.last_seen = ts;

            let pair = (dest.clone(), c.pid, c.remote, c.remote_port);
            let is_new = !prev_pairs.contains(&pair);
            pairs_now.insert(pair);
            if is_new {
                *dest_events.entry(dest.clone()).or_default() += 1;
                *proc_events.entry(c.pid).or_default() += 1;
                new_events.push(ConnEvent {
                    ts,
                    dest: dest.clone(),
                    is_domain,
                    ip: ip.clone(),
                    port: c.remote_port,
                    pid: c.pid,
                    process: info.name.clone(),
                    path: info.path.clone(),
                    state: c.state_name().to_string(),
                });
                let entry = tick_pairs
                    .entry((dest.clone(), c.pid))
                    .or_insert_with(|| TickDest {
                        dest: dest.clone(),
                        is_domain,
                        ip: ip.clone(),
                        pid: c.pid,
                        process: info.name.clone(),
                        path: info.path.clone(),
                        new_events: 0,
                        first_seen: ts,
                        last_seen: ts,
                    });
                entry.new_events += 1;
            }
        }
        prev_pairs = pairs_now;
        unmapped_last_tick = unmapped;

        // --- totals, and remember what was active this tick --------------------
        for d in &mut dests.values_mut() {
            d.events = *dest_events.get(&d.dest).unwrap_or(&0);
            d.procs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            if let Some((name, _)) = d.procs.first() {
                d.top_process = name.clone();
            }
            if let Some((pid, p)) = procs.iter().find(|(_, p)| p.process == d.top_process) {
                d.top_pid = *pid;
                d.top_path = p.path.clone();
            }
            recent_dests.insert(
                d.dest.clone(),
                RecentDest {
                    is_domain: d.is_domain,
                    named: d.named,
                    named_at: d.named_at,
                    ip: d.ip.clone(),
                    pid: d.top_pid,
                    process: d.top_process.clone(),
                    path: d.top_path.clone(),
                    events: d.events,
                    first_seen: d.first_seen,
                    last_seen: ts,
                },
            );
        }
        for p in &mut procs.values_mut() {
            p.events = *proc_events.get(&p.pid).unwrap_or(&0);
            p.dests = dests
                .values()
                .filter(|d| d.procs.iter().any(|(n, _)| n == &p.process))
                .count() as u32;
            recent_procs.insert(
                p.pid,
                RecentProc {
                    process: p.process.clone(),
                    path: p.path.clone(),
                    events: p.events,
                    dests: p.dests,
                    last_seen: ts,
                },
            );
        }

        // --- rows that went idle recently (live = 0) --------------------------
        for (name, r) in &recent_dests {
            if dests.contains_key(name) || ts - r.last_seen > RECENT_WINDOW {
                continue;
            }
            dests.insert(
                name.clone(),
                DestAgg {
                    dest: name.clone(),
                    is_domain: r.is_domain,
                    named: r.named,
                    named_at: r.named_at,
                    ip: r.ip.clone(),
                    live: 0,
                    events: r.events,
                    first_seen: r.first_seen,
                    last_seen: r.last_seen,
                    top_process: r.process.clone(),
                    top_pid: r.pid,
                    top_path: r.path.clone(),
                    category: Category::classify(&r.process, &r.path, r.pid),
                    procs: Vec::new(),
                },
            );
        }
        for (pid, r) in &recent_procs {
            if procs.contains_key(pid) || ts - r.last_seen > RECENT_WINDOW {
                continue;
            }
            procs.insert(
                *pid,
                ProcAgg {
                    pid: *pid,
                    process: r.process.clone(),
                    path: r.path.clone(),
                    live: 0,
                    events: r.events,
                    last_seen: r.last_seen,
                    dests: r.dests,
                    category: Category::classify(&r.process, &r.path, *pid),
                },
            );
        }
        recent_dests.retain(|_, r| ts - r.last_seen <= RECENT_KEEP);
        recent_procs.retain(|_, r| ts - r.last_seen <= RECENT_KEEP);

        // --- persist ----------------------------------------------------------
        let tick_dests: Vec<TickDest> = tick_pairs.into_values().collect();
        if let Err(e) = store.record(&tick_dests, &new_events, &fresh_names, up_bps, down_bps, ts) {
            log::warn!("database write failed: {e}");
            errors.push(format!("db: {e}"));
        }
        if !fresh_names.is_empty() {
            log::debug!(
                "learned {} new names ({} remembered)",
                fresh_names.len(),
                aliases.len()
            );
        }
        if last_prune.elapsed() >= PRUNE_EVERY {
            last_prune = Instant::now();
            match store.prune(cfg.retention_days, ts) {
                Ok(n) if n > 0 => log::info!("retention pruned {n} rows"),
                Ok(_) => {}
                Err(e) => log::warn!("retention prune failed: {e}"),
            }
        }

        // --- publish ----------------------------------------------------------
        spark.push_back((up_bps, down_bps));
        while spark.len() > SPARK_LEN {
            spark.pop_front();
        }
        let mut dest_vec: Vec<DestAgg> = dests.into_values().collect();
        sort_dests(&mut dest_vec);
        dest_vec.truncate(250);
        // The exact strings the panel is about to paint: handy when a name is
        // missing and you want to know why without a screenshot.
        if tick.is_multiple_of(30) && log::log_enabled!(log::Level::Debug) {
            let sample: Vec<String> = dest_vec
                .iter()
                .take(6)
                .map(|d| format!("{} [{}] live {} {}", d.dest, d.named.label(), d.live, d.ip))
                .collect();
            log::debug!("rows: {}", sample.join(" | "));
        }
        let mut proc_vec: Vec<ProcAgg> = procs.into_values().collect();
        proc_vec.sort_by(|a, b| {
            b.live
                .cmp(&a.live)
                .then(b.last_seen.cmp(&a.last_seen))
                .then(b.events.cmp(&a.events))
        });
        proc_vec.truncate(200);

        let snap = Snapshot {
            tick,
            up_bps,
            down_bps,
            dests: dest_vec,
            procs: proc_vec,
            spark: spark.iter().copied().collect(),
            errors,
            dns_entries: dns_cache.len(),
            dns_error: dns_cache.error.clone(),
            names_remembered: aliases.len(),
            connections_seen: seen,
            interval_ms: cfg.poll_interval_ms,
        };
        if let Ok(mut s) = shared.lock() {
            s.snap = snap;
        }

        let sleep = Duration::from_millis(cfg.poll_interval_ms).saturating_sub(started.elapsed());
        if !sleep.is_zero() {
            std::thread::sleep(sleep);
        }
    }
    log::info!("collector stopped");
}

fn note(shared: &Arc<Mutex<Shared>>, msg: String) {
    if let Ok(mut s) = shared.lock() {
        s.note = Some(msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn listen_and_time_wait_are_not_conversations() {
        assert!(is_live_state(5));
        assert!(!is_live_state(2));
        assert!(!is_live_state(11));
    }

    #[test]
    fn collector_gives_up_cleanly_when_the_database_is_unavailable() {
        let shared = Arc::new(Mutex::new(Shared {
            snap: Snapshot::default(),
            cfg: Config::default(),
            note: None,
        }));
        let dir = PathBuf::from("Z:\\trafficdesk-does-not-exist");
        let paths = Paths {
            config: dir.join("config.toml"),
            db: dir.join("trafficdesk.db"),
            log: dir.join("trafficdesk.log"),
            dir,
        };
        let stop = Arc::new(AtomicBool::new(false));
        let handle = spawn(shared.clone(), paths, stop);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !handle.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            handle.is_finished(),
            "collector kept running without a database"
        );
        // It must report the failure rather than panic the process.
        let note = shared.lock().map(|s| s.note.clone()).unwrap_or_default();
        assert!(
            note.unwrap_or_default().contains("database unavailable"),
            "missing note"
        );
        handle.join().expect("collector thread panicked");
    }
}

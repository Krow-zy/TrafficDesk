//! SQLite persistence: rolling connection events, per-destination rollups,
//! throughput samples and 14-day retention.

use crate::model::{ConnEvent, Ts};
use rusqlite::{Connection, OpenFlags, params};
use std::path::Path;

pub struct Store {
    conn: Connection,
}

/// One (destination, process) pair observed during a tick.
#[derive(Clone, Debug)]
pub struct TickDest {
    pub dest: String,
    pub is_domain: bool,
    pub ip: String,
    pub pid: u32,
    pub process: String,
    pub path: String,
    pub new_events: u64,
    pub first_seen: Ts,
    pub last_seen: Ts,
}

#[derive(Clone, Debug)]
pub struct HistoryDest {
    pub dest: String,
    pub is_domain: bool,
    pub ip: String,
    pub pid: u32,
    pub process: String,
    pub path: String,
    pub events: u64,
    pub first_seen: Ts,
    pub last_seen: Ts,
}

#[derive(Clone, Debug)]
pub struct HistoryProc {
    pub pid: u32,
    pub process: String,
    pub path: String,
    pub events: u64,
    pub last_seen: Ts,
    pub dests: u32,
}

#[derive(Clone, Debug, Default)]
pub struct History {
    pub dests: Vec<HistoryDest>,
    pub procs: Vec<HistoryProc>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS dest_stat(
  dest TEXT NOT NULL,
  is_domain INTEGER NOT NULL,
  ip TEXT NOT NULL,
  pid INTEGER NOT NULL,
  process TEXT NOT NULL,
  path TEXT NOT NULL,
  first_seen INTEGER NOT NULL,
  last_seen INTEGER NOT NULL,
  events INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(dest, pid, ip)
);
CREATE INDEX IF NOT EXISTS dest_stat_last ON dest_stat(last_seen);
CREATE TABLE IF NOT EXISTS conn_event(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts INTEGER NOT NULL,
  dest TEXT NOT NULL,
  is_domain INTEGER NOT NULL,
  ip TEXT NOT NULL,
  port INTEGER NOT NULL,
  pid INTEGER NOT NULL,
  process TEXT NOT NULL,
  path TEXT NOT NULL,
  state TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS conn_event_dest ON conn_event(dest, ts);
CREATE TABLE IF NOT EXISTS throughput(ts INTEGER PRIMARY KEY, up_bps REAL NOT NULL, down_bps REAL NOT NULL);
CREATE TABLE IF NOT EXISTS dest_alias(
  ip TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  learned INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS dest_alias_learned ON dest_alias(learned);
";

/// A name the DNS cache taught us, kept so the panel can show it again after
/// the cache entry itself is gone.
#[derive(Clone, Debug)]
pub struct NewAlias {
    pub ip: String,
    pub name: String,
    pub learned: Ts,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=2000;",
        )
        .map_err(|e| format!("pragma: {e}"))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| format!("schema: {e}"))?;
        Ok(Self { conn })
    }

    /// Persist one tick. All writes go in a single transaction.
    pub fn record(
        &mut self,
        pairs: &[TickDest],
        events: &[ConnEvent],
        aliases: &[NewAlias],
        up_bps: f64,
        down_bps: f64,
        ts: Ts,
    ) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        {
            // Names learned this tick are written down, and rows that were
            // stored while the IP was still anonymous take the name with them,
            // so history does not keep showing a bare address. Backfill runs
            // before the upserts below, otherwise the renamed row and the new
            // domain-keyed row would both exist.
            if !aliases.is_empty() {
                let mut rename = tx
                    .prepare_cached("UPDATE OR IGNORE dest_stat SET dest=?1, is_domain=1 WHERE ip=?2 AND is_domain=0")
                    .map_err(|e| e.to_string())?;
                for a in aliases {
                    rename
                        .execute(params![a.name, a.ip])
                        .map_err(|e| e.to_string())?;
                }
                let mut remember = tx
                    .prepare_cached(
                        "INSERT INTO dest_alias(ip,name,learned) VALUES(?1,?2,?3)
                         ON CONFLICT(ip) DO UPDATE SET name=excluded.name, learned=excluded.learned",
                    )
                    .map_err(|e| e.to_string())?;
                for a in aliases {
                    remember
                        .execute(params![a.ip, a.name, a.learned])
                        .map_err(|e| e.to_string())?;
                }
            }

            let mut upsert = tx
                .prepare_cached(
                    "INSERT INTO dest_stat(dest,is_domain,ip,pid,process,path,first_seen,last_seen,events)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
                     ON CONFLICT(dest,pid,ip) DO UPDATE SET
                       last_seen=excluded.last_seen,
                       process=excluded.process,
                       path=excluded.path,
                       events=dest_stat.events+excluded.events",
                )
                .map_err(|e| e.to_string())?;
            for p in pairs {
                upsert
                    .execute(params![
                        p.dest,
                        p.is_domain as i64,
                        p.ip,
                        p.pid as i64,
                        p.process,
                        p.path,
                        p.first_seen,
                        p.last_seen,
                        p.new_events as i64,
                    ])
                    .map_err(|e| e.to_string())?;
            }

            let mut insert = tx
                .prepare_cached(
                    "INSERT INTO conn_event(ts,dest,is_domain,ip,port,pid,process,path,state)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                )
                .map_err(|e| e.to_string())?;
            for e in events {
                insert
                    .execute(params![
                        e.ts,
                        e.dest,
                        e.is_domain as i64,
                        e.ip,
                        e.port as i64,
                        e.pid as i64,
                        e.process,
                        e.path,
                        e.state
                    ])
                    .map_err(|e| e.to_string())?;
            }

            tx.execute(
                "INSERT INTO throughput(ts,up_bps,down_bps) VALUES(?1,?2,?3)
                 ON CONFLICT(ts) DO UPDATE SET up_bps=excluded.up_bps, down_bps=excluded.down_bps",
                params![ts, up_bps, down_bps],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn load_history(&self, since: Ts, limit: usize) -> Result<History, String> {
        let mut dests = Vec::new();
        {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT dest,is_domain,ip,pid,process,path,SUM(events) ev,MIN(first_seen),MAX(last_seen)
                     FROM dest_stat WHERE last_seen>=?1 GROUP BY dest ORDER BY ev DESC LIMIT ?2",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![since, limit as i64], |r| {
                    Ok(HistoryDest {
                        dest: r.get(0)?,
                        is_domain: r.get::<_, i64>(1)? != 0,
                        ip: r.get(2)?,
                        pid: r.get::<_, i64>(3)? as u32,
                        process: r.get(4)?,
                        path: r.get(5)?,
                        events: r.get::<_, i64>(6)? as u64,
                        first_seen: r.get(7)?,
                        last_seen: r.get(8)?,
                    })
                })
                .map_err(|e| e.to_string())?;
            for row in rows {
                dests.push(row.map_err(|e| e.to_string())?);
            }
        }

        let mut procs = Vec::new();
        {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT pid,process,path,SUM(events) ev,MAX(last_seen),COUNT(DISTINCT dest)
                     FROM dest_stat WHERE last_seen>=?1 GROUP BY pid ORDER BY ev DESC LIMIT ?2",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![since, limit as i64], |r| {
                    Ok(HistoryProc {
                        pid: r.get::<_, i64>(0)? as u32,
                        process: r.get(1)?,
                        path: r.get(2)?,
                        events: r.get::<_, i64>(3)? as u64,
                        last_seen: r.get(4)?,
                        dests: r.get::<_, i64>(5)? as u32,
                    })
                })
                .map_err(|e| e.to_string())?;
            for row in rows {
                procs.push(row.map_err(|e| e.to_string())?);
            }
        }
        Ok(History { dests, procs })
    }

    /// Names learned in previous runs, newest first.
    pub fn load_aliases(&self, limit: usize) -> Result<Vec<(String, String, Ts)>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT ip,name,learned FROM dest_alias ORDER BY learned DESC LIMIT ?1")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![limit as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn load_throughput(&self, limit: usize) -> Result<Vec<(f64, f64)>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT up_bps,down_bps FROM throughput ORDER BY ts DESC LIMIT ?1")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![limit as i64], |r| {
                Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?))
            })
            .map_err(|e| e.to_string())?;
        let mut out: Vec<(f64, f64)> = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| e.to_string())?);
        }
        out.reverse();
        Ok(out)
    }

    /// Timeline of connection events for one destination, newest first.
    pub fn timeline(dest: &str, path: &Path, limit: usize) -> Result<Vec<ConnEvent>, String> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT ts,dest,is_domain,ip,port,pid,process,path,state FROM conn_event
                 WHERE dest=?1 ORDER BY ts DESC LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![dest, limit as i64], |r| {
                Ok(ConnEvent {
                    ts: r.get(0)?,
                    dest: r.get(1)?,
                    is_domain: r.get::<_, i64>(2)? != 0,
                    ip: r.get(3)?,
                    port: r.get::<_, i64>(4)? as u16,
                    pid: r.get::<_, i64>(5)? as u32,
                    process: r.get(6)?,
                    path: r.get(7)?,
                    state: r.get(8)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    pub fn prune(&mut self, retention_days: u32, now: Ts) -> Result<u64, String> {
        let cutoff = now - retention_days as i64 * 86_400_000;
        let spark_cutoff = now - 86_400_000;
        let mut removed = 0u64;
        for (sql, bound) in [
            ("DELETE FROM dest_stat WHERE last_seen < ?1", cutoff),
            ("DELETE FROM conn_event WHERE ts < ?1", cutoff),
            ("DELETE FROM throughput WHERE ts < ?1", spark_cutoff),
        ] {
            removed += self
                .conn
                .execute(sql, params![bound])
                .map_err(|e| e.to_string())? as u64;
        }
        // Remembered names follow their destinations out: an endpoint that has
        // not been seen inside the retention window is forgotten, which keeps
        // the table bounded by the number of live endpoints rather than by the
        // machine's uptime.
        removed += self
            .conn
            .execute(
                "DELETE FROM dest_alias WHERE ip NOT IN (SELECT DISTINCT ip FROM dest_stat)",
                [],
            )
            .map_err(|e| e.to_string())? as u64;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::now_ms;

    fn tmp_db(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("trafficdesk-test-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("t.db")
    }

    #[test]
    fn roundtrip_history_and_dedupe_pairs() {
        let path = tmp_db("roundtrip");
        let _ = std::fs::remove_file(&path);
        let mut store = Store::open(&path).unwrap();
        let now = now_ms();

        let pair = TickDest {
            dest: "example.com".into(),
            is_domain: true,
            ip: "93.184.216.34".into(),
            pid: 1234,
            process: "browser.exe".into(),
            path: "C:\\browser.exe".into(),
            new_events: 2,
            first_seen: now,
            last_seen: now,
        };
        store
            .record(
                std::slice::from_ref(&pair),
                &[ConnEvent {
                    ts: now,
                    dest: "example.com".into(),
                    is_domain: true,
                    ip: "93.184.216.34".into(),
                    port: 443,
                    pid: 1234,
                    process: "browser.exe".into(),
                    path: "C:\\browser.exe".into(),
                    state: "ESTAB".into(),
                }],
                &[],
                1000.0,
                2000.0,
                now,
            )
            .unwrap();
        // Second tick for the same pair must accumulate, not duplicate the row.
        store
            .record(
                &[TickDest {
                    new_events: 1,
                    last_seen: now + 1000,
                    ..pair
                }],
                &[],
                &[],
                1500.0,
                2500.0,
                now + 1000,
            )
            .unwrap();

        let history = store.load_history(now - 60_000, 100).unwrap();
        assert_eq!(history.dests.len(), 1);
        assert_eq!(history.dests[0].events, 3);
        assert_eq!(history.dests[0].last_seen, now + 1000);
        assert_eq!(history.procs.len(), 1);
        assert_eq!(history.procs[0].events, 3);

        let spark = store.load_throughput(60).unwrap();
        assert_eq!(spark.len(), 2);
        assert_eq!(spark[1], (1500.0, 2500.0));

        // Retention removes the same rows when the cutoff moves past them.
        let removed = store.prune(0, now + 2 * 86_400_000).unwrap();
        assert!(removed >= 2);
    }

    #[test]
    fn learned_names_rename_history_and_survive_restart() {
        let path = tmp_db("alias");
        let _ = std::fs::remove_file(&path);
        let now = now_ms();
        let mut store = Store::open(&path).unwrap();

        // First sighting: the peer had no name yet, so the row is keyed by IP.
        let anonymous = TickDest {
            dest: "203.0.113.7".into(),
            is_domain: false,
            ip: "203.0.113.7".into(),
            pid: 42,
            process: "app.exe".into(),
            path: "C:\\app.exe".into(),
            new_events: 1,
            first_seen: now,
            last_seen: now,
        };
        store
            .record(&[anonymous], &[], &[], 10.0, 20.0, now)
            .unwrap();

        // Later the DNS cache names it: the name is stored, the old row is
        // renamed rather than duplicated, and future ticks use the domain.
        let alias = NewAlias {
            ip: "203.0.113.7".into(),
            name: "cdn.example.net".into(),
            learned: now + 500,
        };
        let named = TickDest {
            dest: "cdn.example.net".into(),
            is_domain: true,
            ip: "203.0.113.7".into(),
            pid: 42,
            process: "app.exe".into(),
            path: "C:\\app.exe".into(),
            new_events: 1,
            first_seen: now + 500,
            last_seen: now + 500,
        };
        store
            .record(
                &[named],
                &[],
                std::slice::from_ref(&alias),
                30.0,
                40.0,
                now + 500,
            )
            .unwrap();

        let history = store.load_history(now - 60_000, 100).unwrap();
        assert_eq!(
            history.dests.len(),
            1,
            "renamed row must not coexist with the IP row: {:?}",
            history.dests
        );
        assert_eq!(history.dests[0].dest, "cdn.example.net");
        assert!(history.dests[0].is_domain);

        let aliases = store.load_aliases(10).unwrap();
        assert_eq!(
            aliases,
            vec![(
                "203.0.113.7".to_string(),
                "cdn.example.net".to_string(),
                now + 500
            )]
        );

        // Names whose destination has aged out are dropped by retention.
        store.prune(0, now + 2 * 86_400_000).unwrap();
        assert!(store.load_aliases(10).unwrap().is_empty());
    }
}

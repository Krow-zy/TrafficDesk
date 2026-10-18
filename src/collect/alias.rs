//! Remembered IP -> name pairs.
//!
//! The Windows DNS client cache is the only naming source (no reverse lookups,
//! no queries of our own). That cache is small and entries expire, so a name
//! seen once is copied here and persisted: an endpoint that the cache has since
//! dropped keeps its name. The book is capped, so its memory cannot grow with
//! uptime, and SQLite rows are pruned alongside the destinations they describe.

use crate::model::Ts;
use crate::store::NewAlias;
use std::collections::HashMap;
use std::net::IpAddr;

/// Roughly 400 KiB of names at the cap, and far more entries than a desktop
/// ever talks to inside the retention window.
const CAP: usize = 4096;

struct Entry {
    name: String,
    learned: Ts,
}

#[derive(Default)]
pub struct AliasBook {
    map: HashMap<IpAddr, Entry>,
}

impl AliasBook {
    /// Rebuild from persisted rows, which arrive newest first, so the cap keeps
    /// the most recently learned names.
    pub fn from_rows(rows: Vec<(String, String, Ts)>) -> Self {
        let mut map: HashMap<IpAddr, Entry> = HashMap::with_capacity(rows.len().min(CAP));
        for (ip, name, learned) in rows {
            if map.len() >= CAP {
                break;
            }
            let Ok(ip) = ip.parse::<IpAddr>() else {
                continue;
            };
            map.insert(ip, Entry { name, learned });
        }
        Self { map }
    }

    pub fn get(&self, ip: &IpAddr) -> Option<(&str, Ts)> {
        self.map.get(ip).map(|e| (e.name.as_str(), e.learned))
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Absorb names the DNS cache knows right now. Returns the ones worth
    /// writing down: new pairs, or a pair whose name changed. Callers persist
    /// only these, so a steady state writes nothing.
    pub fn learn(
        &mut self,
        named: impl Iterator<Item = (IpAddr, String)>,
        ts: Ts,
    ) -> Vec<NewAlias> {
        let mut fresh = Vec::new();
        for (ip, name) in named {
            if self.map.get(&ip).is_some_and(|e| e.name == name) {
                continue;
            }
            self.map.insert(
                ip,
                Entry {
                    name: name.clone(),
                    learned: ts,
                },
            );
            fresh.push(NewAlias {
                ip: ip.to_string(),
                name,
                learned: ts,
            });
        }
        if self.map.len() > CAP {
            self.trim();
        }
        fresh
    }

    /// Drop the oldest entries back down to the cap.
    fn trim(&mut self) {
        let mut ages: Vec<(Ts, IpAddr)> = self.map.iter().map(|(ip, e)| (e.learned, *ip)).collect();
        ages.sort_unstable();
        for (_, ip) in ages.into_iter().take(self.map.len() - CAP) {
            self.map.remove(&ip);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn learns_persists_and_remembers() {
        let mut book = AliasBook::default();
        let fresh = book.learn(
            [(ip("1.1.1.1"), "one.example".to_string())].into_iter(),
            100,
        );
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].name, "one.example");
        // Same name again: nothing new to write.
        assert!(
            book.learn(
                [(ip("1.1.1.1"), "one.example".to_string())].into_iter(),
                200
            )
            .is_empty()
        );
        assert_eq!(
            book.get(&ip("1.1.1.1")).map(|(n, _)| n),
            Some("one.example")
        );
        assert!(book.get(&ip("2.2.2.2")).is_none());
    }

    #[test]
    fn renamed_endpoint_is_written_again() {
        let mut book = AliasBook::default();
        book.learn(
            [(ip("1.1.1.1"), "old.example".to_string())].into_iter(),
            100,
        );
        let fresh = book.learn(
            [(ip("1.1.1.1"), "new.example".to_string())].into_iter(),
            300,
        );
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].name, "new.example");
        assert_eq!(book.get(&ip("1.1.1.1")), Some(("new.example", 300)));
    }

    #[test]
    fn rows_round_trip_and_keep_newest_at_the_cap() {
        let newest = CAP + 49;
        let addr = |i: usize| format!("10.0.{}.{}", i / 250, i % 250);
        let mut rows = Vec::new();
        for i in 0..=newest {
            rows.push((addr(i), format!("h{i}.example"), i as Ts));
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r.2)); // newest first, as SQL returns them
        let book = AliasBook::from_rows(rows);
        assert_eq!(book.len(), CAP);
        assert!(
            book.get(&ip(&addr(newest))).is_some(),
            "newest row should survive"
        );
        assert!(
            book.get(&ip(&addr(0))).is_none(),
            "oldest row should be dropped"
        );
    }

    #[test]
    fn trim_keeps_the_last_learned() {
        let mut book = AliasBook::default();
        for i in 0..CAP + 10 {
            book.learn(
                [(
                    ip(&format!("10.1.{}.{}", i / 250, i % 250)),
                    format!("h{i}"),
                )]
                .into_iter(),
                i as Ts,
            );
        }
        assert!(book.len() <= CAP);
        assert!(
            book.get(&ip(&format!(
                "10.1.{}.{}",
                (CAP + 9) / 250,
                (CAP + 9) % 250
            )))
            .is_some()
        );
    }
}

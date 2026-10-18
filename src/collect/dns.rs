//! Remote IP -> domain, taken only from Windows' own DNS cache.
//!
//! The records come straight from the cache (see [`super::dnscache`]), which
//! already pairs every cached name with its addresses, so no lookup of any kind
//! is issued: looking at traffic can never create traffic or pollute the cache.
//! Anything the cache does not know stays an IP - by design.
//!
//! Cache entries expire quickly while connections live for a long time, so the
//! collector copies every name it sees into its own name memory (see
//! `collect::alias`).

use std::collections::HashMap;
use std::net::IpAddr;

const DNS_TYPE_A: u16 = 1;
const DNS_TYPE_PTR: u16 = 12;
const DNS_TYPE_AAAA: u16 = 28;

#[derive(Default)]
pub struct DnsCache {
    map: HashMap<IpAddr, String>,
    /// Records the cache held at the last refresh.
    pub entries: usize,
    pub error: Option<String>,
}

impl DnsCache {
    pub fn get(&self, ip: &IpAddr) -> Option<&str> {
        self.map.get(ip).map(|s| s.as_str())
    }

    /// Everything currently mapped, for the name memory to absorb.
    pub fn iter(&self) -> impl Iterator<Item = (&IpAddr, &String)> {
        self.map.iter()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Refresh from the OS cache. Failures keep the previous map and are
    /// recorded for the UI; they never propagate as an error or a panic.
    pub fn refresh(&mut self) {
        let records = match super::dnscache::records() {
            Ok(records) => records,
            Err(e) => {
                log::warn!("DNS cache read failed: {e}");
                self.error = Some(e);
                return;
            }
        };

        self.entries = records.len();
        let mut map: HashMap<IpAddr, String> = HashMap::with_capacity(records.len());
        for record in &records {
            match record.rtype {
                DNS_TYPE_A | DNS_TYPE_AAAA => {
                    let Ok(ip) = record.data.trim().parse::<IpAddr>() else {
                        continue;
                    };
                    if ip.is_unspecified() || ip.is_loopback() {
                        continue;
                    }
                    insert_preferred(&mut map, ip, &tidy_name(&record.name));
                }
                DNS_TYPE_PTR => {
                    // A PTR record is keyed by its reverse name, so the address
                    // comes from the entry name and the hostname from the data.
                    let Some(ip) = ip_from_reverse_name(&record.name) else {
                        continue;
                    };
                    if ip.is_unspecified() || ip.is_loopback() {
                        continue;
                    }
                    let host = tidy_name(&record.data);
                    if !host.is_empty() {
                        insert_preferred(&mut map, ip, &host);
                    }
                }
                _ => {}
            }
        }

        self.map = map;
        self.error = None;

        log::debug!(
            "dns refresh: {} cache records -> {} addresses mapped",
            self.entries,
            self.map.len()
        );
        if self.entries > 0 && self.map.is_empty() {
            log::warn!(
                "dns refresh mapped no addresses from {} records",
                self.entries
            );
        } else if log::log_enabled!(log::Level::Debug) {
            let mut samples: Vec<String> = self
                .map
                .iter()
                .take(10)
                .map(|(ip, n)| format!("{ip}={n}"))
                .collect();
            samples.sort();
            log::debug!("dns sample: {}", samples.join(" "));
        }
    }
}

/// Windows renders some cached labels with `---` where a dot belongs.
fn tidy_name(name: &str) -> String {
    name.trim_end_matches('.')
        .replace("---", ".")
        .to_ascii_lowercase()
}

/// Keep the shortest name when several describe one address: "docs.rs" beats
/// "docs.rs.cdn.example.net".
fn insert_preferred(map: &mut HashMap<IpAddr, String>, ip: IpAddr, name: &str) {
    match map.get(&ip) {
        Some(existing) if existing.len() <= name.len() => {}
        _ => {
            map.insert(ip, name.to_string());
        }
    }
}

/// `34.216.184.93.in-addr.arpa` -> `93.184.216.34`, and the ip6 nibble form
/// (least significant nibble first) back to its address.
fn ip_from_reverse_name(name: &str) -> Option<IpAddr> {
    let lower = name.to_ascii_lowercase();
    if let Some(body) = lower.strip_suffix(".in-addr.arpa") {
        let mut octets: Vec<&str> = body.split('.').collect();
        if octets.len() != 4 {
            return None;
        }
        octets.reverse();
        return octets
            .join(".")
            .parse::<std::net::Ipv4Addr>()
            .ok()
            .map(IpAddr::V4);
    }
    let body = lower.strip_suffix(".ip6.arpa")?;
    let nibbles: Vec<&str> = body.split('.').collect();
    if nibbles.len() != 32
        || !nibbles
            .iter()
            .all(|n| n.len() == 1 && n.chars().all(|c| c.is_ascii_hexdigit()))
    {
        return None;
    }
    let hex: String = nibbles.iter().rev().copied().collect();
    let mut octets = [0u8; 16];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        octets[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(IpAddr::V6(std::net::Ipv6Addr::from(octets)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverse_names_decode_back_to_their_address() {
        // Reverse names list the octets least significant first, so the name
        // for 93.184.216.34 is "34.216.184.93.in-addr.arpa".
        assert_eq!(
            ip_from_reverse_name("34.216.184.93.in-addr.arpa"),
            Some("93.184.216.34".parse::<IpAddr>().unwrap())
        );
        // IPv6 nibbles arrive least significant first.
        let v6 = "2001:db8::1".parse::<std::net::Ipv6Addr>().unwrap();
        let mut nibbles: Vec<String> = v6
            .octets()
            .iter()
            .flat_map(|b| {
                format!("{b:02x}")
                    .chars()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
            })
            .collect();
        nibbles.reverse();
        let name = format!("{}.ip6.arpa", nibbles.join("."));
        assert_eq!(ip_from_reverse_name(&name), Some(IpAddr::V6(v6)));
        // Rubbish must not panic or invent an address.
        assert_eq!(ip_from_reverse_name("not-a-reverse-name.example"), None);
        assert_eq!(ip_from_reverse_name("1.2.3.in-addr.arpa"), None);
        assert_eq!(ip_from_reverse_name(".in-addr.arpa"), None);
    }

    #[test]
    fn tidy_name_uses_dots_and_lowercase() {
        assert_eq!(tidy_name("WWW---Example---COM."), "www.example.com");
        assert_eq!(tidy_name("plain.example.com"), "plain.example.com");
    }

    #[test]
    fn shortest_name_wins_for_a_shared_address() {
        let mut map = HashMap::new();
        let ip: IpAddr = "203.0.113.9".parse().unwrap();
        insert_preferred(&mut map, ip, "docs.rs.cdn.example.net");
        insert_preferred(&mut map, ip, "docs.rs");
        assert_eq!(map.get(&ip).map(String::as_str), Some("docs.rs"));
        // A longer name never displaces the shorter one.
        insert_preferred(&mut map, ip, "a.much.longer.name.example");
        assert_eq!(map.get(&ip).map(String::as_str), Some("docs.rs"));
    }

    #[test]
    fn refresh_never_panics_and_reports_entries() {
        let mut cache = DnsCache::default();
        cache.refresh();
        // An empty cache is legal; a hard error is not.
        assert!(
            cache.error.is_none(),
            "unexpected cache error: {:?}",
            cache.error
        );
        for (ip, name) in &cache.map {
            assert!(!name.is_empty());
            assert!(!ip.is_unspecified());
            assert!(!ip.is_loopback());
        }
    }

    /// Diagnostic: how much of the live cache the reader sees, and how long the
    /// query takes. `cargo test --release -- --ignored --nocapture dnscache_reads`
    #[test]
    #[ignore = "reads the machine's live DNS cache"]
    fn dnscache_reads_live_records() {
        let started = std::time::Instant::now();
        let records = super::super::dnscache::records().unwrap();
        let elapsed = started.elapsed();
        let addresses = records
            .iter()
            .filter(|r| r.rtype == DNS_TYPE_A || r.rtype == DNS_TYPE_AAAA)
            .filter(|r| r.data.parse::<IpAddr>().is_ok())
            .count();
        println!(
            "{} cache records ({} with a usable address) read in {:?}",
            records.len(),
            addresses,
            elapsed
        );
        for r in records.iter().take(10) {
            println!("  {} type {} = {}", r.name, r.rtype, r.data);
        }
    }

    /// Manual cross-check against `Get-DnsClientCache`:
    /// `cargo test dump_cache_mappings -- --ignored --nocapture`
    #[test]
    #[ignore = "manual cross-check against Get-DnsClientCache"]
    fn dump_cache_mappings() {
        let mut cache = DnsCache::default();
        let started = std::time::Instant::now();
        cache.refresh();
        let mut rows: Vec<(&IpAddr, &String)> = cache.map.iter().collect();
        rows.sort_by(|a, b| a.1.cmp(b.1));
        println!(
            "{} cache records, {} addresses mapped in {:?}",
            cache.entries,
            rows.len(),
            started.elapsed()
        );
        for (ip, name) in rows.iter().take(25) {
            println!("{ip} => {name}");
        }
    }
}

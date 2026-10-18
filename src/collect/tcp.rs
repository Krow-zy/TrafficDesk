//! Full TCP connection enumeration.
//!
//! `GetExtendedTcpTable(TCP_TABLE_OWNER_PID_ALL)` returns every socket in the
//! system for the caller's session, so each tick is a complete census rather
//! than a sample. Connections that open and close inside one poll interval are
//! invisible to this API; only byte counters or ETW would catch those.

use crate::model::Conn;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID,
    MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_ALL,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};

const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

pub fn enumerate() -> Result<Vec<Conn>, String> {
    let mut out = Vec::with_capacity(128);
    let mut first_err: Option<String> = None;

    match table_v4() {
        Ok(rows) => out.extend(rows),
        Err(e) => {
            log::warn!("TCP v4 enumeration failed: {e}");
            first_err = Some(e);
        }
    }
    match table_v6() {
        Ok(rows) => out.extend(rows),
        Err(e) => {
            log::warn!("TCP v6 enumeration failed: {e}");
            if first_err.is_none() {
                first_err = Some(e);
            }
        }
    }

    match (out.is_empty(), first_err) {
        (true, Some(e)) => Err(e),
        _ => Ok(out),
    }
}

/// Size query + fetch, into a `u32`-aligned scratch buffer.
fn raw_table(family: u16, class: i32) -> Result<Vec<u32>, String> {
    let mut size: u32 = 0;
    let r =
        unsafe { GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, family as u32, class, 0) };
    if r != 0 && r != ERROR_INSUFFICIENT_BUFFER {
        return Err(format!("GetExtendedTcpTable(size) error {r}"));
    }
    if size == 0 {
        return Ok(Vec::new());
    }
    let mut buf = vec![0u32; size.div_ceil(4) as usize];
    let r = unsafe {
        GetExtendedTcpTable(
            buf.as_mut_ptr().cast(),
            &mut size,
            0,
            family as u32,
            class,
            0,
        )
    };
    if r != 0 {
        return Err(format!("GetExtendedTcpTable(data) error {r}"));
    }
    Ok(buf)
}

fn port(raw: u32) -> u16 {
    u16::from_be((raw & 0xFFFF) as u16)
}

fn v4(addr: u32) -> Ipv4Addr {
    // The DWORD holds the address in network byte order; the byte order in
    // memory is what we want.
    Ipv4Addr::from(addr.to_le_bytes())
}

fn interesting(remote: IpAddr) -> bool {
    match remote {
        IpAddr::V4(a) => !a.is_unspecified() && !a.is_loopback() && !a.is_broadcast(),
        IpAddr::V6(a) => !a.is_unspecified() && !a.is_loopback(),
    }
}

fn table_v4() -> Result<Vec<Conn>, String> {
    let buf = raw_table(AF_INET, TCP_TABLE_OWNER_PID_ALL)?;
    if buf.is_empty() {
        return Ok(Vec::new());
    }
    let table = buf.as_ptr().cast::<MIB_TCPTABLE_OWNER_PID>();
    let n = unsafe { (*table).dwNumEntries } as usize;
    let rows = unsafe { (*table).table.as_ptr() };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let row: &MIB_TCPROW_OWNER_PID = unsafe { &*rows.add(i) };
        let remote = IpAddr::V4(v4(row.dwRemoteAddr));
        if !interesting(remote) {
            continue;
        }
        out.push(Conn {
            pid: row.dwOwningPid,
            remote,
            remote_port: port(row.dwRemotePort),
            state: row.dwState,
        });
    }
    Ok(out)
}

fn table_v6() -> Result<Vec<Conn>, String> {
    let buf = raw_table(AF_INET6, TCP_TABLE_OWNER_PID_ALL)?;
    if buf.is_empty() {
        return Ok(Vec::new());
    }
    let table = buf.as_ptr().cast::<MIB_TCP6TABLE_OWNER_PID>();
    let n = unsafe { (*table).dwNumEntries } as usize;
    let rows = unsafe { (*table).table.as_ptr() };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let row: &MIB_TCP6ROW_OWNER_PID = unsafe { &*rows.add(i) };
        let remote = IpAddr::V6(Ipv6Addr::from(row.ucRemoteAddr));
        if !interesting(remote) {
            continue;
        }
        out.push(Conn {
            pid: row.dwOwningPid,
            remote,
            remote_port: port(row.dwRemotePort),
            state: row.dwState,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_decodes_network_order_low_word() {
        // The API returns the port in network byte order in the low 16 bits, so
        // port 443 arrives as the DWORD 0x0000_BB01 on a little-endian machine.
        assert_eq!(port(0x0000_BB01), 443);
        assert_eq!(port(0x0000_5000), 80);
        assert_eq!(port(0xFFFF_0050), 0x5000);
    }

    #[test]
    fn ipv4_dword_decodes_to_expected_octets() {
        // 93.184.216.34 -> 0x5DB8D822 in network order, stored little-endian.
        assert_eq!(v4(0x22D8B85D).to_string(), "93.184.216.34");
    }

    #[test]
    fn filters_loopback_and_unspecified() {
        assert!(!interesting(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(!interesting(IpAddr::V4(Ipv4Addr::UNSPECIFIED)));
        assert!(interesting(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
    }

    #[test]
    fn enumeration_never_errors_on_this_machine() {
        // Empty is fine; an error here means the FFI plumbing is wrong.
        let rows = enumerate().expect("enumerate");
        for c in rows {
            assert_ne!(c.remote_port, 0);
        }
    }
}

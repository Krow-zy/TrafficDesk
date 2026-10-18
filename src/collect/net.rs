//! Global interface byte counters (upload/download throughput).
//!
//! Per-connection byte counts need ETW; this is the honest global number.

use windows_sys::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIfTable2, IF_TYPE_SOFTWARE_LOOPBACK, IF_TYPE_TUNNEL, MIB_IF_TABLE2,
};

/// `IF_OPER_STATUS_UP`
const IF_OPER_STATUS_UP: i32 = 1;

/// Total (received, sent) bytes across up, non-loopback, non-tunnel interfaces.
pub fn counters() -> Result<(u64, u64), String> {
    let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
    let rc = unsafe { GetIfTable2(&mut table) };
    if rc != 0 {
        return Err(format!("GetIfTable2 error {rc}"));
    }
    if table.is_null() {
        return Err("GetIfTable2 returned no table".to_string());
    }

    let mut received = 0u64;
    let mut sent = 0u64;
    unsafe {
        let count = (*table).NumEntries as usize;
        let base = (*table).Table.as_ptr();
        for i in 0..count {
            let row = &*base.add(i);
            if row.OperStatus != IF_OPER_STATUS_UP {
                continue;
            }
            if row.Type == IF_TYPE_SOFTWARE_LOOPBACK || row.Type == IF_TYPE_TUNNEL {
                continue;
            }
            received = received.saturating_add(row.InOctets);
            sent = sent.saturating_add(row.OutOctets);
        }
        FreeMibTable(table.cast());
    }
    Ok((received, sent))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_are_monotonic_across_calls() {
        let first = counters().expect("first read");
        std::thread::sleep(std::time::Duration::from_millis(200));
        let second = counters().expect("second read");
        // Counters only wrap on adapter reset; a decrease here would mean we are
        // summing the wrong rows.
        assert!(second.0 >= first.0 || first.0 > u64::MAX / 2);
        assert!(second.1 >= first.1 || first.1 > u64::MAX / 2);
    }
}

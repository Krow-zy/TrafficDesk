//! PID -> process name / image path.

use std::collections::HashMap;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};

#[derive(Clone, Debug, Default)]
pub struct ProcInfo {
    pub name: String,
    pub path: String,
}

/// Caches lookups and refreshes the Toolhelp32 process list periodically so dead
/// PIDs do not pin stale (or reused) names forever.
pub struct PidMap {
    cache: HashMap<u32, ProcInfo>,
    snapshot: HashMap<u32, String>,
    last_full: Option<Instant>,
}

impl Default for PidMap {
    fn default() -> Self {
        Self::new()
    }
}

impl PidMap {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
            snapshot: HashMap::new(),
            last_full: None,
        }
    }

    pub fn refresh_if_due(&mut self) {
        let due = self
            .last_full
            .map(|t| t.elapsed() > Duration::from_secs(20))
            .unwrap_or(true);
        if !due {
            return;
        }
        self.last_full = Some(Instant::now());
        match snapshot_processes() {
            Ok(map) => {
                self.cache.retain(|pid, _| map.contains_key(pid));
                self.snapshot = map;
            }
            Err(e) => log::warn!("process snapshot failed: {e}"),
        }
    }

    pub fn get(&mut self, pid: u32) -> ProcInfo {
        if let Some(info) = self.cache.get(&pid).filter(|i| !i.name.is_empty()) {
            return info.clone();
        }
        let info = match image_path(pid) {
            Some(path) => ProcInfo {
                name: base_name(&path),
                path,
            },
            None => ProcInfo {
                name: self
                    .snapshot
                    .get(&pid)
                    .cloned()
                    .unwrap_or_else(|| format!("pid {pid}")),
                path: String::new(),
            },
        };
        self.cache.insert(pid, info.clone());
        info
    }
}

fn base_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_string()
}

fn image_path(pid: u32) -> Option<String> {
    match pid {
        0 => return Some("System Idle Process".to_string()),
        4 => return Some("System".to_string()),
        _ => {}
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(handle);
        if ok == 0 || len == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn snapshot_processes() -> Result<HashMap<u32, String>, String> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE || snap.is_null() {
            return Err("CreateToolhelp32Snapshot failed".to_string());
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut map = HashMap::new();
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let name_len = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                map.insert(
                    entry.th32ProcessID,
                    String::from_utf16_lossy(&entry.szExeFile[..name_len]),
                );
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
        Ok(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_name_extracts_exe() {
        assert_eq!(
            base_name("C:\\Windows\\System32\\svchost.exe"),
            "svchost.exe"
        );
    }

    #[test]
    fn maps_own_process() {
        let mut map = PidMap::new();
        map.refresh_if_due();
        let info = map.get(std::process::id());
        assert!(info.name.to_ascii_lowercase().contains("test") || !info.name.is_empty());
        assert!(info.path.to_ascii_lowercase().ends_with(".exe"));
    }

    #[test]
    fn bogus_pid_falls_back_without_panicking() {
        let mut map = PidMap::new();
        let info = map.get(0xFFFF_FFFE);
        assert!(!info.name.is_empty());
    }
}

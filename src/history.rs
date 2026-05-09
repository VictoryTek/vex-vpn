//! Connection history — append-only JSONL log at
//! ~/.local/state/vex-vpn/history.jsonl

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HistoryEntry {
    pub ts_start: u64,
    pub ts_end: u64,
    pub region: String,
    pub bytes_rx: u64,
    pub bytes_tx: u64,
    pub disconnect_reason: String,
}

pub fn history_path() -> PathBuf {
    let base = std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".local").join("state")
        });
    base.join("vex-vpn").join("history.jsonl")
}

/// Append one completed-session entry to the JSONL log.
/// The file and its parent directory are created on demand.
/// I/O errors are logged and swallowed — history is best-effort.
pub fn append_entry(entry: &HistoryEntry) {
    let path = history_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let line = match serde_json::to_string(entry) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("history: failed to serialize entry: {}", e);
            return;
        }
    };
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut f) => {
            let _ = writeln!(f, "{}", line);
        }
        Err(e) => tracing::warn!("history: failed to open {:?}: {}", path, e),
    }
}

/// Read the most recent `n` entries in reverse-chronological order.
/// Returns an empty Vec on any I/O or parse error.
pub fn load_recent(n: usize) -> Vec<HistoryEntry> {
    let content = match std::fs::read_to_string(history_path()) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut entries: Vec<HistoryEntry> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    // Return newest first — reverse the order and take at most n.
    let start = entries.len().saturating_sub(n);
    entries.drain(..start);
    entries.reverse();
    entries
}

/// Format a duration in seconds as a human-readable string.
pub fn format_duration(seconds: u64) -> String {
    if seconds >= 3600 {
        format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60)
    } else if seconds >= 60 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{}s", seconds)
    }
}

/// Format a Unix timestamp as a relative date string.
pub fn format_timestamp(ts: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let age = now.saturating_sub(ts);
    let hour = (ts % 86400) / 3600;
    let min = (ts % 3600) / 60;
    if age < 86400 {
        format!("Today {:02}:{:02}", hour, min)
    } else if age < 172800 {
        format!("Yesterday {:02}:{:02}", hour, min)
    } else {
        format!("{} days ago {:02}:{:02}", age / 86400, hour, min)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(125), "2m 5s");
        assert_eq!(format_duration(7530), "2h 5m");
    }

    #[test]
    fn test_round_trip_jsonl() {
        let e = HistoryEntry {
            ts_start: 1_700_000_000,
            ts_end: 1_700_000_330,
            region: "US East".to_string(),
            bytes_rx: 1024,
            bytes_tx: 512,
            disconnect_reason: "user".to_string(),
        };
        let line = serde_json::to_string(&e).unwrap();
        let decoded: HistoryEntry = serde_json::from_str(&line).unwrap();
        assert_eq!(decoded.region, e.region);
        assert_eq!(decoded.bytes_rx, e.bytes_rx);
    }

    #[test]
    fn test_load_recent_empty() {
        // Set XDG_STATE_HOME to a temp dir to avoid touching real history.
        let dir = std::env::temp_dir().join("vex_vpn_test_history_empty");
        let prev = std::env::var("XDG_STATE_HOME").ok();
        std::env::set_var("XDG_STATE_HOME", &dir);
        let entries = load_recent(10);
        assert!(entries.is_empty());
        if let Some(p) = prev {
            std::env::set_var("XDG_STATE_HOME", p);
        } else {
            std::env::remove_var("XDG_STATE_HOME");
        }
    }

    #[test]
    fn test_history_path_respects_xdg_state_home() {
        std::env::set_var("XDG_STATE_HOME", "/tmp/test_state");
        let path = history_path();
        assert_eq!(
            path,
            std::path::PathBuf::from("/tmp/test_state/vex-vpn/history.jsonl")
        );
        std::env::remove_var("XDG_STATE_HOME");
    }
}

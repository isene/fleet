//! Inbox scan: items recently arrived in the watched drop points.
//!
//! One readdir per watch per refresh, mtime-filtered. Dotfiles and
//! directories are skipped so a `*` glob on a busy folder stays sane.

use crate::config::{glob_match, Config};
use std::path::PathBuf;
use std::time::SystemTime;

pub struct Item {
    pub label: String,
    pub path: PathBuf,
    pub name: String,
    pub age_secs: u64,
}

pub struct LogEntry {
    pub dest: String,
    pub text: String,
    pub ts: u64,
    /// The mailbox file, for rows that still have one. Delivered traffic
    /// in the log popup has none: the file is gone by then.
    pub path: Option<PathBuf>,
}

/// Messages sitting in the bus and relay mailboxes, not yet delivered.
/// Newest first. These rows live in the INBOX pane; delivered traffic
/// lives in the log popup.
pub fn pending() -> Vec<LogEntry> {
    let now = SystemTime::now();
    let mut out = Vec::new();
    for root in [crate::config::home().join(".fleet/bus"),
                 crate::config::home().join(".fleet/relay")] {
        let dirs = match std::fs::read_dir(&root) {
            Ok(d) => d,
            Err(_) => continue,
        };
        for d in dirs.flatten() {
            let dest = d.file_name().to_string_lossy().to_string();
            let files = match std::fs::read_dir(d.path()) {
                Ok(f) => f,
                Err(_) => continue,
            };
            for f in files.flatten() {
                let p = f.path();
                if p.extension().map(|e| e != "msg").unwrap_or(true) {
                    continue;
                }
                let ts = f
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or_else(|| {
                        now.duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0)
                    });
                let text = std::fs::read_to_string(&p).unwrap_or_default();
                let text: String = text
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(120)
                    .collect();
                out.push(LogEntry { dest: dest.clone(), text, ts, path: Some(p) });
            }
        }
    }
    out.sort_by(|a, b| b.ts.cmp(&a.ts));
    out
}

/// Tail of ~/.fleet/log: the delivered bus traffic, newest first.
/// The hook appends deliveries; fleet appends phone-bound sends it sees.
pub fn log_tail(n: usize) -> Vec<LogEntry> {
    let path = crate::config::home().join(".fleet/log");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    text.lines()
        .rev()
        .take(n)
        .filter_map(|l| {
            let mut f = l.splitn(3, '\t');
            let ts: u64 = f.next()?.parse().ok()?;
            let dest = f.next()?.to_string();
            let text = f.next()?.to_string();
            Some(LogEntry { dest, text, ts, path: None })
        })
        .collect()
}

pub fn scan(cfg: &Config) -> Vec<Item> {
    let now = SystemTime::now();
    let max_age = cfg.inbox_days * 86400;
    let mut out = Vec::new();
    for w in &cfg.watches {
        let entries = match std::fs::read_dir(&w.dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') && !w.glob.starts_with('.') {
                continue;
            }
            if !glob_match(&w.glob, &name) {
                continue;
            }
            let md = match e.metadata() {
                Ok(m) if m.is_file() => m,
                _ => continue,
            };
            let age = md
                .modified()
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .map(|d| d.as_secs())
                .unwrap_or(u64::MAX);
            if age <= max_age {
                out.push(Item {
                    label: w.label.clone(),
                    path: e.path(),
                    name,
                    age_secs: age,
                });
            }
        }
    }
    out.sort_by_key(|i| i.age_secs);
    out
}

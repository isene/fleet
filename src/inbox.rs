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
            Some(LogEntry { dest, text, ts })
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

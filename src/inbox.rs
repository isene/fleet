//! Inbox scan: items recently arrived in the watched drop points.
//!
//! One readdir per watch per refresh, mtime-filtered. Dotfiles and
//! directories are skipped so a `*` glob on a busy folder stays sane.

use crate::config::{glob_match, Config};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub struct Item {
    pub label: String,
    pub path: PathBuf,
    pub name: String,
    pub age_secs: u64,
}

pub struct LogEntry {
    pub dest: String,
    /// Sender tag, from the message's trailing `-- <tag>` line. "?" when
    /// unsigned. Empty for delivered-log rows, whose sender is not kept.
    pub from: String,
    pub text: String,
    pub ts: u64,
    /// The mailbox file, for rows that still have one. Delivered traffic
    /// in the log popup has none: the file is gone by then.
    pub path: Option<PathBuf>,
}

/// The sender tag a message signs off with. Two conventions in the
/// wild: bus messages end `-- <tag>`, phone-relay ones `/<tag>` (often
/// `/asm (chasm session)`). Returns the bare tag, no sigil. None when
/// unsigned.
fn parse_sender(raw: &str) -> Option<String> {
    for line in raw.lines().rev() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("-- ") {
            let tag = rest.split_whitespace().next().unwrap_or("");
            if !tag.is_empty() {
                return Some(tag.to_string());
            }
        }
        if let Some(rest) = l.strip_prefix('/') {
            // `/freewill` or `/asm (chasm session)` — the tag is the
            // first word, letters/digits/-/_ only (skip real paths).
            let tag: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            if !tag.is_empty() && tag.len() == rest.split_whitespace().next().unwrap_or("").len() {
                return Some(tag);
            }
        }
    }
    None
}

/// Messages sitting in the bus and relay mailboxes, not yet delivered.
/// Newest first. These rows live in the INBOX pane; delivered traffic
/// lives in the log popup.
/// A message the phone sent: the fleet server names it <time>-phone.msg.
pub fn from_phone(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with("-phone.msg"))
}

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
            // Skip Syncthing's own marker dir (.stfolder) and any dotdir.
            if dest.starts_with('.') {
                continue;
            }
            let files = match std::fs::read_dir(d.path()) {
                Ok(f) => f,
                Err(_) => continue,
            };
            for f in files.flatten() {
                let p = f.path();
                let name = f.file_name().to_string_lossy().to_string();
                // The bus stores one plain file per message, any name, no
                // extension (the receiving hook reads them all). Skip only
                // dotfiles (.stfolder) and subdirectories.
                if name.starts_with('.') {
                    continue;
                }
                if f.file_type().map(|t| !t.is_file()).unwrap_or(true) {
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
                let raw = std::fs::read_to_string(&p).unwrap_or_default();
                let from = parse_sender(&raw).unwrap_or_else(|| "?".into());
                let text: String = raw
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(120)
                    .collect();
                out.push(LogEntry { dest: dest.clone(), from, text, ts, path: Some(p) });
            }
        }
    }
    out.sort_by(|a, b| b.ts.cmp(&a.ts));
    out
}

/// Open and close each message waiting for `tag`, changing nothing. A
/// session that reads its own messages (deck under gluon) watches its
/// mailbox for a file closed after writing, so this asks it to read them.
pub fn nudge(tag: &str) -> usize {
    let home = crate::config::home();
    [".fleet/bus", ".fleet/relay"].iter().map(|r| nudge_in(&home.join(r).join(tag))).sum()
}

fn nudge_in(dir: &Path) -> usize {
    let Ok(files) = std::fs::read_dir(dir) else { return 0 };
    files
        .flatten()
        .filter(|f| !f.file_name().to_string_lossy().starts_with('.'))
        .filter(|f| f.file_type().is_ok_and(|t| t.is_file()))
        .filter(|f| std::fs::OpenOptions::new().append(true).open(f.path()).is_ok())
        .count()
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
            Some(LogEntry { dest, from: String::new(), text, ts, path: None })
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

#[cfg(test)]
mod tests {
    #[test]
    fn a_phone_message_is_known_by_its_name() {
        assert!(super::from_phone(std::path::Path::new("/r/ci/1761270000-phone.msg")));
        assert!(!super::from_phone(std::path::Path::new("/b/ci/1761270000-fe2o3")));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_nudge_fires_the_watch_and_keeps_the_message() {
        let dir = std::env::temp_dir().join(format!("fleet-nudge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let msg = dir.join("note");
        std::fs::write(&msg, "hi -- fe2o3").unwrap();
        let before = std::fs::metadata(&msg).unwrap().modified().unwrap();
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK) };
        let p = std::ffi::CString::new(dir.to_str().unwrap()).unwrap();
        unsafe { libc::inotify_add_watch(fd, p.as_ptr(), libc::IN_CLOSE_WRITE) };
        assert_eq!(super::nudge_in(&dir), 1);
        let mut buf = [0u8; 256];
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        unsafe { libc::close(fd) };
        let after = std::fs::metadata(&msg).unwrap().modified().unwrap();
        let text = std::fs::read_to_string(&msg).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(n > 0, "no IN_CLOSE_WRITE");
        assert_eq!(text, "hi -- fe2o3");
        assert_eq!(after, before);
    }
}

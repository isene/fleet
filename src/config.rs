//! ~/.fleetrc — plain text, whitespace-separated, '#' comments.
//!
//!   inbox <label> <dir> <glob>     a folder to watch for arriving items
//!   recent_days N                  sessions younger than this are listed
//!   idle_mins N                    older than this and a session is "idle"
//!   inbox_days N                   inbox items younger than this are shown
//!
//! Any `inbox` line in the file REPLACES the built-in watches, so other
//! users adapt fleet to their own drop points. The defaults encode this
//! machine's conventions: laptop screenshots land in ~ as *_scrot.png,
//! phone items (screenshots, files) land in ~/.transfer.

use std::path::PathBuf;

pub struct Watch {
    pub label: String,
    pub dir: PathBuf,
    pub glob: String,
}

pub struct Config {
    pub watches: Vec<Watch>,
    pub recent_days: u64,
    pub idle_mins: u64,
    pub inbox_days: u64,
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

fn expand(p: &str) -> PathBuf {
    if p == "~" {
        home()
    } else if let Some(rest) = p.strip_prefix("~/") {
        home().join(rest)
    } else {
        PathBuf::from(p)
    }
}

impl Config {
    pub fn load() -> Config {
        let mut cfg = Config {
            watches: Vec::new(),
            recent_days: 7,
            idle_mins: 30,
            inbox_days: 3,
        };
        let mut have_inbox = false;
        if let Ok(text) = std::fs::read_to_string(home().join(".fleetrc")) {
            for line in text.lines() {
                let line = line.split('#').next().unwrap_or("");
                let f: Vec<&str> = line.split_whitespace().collect();
                match f.as_slice() {
                    ["inbox", label, dir, glob] => {
                        have_inbox = true;
                        cfg.watches.push(Watch {
                            label: label.to_string(),
                            dir: expand(dir),
                            glob: glob.to_string(),
                        });
                    }
                    ["recent_days", n] => cfg.recent_days = n.parse().unwrap_or(cfg.recent_days),
                    ["idle_mins", n] => cfg.idle_mins = n.parse().unwrap_or(cfg.idle_mins),
                    ["inbox_days", n] => cfg.inbox_days = n.parse().unwrap_or(cfg.inbox_days),
                    _ => {}
                }
            }
        }
        if !have_inbox {
            cfg.watches.push(Watch {
                label: "scrots".into(),
                dir: home(),
                glob: "*_scrot.png".into(),
            });
            cfg.watches.push(Watch {
                label: "phone".into(),
                dir: home().join(".transfer"),
                glob: "*".into(),
            });
        }
        cfg
    }
}

/// Minimal glob: '*' matches any run, '?' one char. Case-sensitive.
pub fn glob_match(pat: &str, name: &str) -> bool {
    fn rec(p: &[u8], n: &[u8]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some(b'*'), _) => rec(&p[1..], n) || (!n.is_empty() && rec(p, &n[1..])),
            (Some(b'?'), Some(_)) => rec(&p[1..], &n[1..]),
            (Some(a), Some(b)) if a == b => rec(&p[1..], &n[1..]),
            _ => false,
        }
    }
    rec(pat.as_bytes(), name.as_bytes())
}

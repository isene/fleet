//! ~/.fleetrc — plain text, whitespace-separated, '#' comments.
//!
//!   inbox <label> <dir> <glob>     a folder to watch for arriving items
//!   recent_days N                  sessions younger than this are listed
//!   idle_mins N                    older than this and a session is "idle"
//!   inbox_days N                   inbox items younger than this are shown
//!   ctx_window_k N                 context window in k tokens (default 1000);
//!                                  CTX turns yellow at 50 % and red at 80 %
//!                                  of it, like the CC statusline
//!   session <tag> <ws> [bg]        where a resumed session's glass opens
//!                                  (ws is 1-based, or '-' for none; bg is
//!                                  the glass background as BARE hex, e.g.
//!                                  1a1a2e — no leading '#', which the file
//!                                  reads as a comment)
//!
//! Any `inbox` line in the file REPLACES the built-in watches, so other
//! users adapt fleet to their own drop points. The defaults encode this
//! machine's conventions: laptop screenshots land in ~ as *_scrot.png,
//! phone items (screenshots, files) land in ~/.transfer.

use std::collections::HashMap;
use std::path::PathBuf;

pub struct Watch {
    pub label: String,
    pub dir: PathBuf,
    pub glob: String,
}

/// Where a resumed session's glass opens, and in what colour. Both
/// optional: a tag may set only a workspace, only a background, or both.
#[derive(Clone, Default)]
pub struct SessionPref {
    pub ws: Option<u32>,      // 0-based; None = wherever fleet is
    pub bg: Option<String>,   // glass background hex, e.g. "#1a1a2e"
}

pub struct Config {
    pub watches: Vec<Watch>,
    pub recent_days: u64,
    pub idle_mins: u64,
    pub inbox_days: u64,
    pub ctx_window_k: u64,
    pub session_prefs: HashMap<String, SessionPref>,
}

/// A 1-based workspace field from the config ('-' = none) to 0-based.
fn parse_ws(s: &str) -> Option<u32> {
    if s == "-" {
        None
    } else {
        s.parse::<u32>().ok().map(|n| n.saturating_sub(1))
    }
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
            ctx_window_k: 1000,
            session_prefs: HashMap::new(),
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
                    ["ctx_window_k", n] => cfg.ctx_window_k = n.parse().unwrap_or(cfg.ctx_window_k),
                    ["inbox_days", n] => cfg.inbox_days = n.parse().unwrap_or(cfg.inbox_days),
                    ["session", tag, ws] => {
                        cfg.session_prefs.insert(tag.to_string(),
                            SessionPref { ws: parse_ws(ws), bg: None });
                    }
                    ["session", tag, ws, bg] => {
                        cfg.session_prefs.insert(tag.to_string(), SessionPref {
                            ws: parse_ws(ws),
                            bg: Some(bg.trim_start_matches('#').to_string()),
                        });
                    }
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

/// Update-or-insert the `session <tag> ...` line in ~/.fleetrc, keeping
/// every other line. A pref with neither ws nor bg removes the line.
/// Atomic (temp + rename) so a reload never sees a half-written file.
pub fn write_session_pref(tag: &str, pref: &SessionPref) {
    let path = home().join(".fleetrc");
    let mut lines: Vec<String> = std::fs::read_to_string(&path)
        .map(|t| t.lines().map(String::from).collect())
        .unwrap_or_default();
    lines.retain(|l| {
        let f: Vec<&str> = l.split('#').next().unwrap_or("").split_whitespace().collect();
        !(f.len() >= 2 && f[0] == "session" && f[1] == tag)
    });
    if pref.ws.is_some() || pref.bg.is_some() {
        let ws = pref.ws.map(|w| (w + 1).to_string()).unwrap_or_else(|| "-".into());
        let mut line = format!("session {} {}", tag, ws);
        if let Some(bg) = &pref.bg {
            line.push(' ');
            line.push_str(bg.trim_start_matches('#'));
        }
        lines.push(line);
    }
    let tmp = path.with_extension("fleetrc.tmp");
    let body = lines.join("\n") + "\n";
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
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

//! ~/.fleetrc — plain text, whitespace-separated, '#' comments.
//!
//!   inbox <label> <dir> <glob>     a folder to watch for arriving items
//!   recent_days N                  sessions younger than this are listed
//!   idle_mins N                    older than this and a session is "idle"
//!   inbox_days N                   inbox items younger than this are shown
//!   ctx_window_k N                 context window in k tokens (default 1000);
//!                                  CTX turns yellow at 50 % and red at 75 %
//!                                  of it, like the CC statusline
//!   parked <tag>                   a session to leave alone: listed, but
//!                                  not counted and sorted to the bottom
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

use std::collections::{HashMap, HashSet};
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
    pub title: Option<String>, // window title, kept from Claude Code's own
}

pub struct Config {
    pub watches: Vec<Watch>,
    pub recent_days: u64,
    pub idle_mins: u64,
    pub inbox_days: u64,
    pub ctx_window_k: u64,
    pub session_prefs: HashMap<String, SessionPref>,
    /// Tags the user has told fleet to stop nagging about. Cleared the
    /// moment such a session starts working again.
    pub parked: HashSet<String>,
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
            parked: HashSet::new(),
        };
        let mut have_inbox = false;
        if let Ok(text) = std::fs::read_to_string(home().join(".fleetrc")) {
            for line in text.lines() {
                // `title <tag> <text>`: read before the comment cut, since
                // a title may well hold a '#' ("#rust").
                if let Some(rest) = line.strip_prefix("title ") {
                    if let Some((tag, t)) = rest.trim().split_once(char::is_whitespace) {
                        cfg.session_prefs.entry(tag.to_string()).or_default().title = Some(t.trim().to_string());
                    }
                    continue;
                }
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
                    ["parked", tag] => {
                        cfg.parked.insert(tag.to_string());
                    }
                    ["session", tag, ws] => {
                        cfg.session_prefs.entry(tag.to_string()).or_default().ws = parse_ws(ws);
                    }
                    ["session", tag, ws, bg] => {
                        let p = cfg.session_prefs.entry(tag.to_string()).or_default();
                        p.ws = parse_ws(ws);
                        p.bg = Some(bg.trim_start_matches('#').to_string());
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

/// Update-or-insert the `session <tag> ...` and `title <tag> ...` lines
/// in ~/.fleetrc, keeping every other line. A part left empty removes
/// its line.
/// Atomic (temp + rename) so a reload never sees a half-written file.
pub fn write_session_pref(tag: &str, pref: &SessionPref) {
    let path = home().join(".fleetrc");
    let mut lines: Vec<String> = std::fs::read_to_string(&path)
        .map(|t| t.lines().map(String::from).collect())
        .unwrap_or_default();
    lines.retain(|l| {
        let f: Vec<&str> = l.split('#').next().unwrap_or("").split_whitespace().collect();
        let titled = l.strip_prefix("title ").is_some_and(|r| r.split_whitespace().next() == Some(tag));
        !(f.len() >= 2 && f[0] == "session" && f[1] == tag) && !titled
    });
    if let Some(t) = pref.title.as_ref().filter(|t| !t.is_empty()) {
        lines.push(format!("title {} {}", tag, t));
    }
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

/// Rewrite every `parked` line in ~/.fleetrc, keeping the rest. Called
/// when the user parks a session and when a parked one starts working,
/// so it is a handful of writes a day rather than one per tick.
pub fn write_parked(parked: &HashSet<String>) {
    let path = home().join(".fleetrc");
    let mut lines: Vec<String> = std::fs::read_to_string(&path)
        .map(|t| t.lines().map(String::from).collect())
        .unwrap_or_default();
    lines.retain(|l| {
        let f: Vec<&str> = l.split('#').next().unwrap_or("").split_whitespace().collect();
        !(f.len() >= 2 && f[0] == "parked")
    });
    let mut tags: Vec<&String> = parked.iter().collect();
    tags.sort();
    for t in tags {
        lines.push(format!("parked {}", t));
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

#[cfg(test)]
mod title_tests {
    use super::*;

    #[test]
    fn a_title_with_a_hash_survives_a_write_and_a_load() {
        let dir = std::env::temp_dir().join(format!("fleet-title-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HOME", &dir);
        std::fs::write(dir.join(".fleetrc"), "session rust 3 280800\nparked x\n").unwrap();
        let mut p = Config::load().session_prefs.remove("rust").unwrap();
        p.title = Some("#rust work".into());
        write_session_pref("rust", &p);
        let back = Config::load().session_prefs.remove("rust").unwrap();
        assert_eq!(back.title.as_deref(), Some("#rust work"));
        assert_eq!((back.ws, back.bg.as_deref()), (Some(2), Some("280800")));
        p.title = None;
        write_session_pref("rust", &p);
        let text = std::fs::read_to_string(dir.join(".fleetrc")).unwrap();
        assert!(!text.contains("title"), "{text}");
        assert!(text.contains("parked x"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! fleet — Claude Code mission control TUI.
//!
//! One screen for every session on the machine: who is working, who waits
//! for you, on which workspace, plus the inbox folders where handoffs land
//! (laptop scrots in ~, phone items in ~/.transfer; configurable).
//!
//! Battery posture: a 2 s tick while open. Each tick is one stat per
//! session file (tails are re-read only on mtime change), one readdir per
//! inbox folder, and one /proc sweep for claude pids. No daemons, nothing
//! runs after q.

mod config;
mod inbox;
mod sessions;
mod winmap;

use config::Config;
use crust::style;
use crust::{Crust, Input, Pane};
use sessions::{Cache, Session, State};
use std::process::{Command, Stdio};
use winmap::WinMap;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(PartialEq, Clone, Copy)]
enum Focus {
    Sessions,
    Inbox,
}

fn fmt_age(s: u64) -> String {
    if s >= 86400 {
        format!("{}d{}h", s / 86400, (s % 86400) / 3600)
    } else if s >= 3600 {
        format!("{}h{}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m", s / 60)
    } else {
        format!("{}s", s)
    }
}

fn state_color(s: State) -> u8 {
    match s {
        State::Working => 46,
        State::Yours => 208,
        State::Idle => 244,
        State::Off => 238,
    }
}

fn clip(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        format!("{:<width$}", s, width = max)
    } else {
        let mut t: String = s.chars().take(max - 1).collect();
        t.push('…');
        t
    }
}

fn main() {
    if std::env::args().skip(1).any(|a| a == "-h" || a == "--help") {
        println!("fleet — Claude Code mission control (Fe2O3 suite)");
        println!();
        println!("Usage: fleet [--list]");
        println!();
        println!("  --list    print sessions and inbox as text and exit");
        println!();
        println!("Sessions with state (working / YOURS / idle / off), workspace and");
        println!("context size, plus the inbox folders where handoffs land.");
        println!("Config: ~/.fleetrc (see README).");
        return;
    }
    if std::env::args().skip(1).any(|a| a == "-v" || a == "--version") {
        println!("fleet {}", VERSION);
        return;
    }

    let cfg = Config::load();
    let mut cache = Cache::new();

    if std::env::args().skip(1).any(|a| a == "--list") {
        let wm = WinMap::connect();
        let map = wm.as_ref().map(|w| w.refresh()).unwrap_or_default();
        for mut s in sessions::scan(&cfg, &mut cache) {
            s.ws = s.pid.and_then(|p| sessions::window_ancestor(p, &map));
            println!(
                "{:<10} {:<8} {:>6} ws={} ctx={} {:<8} {}",
                s.tag,
                s.state.label(),
                fmt_age(s.age_secs),
                s.ws.map(|w| (w + 1).to_string()).unwrap_or_else(|| "-".into()),
                s.ctx_k.map(|k| format!("{}k", k)).unwrap_or_else(|| "-".into()),
                s.model,
                s.prompt
            );
        }
        for i in inbox::scan(&cfg) {
            println!("inbox {:<8} {:>6} {}", i.label, fmt_age(i.age_secs), i.name);
        }
        return;
    }

    Crust::init();
    let (mut cols, mut rows) = Crust::terminal_size();
    let wm = WinMap::connect();
    let mut focus = Focus::Sessions;
    let mut sel_s = 0usize;
    let mut sel_i = 0usize;
    let mut pending_del: Option<std::path::PathBuf> = None;
    let mut flash = String::new();

    loop {
        let map = wm.as_ref().map(|w| w.refresh()).unwrap_or_default();
        let mut sess = sessions::scan(&cfg, &mut cache);
        for s in &mut sess {
            s.ws = s.pid.and_then(|p| sessions::window_ancestor(p, &map));
        }
        let items = inbox::scan(&cfg);
        sel_s = sel_s.min(sess.len().saturating_sub(1));
        sel_i = sel_i.min(items.len().saturating_sub(1));

        let body = rows.saturating_sub(2) as usize;
        let inbox_h = (items.len() + 2).clamp(3, (body / 3).max(3));
        let sess_h = body.saturating_sub(inbox_h);

        draw_header(cols, &sess, &items);
        draw_sessions(cols, 2, sess_h as u16, &sess, focus == Focus::Sessions, sel_s);
        draw_inbox(cols, 2 + sess_h as u16, inbox_h as u16, &items,
                   focus == Focus::Inbox, sel_i);
        draw_footer(cols, rows, focus, &pending_del, &flash);

        let key = Input::getchr(Some(2));
        let k = key.as_deref();
        flash.clear();

        if let Some(path) = pending_del.take() {
            if k == Some("y") || k == Some("Y") {
                match std::fs::remove_file(&path) {
                    Ok(()) => flash = format!("deleted {}", path.display()),
                    Err(e) => flash = format!("delete failed: {}", e),
                }
            } else {
                flash = "delete cancelled".into();
            }
            continue;
        }

        match k {
            Some("q") | Some("Q") => break,
            Some("TAB") => {
                focus = if focus == Focus::Sessions { Focus::Inbox } else { Focus::Sessions };
            }
            Some("UP") => match focus {
                Focus::Sessions => sel_s = sel_s.saturating_sub(1),
                Focus::Inbox => sel_i = sel_i.saturating_sub(1),
            },
            Some("DOWN") => match focus {
                Focus::Sessions => {
                    if sel_s + 1 < sess.len() {
                        sel_s += 1;
                    }
                }
                Focus::Inbox => {
                    if sel_i + 1 < items.len() {
                        sel_i += 1;
                    }
                }
            },
            Some("ENTER") | Some("o") => match focus {
                Focus::Sessions => {
                    if let Some(s) = sess.get(sel_s) {
                        flash = match s.ws {
                            Some(w) => format!("{} is on workspace {} (Mod4+{})",
                                               s.tag, w + 1, w + 1),
                            None => format!("{} has no window here", s.tag),
                        };
                    }
                }
                Focus::Inbox => {
                    if let Some(i) = items.get(sel_i) {
                        let _ = Command::new("xdg-open")
                            .arg(&i.path)
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .spawn();
                        flash = format!("opened {}", i.name);
                    }
                }
            },
            Some("D") if focus == Focus::Inbox => {
                if let Some(i) = items.get(sel_i) {
                    pending_del = Some(i.path.clone());
                }
            }
            Some("RESIZE") => {
                let (c, r) = Crust::terminal_size();
                cols = c;
                rows = r;
            }
            _ => {}
        }
    }
    Crust::cleanup();
}

fn draw_header(cols: u16, sess: &[Session], items: &[inbox::Item]) {
    let mut pane = Pane::new(1, 1, cols, 1, 255, 236);
    let yours = sess.iter().filter(|s| s.state == State::Yours).count();
    let working = sess.iter().filter(|s| s.state == State::Working).count();
    let mut line = format!(" {}  ", style::bold("fleet"));
    line.push_str(&style::styled(&format!("{} YOURS", yours), Some(208), None, "b"));
    line.push_str(&format!("  ·  {} working  ·  {} sessions", working, sess.len()));
    line.push_str(&format!("  ·  inbox {}", items.len()));
    pad(&mut line, cols as usize);
    pane.set_text(&line);
    pane.refresh();
}

fn draw_sessions(cols: u16, y: u16, h: u16, sess: &[Session], focused: bool, sel: usize) {
    let mut pane = Pane::new(1, y, cols, h, 231, 0);
    let hdr = format!(
        " {:<10}  {:<7}  {:>6}  {:>2}  {:>5}  {:<8}  {}",
        "SESSION", "STATE", "AGE", "WS", "CTX", "MODEL", "LAST PROMPT"
    );
    let mut out = format!("{}\n", style::styled(&hdr, Some(250), None, "b"));
    let take = (h as usize).saturating_sub(1).min(sess.len());
    for (i, s) in sess.iter().take(take).enumerate() {
        let width = (cols as usize).saturating_sub(52);
        let line = format!(
            " {:<10}  {}  {:>6}  {:>2}  {:>5}  {:<8}  {}",
            clip(&s.tag, 10),
            style::styled(&format!("{:<7}", s.state.label()),
                          Some(state_color(s.state)), None,
                          if s.state == State::Yours { "b" } else { "" }),
            fmt_age(s.age_secs),
            s.ws.map(|w| (w + 1).to_string()).unwrap_or_else(|| "·".into()),
            s.ctx_k.map(|k| format!("{}k", k)).unwrap_or_else(|| "·".into()),
            clip(&s.model, 8),
            clip(&s.prompt, width.max(10))
        );
        let line = if focused && i == sel {
            style::styled(&line, None, Some(238), "")
        } else {
            line
        };
        out.push_str(&line);
        out.push('\n');
    }
    pane.set_text(out.trim_end_matches('\n'));
    pane.refresh();
}

fn draw_inbox(cols: u16, y: u16, h: u16, items: &[inbox::Item], focused: bool, sel: usize) {
    let mut pane = Pane::new(1, y, cols, h, 231, 0);
    let hdr = format!(" {:<8}  {:>6}  {}", "INBOX", "AGE", "FILE");
    let mut out = format!("{}\n", style::styled(&hdr, Some(250), None, "b"));
    if items.is_empty() {
        out.push_str(&style::dim("  nothing waiting"));
    }
    let take = (h as usize).saturating_sub(1).min(items.len());
    for (i, it) in items.iter().take(take).enumerate() {
        let line = format!(
            " {}  {:>6}  {}",
            style::styled(&clip(&it.label, 8), Some(51), None, ""),
            fmt_age(it.age_secs),
            clip(&it.name, (cols as usize).saturating_sub(22).max(10))
        );
        let line = if focused && i == sel {
            style::styled(&line, None, Some(238), "")
        } else {
            line
        };
        out.push_str(&line);
        out.push('\n');
    }
    pane.set_text(out.trim_end_matches('\n'));
    pane.refresh();
}

fn draw_footer(cols: u16, rows: u16, focus: Focus, pending: &Option<std::path::PathBuf>,
               flash: &str) {
    let mut pane = Pane::new(1, rows, cols, 1, 244, 236);
    let left = if let Some(p) = pending {
        style::styled(
            &format!(" delete {}?  y / n", p.file_name().unwrap_or_default().to_string_lossy()),
            Some(208), None, "b")
    } else if !flash.is_empty() {
        style::fg(flash, 46)
    } else {
        match focus {
            Focus::Sessions => " q quit · TAB inbox · ↑↓ · Enter where".to_string(),
            Focus::Inbox => " q quit · TAB sessions · ↑↓ · o open · D delete".to_string(),
        }
    };
    let right = format!("fleet v{} ", VERSION);
    let mut line = left;
    let pad_n = (cols as usize).saturating_sub(visible_len(&line) + right.len());
    line.push_str(&" ".repeat(pad_n));
    line.push_str(&right);
    pane.set_text(&line);
    pane.refresh();
}

fn visible_len(s: &str) -> usize {
    let mut n = 0;
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == 0x1b && i + 1 < b.len() && b[i + 1] == b'[' {
            i += 2;
            while i < b.len() && b[i] != b'm' {
                i += 1;
            }
            i += 1;
        } else {
            n += 1;
            i += 1;
        }
    }
    n
}

fn pad(s: &mut String, target: usize) {
    let n = visible_len(s);
    if n < target {
        s.push_str(&" ".repeat(target - n));
    }
}

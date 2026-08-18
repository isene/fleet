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
mod rollup;
mod sessions;
mod winmap;

use config::Config;
use crust::style;
use crust::{Crust, Input, Pane, Popup};
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

/// Truncate without padding: for a row's last column, so the line never
/// reaches pane width (a full-width row wraps and double-spaces the list).
fn clip_end(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max - 1).collect();
        t.push('…');
        t
    }
}

/// Selection bar: a background that survives the full resets styled()
/// spans end in, re-armed after every reset the way crust's select_bar
/// re-arms reverse. Foreground colors stay.
fn bg_keep(s: &str, bg: u8) -> String {
    let arm = format!("\x1b[48;5;{}m", bg);
    format!("{}{}\x1b[49m",
            arm, s.replace("\x1b[0m", &format!("\x1b[0m{}", arm)))
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
        println!("Usage: fleet [--list | --today]");
        println!();
        println!("  --list     print sessions and inbox as text and exit");
        println!("  --today    print today's token rollup per session and exit");
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

    if std::env::args().skip(1).any(|a| a == "--today") {
        let rows = rollup::today(&sessions::load_tags());
        let (mut out, mut inp) = (0u64, 0u64);
        for r in &rows {
            println!("{:<12} {:>7}k out  {:>6}k in  {:>4} turns",
                     r.tag, r.out_tokens / 1000, r.in_tokens / 1000, r.turns);
            out += r.out_tokens;
            inp += r.in_tokens;
        }
        println!("{:<12} {:>7}k out  {:>6}k in", "TOTAL", out / 1000, inp / 1000);
        return;
    }

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
    let mut marked: Vec<std::path::PathBuf> = Vec::new();
    let mut flash = String::new();
    let mut rollup_rows: Option<Vec<rollup::Row>> = None;
    let mut msg_to: Option<(String, String)> = None; // (bus address, shown tag)
    let mut msg_buf = String::new();

    loop {
        let map = wm.as_ref().map(|w| w.refresh()).unwrap_or_default();
        let mut sess = sessions::scan(&cfg, &mut cache);
        for s in &mut sess {
            s.ws = s.pid.and_then(|p| sessions::window_ancestor(p, &map));
        }
        let items = inbox::scan(&cfg);
        marked.retain(|p| items.iter().any(|i| &i.path == p));
        sel_s = sel_s.min(sess.len().saturating_sub(1));
        sel_i = sel_i.min(items.len().saturating_sub(1));

        let body = rows.saturating_sub(2) as usize;
        let inbox_h = (items.len() + 2).clamp(3, (body / 3).max(3));
        let sess_h = body.saturating_sub(inbox_h);

        draw_header(cols, &sess, &items);
        if let Some(rows_r) = &rollup_rows {
            draw_rollup(cols, 2, body as u16, rows_r);
        } else {
            draw_sessions(cols, 2, sess_h as u16, &sess, focus == Focus::Sessions, sel_s);
            draw_inbox(cols, 2 + sess_h as u16, inbox_h as u16, &items,
                       focus == Focus::Inbox, sel_i, &marked);
        }
        draw_footer(cols, rows, focus, &flash,
                    rollup_rows.is_some(), &msg_to, &msg_buf);

        let key = Input::getchr(Some(2));
        let k = key.as_deref();
        flash.clear();

        // Message-input mode captures every keystroke until Enter or Esc.
        if let Some((addr, tag)) = msg_to.clone() {
            match k {
                Some("ENTER") => {
                    if !msg_buf.trim().is_empty() {
                        flash = match send_msg(&addr, msg_buf.trim()) {
                            Ok(()) => format!("sent to {}", tag),
                            Err(e) => format!("send failed: {}", e),
                        };
                    }
                    msg_to = None;
                    msg_buf.clear();
                }
                Some("ESC") => {
                    msg_to = None;
                    msg_buf.clear();
                }
                Some("BACKSPACE") => {
                    msg_buf.pop();
                }
                Some(s) if s.chars().count() == 1 => msg_buf.push_str(s),
                _ => {}
            }
            continue;
        }

        if rollup_rows.is_some() {
            match k {
                Some("ESC") | Some("c") | Some("q") | Some("Q") => rollup_rows = None,
                _ => {}
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
                            Some(w) => jump(&s.tag, w),
                            None => resurrect(s),
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
            Some("d") if focus == Focus::Inbox => {
                if let Some(i) = items.get(sel_i) {
                    if let Some(pos) = marked.iter().position(|p| p == &i.path) {
                        marked.remove(pos);
                    } else {
                        marked.push(i.path.clone());
                    }
                    if sel_i + 1 < items.len() {
                        sel_i += 1; // flag-and-advance, pointer style
                    }
                }
            }
            Some("<") => {
                if marked.is_empty() {
                    flash = "nothing flagged for deletion (press d to flag)".into();
                } else {
                    let mut n = 0;
                    for p in &marked {
                        if std::fs::remove_file(p).is_ok() {
                            n += 1;
                        }
                    }
                    marked.clear();
                    flash = format!("deleted {} file(s)", n);
                }
            }
            Some("m") if focus == Focus::Sessions => {
                if let Some(s) = sess.get(sel_s) {
                    // A bookmark tag is the stable address (it follows the
                    // session across id changes); a raw id is the fallback.
                    let addr = if s.tagged { s.tag.clone() } else { s.id.clone() };
                    msg_to = Some((addr, s.tag.clone()));
                    msg_buf.clear();
                }
            }
            Some("c") => {
                rollup_rows = Some(rollup::today(&sessions::load_tags()));
            }
            Some("?") | Some("h") => help(),
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
    // Darker than the column-header bars (236), so the two read apart.
    let mut pane = Pane::new(1, 1, cols, 1, 255, 234);
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

/// Full-width bold header bar on a dark background, for readability.
fn header_bar(text: &str, cols: u16) -> String {
    let mut s = text.to_string();
    pad(&mut s, cols as usize);
    format!("{}\n", style::styled(&s, Some(250), Some(236), "b"))
}

/// Context size coloring: the statusline's green / yellow / red family.
fn ctx_color(k: u64) -> u8 {
    if k < 150 { 78 } else if k < 400 { 220 } else { 196 }
}

fn draw_sessions(cols: u16, y: u16, h: u16, sess: &[Session], focused: bool, sel: usize) {
    let mut pane = Pane::new(1, y, cols, h, 231, 0);
    let hdr = format!(
        " {:<10}  {:<7}  {:>6}  {:>2}  {:>5}  {:<8}  {}",
        "SESSION", "STATE", "AGE", "WS", "CTX", "MODEL", "LAST PROMPT"
    );
    let mut out = header_bar(&hdr, cols);
    let take = (h as usize).saturating_sub(1).min(sess.len());
    for (i, s) in sess.iter().take(take).enumerate() {
        let width = (cols as usize).saturating_sub(52);
        // Colors follow the CC statusline: bookmark tags magenta 13,
        // model bold blue, context green/yellow/red, timestamps gray 242.
        let ctx = s.ctx_k.map(|k| format!("{}k", k)).unwrap_or_else(|| "·".into());
        let line = format!(
            " {}  {}  {}  {:>2}  {}  {}  {}",
            style::fg(&clip(&s.tag, 10), 13),
            style::styled(&format!("{:<7}", s.state.label()),
                          Some(state_color(s.state)), None,
                          if s.state == State::Yours { "b" } else { "" }),
            style::fg(&format!("{:>6}", fmt_age(s.age_secs)), 242),
            s.ws.map(|w| (w + 1).to_string()).unwrap_or_else(|| "·".into()),
            style::fg(&format!("{:>5}", ctx), s.ctx_k.map(ctx_color).unwrap_or(242)),
            style::styled(&clip(&s.model, 8), Some(33), None, "b"),
            clip_end(&s.prompt, width.max(10))
        );
        let line = if focused && i == sel {
            bg_keep(line.trim_end(), 238)
        } else {
            line
        };
        out.push_str(&line);
        out.push('\n');
    }
    pane.set_text(out.trim_end_matches('\n'));
    pane.refresh();
}

fn draw_inbox(cols: u16, y: u16, h: u16, items: &[inbox::Item],
              focused: bool, sel: usize, marked: &[std::path::PathBuf]) {
    let mut pane = Pane::new(1, y, cols, h, 231, 0);
    let hdr = format!(" {:<8}  {:>6}  {}", "INBOX", "AGE", "FILE");
    let mut out = header_bar(&hdr, cols);
    if items.is_empty() {
        out.push_str(&style::dim("  nothing waiting"));
    }
    let take = (h as usize).saturating_sub(1).min(items.len());
    for (i, it) in items.iter().take(take).enumerate() {
        let line = format!(
            " {}  {}  {}",
            style::fg(&clip(&it.label, 8), 13),
            style::fg(&format!("{:>6}", fmt_age(it.age_secs)), 242),
            clip_end(&it.name, (cols as usize).saturating_sub(22).max(10))
        );
        // Delete-flagged (pointer style): the whole row dark red;
        // selection still shows via the bar.
        let line = if marked.contains(&it.path) {
            let body = style::fg(crust::strip_ansi(&line).trim_end(), 88);
            if focused && i == sel {
                bg_keep(&body, 238)
            } else {
                body
            }
        } else if focused && i == sel {
            bg_keep(line.trim_end(), 238)
        } else {
            line
        };
        out.push_str(&line);
        out.push('\n');
    }
    pane.set_text(out.trim_end_matches('\n'));
    pane.refresh();
}

/// Switch to the session's workspace by injecting tile's own hotkey
/// (frame supports XTEST). Falls back to naming the workspace.
fn jump(tag: &str, ws: u32) -> String {
    let key = format!("super+{}", ws + 1);
    match Command::new("xdotool")
        .args(["key", &key])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(st) if st.success() => format!("→ {} on ws {}", tag, ws + 1),
        _ => format!("{} is on workspace {} (Mod4+{})", tag, ws + 1, ws + 1),
    }
}

/// A session with no window on this display: resume it in a fresh glass.
/// Tagged sessions go through `cc <tag>` (path + auto-follow); untagged
/// ones get a plain resume in their own working directory.
fn resurrect(s: &Session) -> String {
    let mut c = Command::new("glass");
    if s.tagged {
        c.args(["-e", "cc", &s.tag]);
    } else {
        c.args(["-e", "claude", "--resume", &s.id]);
        if !s.cwd.is_empty() {
            c.current_dir(&s.cwd);
        }
    }
    match c.stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        Ok(_) => format!("resuming {} in a new glass", s.tag),
        Err(e) => format!("glass failed: {}", e),
    }
}

fn send_msg(addr: &str, text: &str) -> std::io::Result<()> {
    let dir = config::home().join(".fleet/bus").join(addr);
    std::fs::create_dir_all(&dir)?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    std::fs::write(dir.join(format!("{}-fleet.msg", ts)), format!("{}\n", text))
}

fn draw_rollup(cols: u16, y: u16, h: u16, rows: &[rollup::Row]) {
    let mut pane = Pane::new(1, y, cols, h, 231, 0);
    let hdr = format!(" {:<12}  {:>9}  {:>9}  {:>6}", "TODAY", "OUT", "IN", "TURNS");
    let mut out = format!("{}\n", style::styled(&hdr, Some(250), None, "b"));
    let (mut o, mut i) = (0u64, 0u64);
    for r in rows {
        o += r.out_tokens;
        i += r.in_tokens;
    }
    for r in rows.iter().take((h as usize).saturating_sub(3)) {
        out.push_str(&format!(
            " {:<12}  {:>8}k  {:>8}k  {:>6}\n",
            clip(&r.tag, 12), r.out_tokens / 1000, r.in_tokens / 1000, r.turns
        ));
    }
    out.push_str(&style::styled(
        &format!(" {:<12}  {:>8}k  {:>8}k", "TOTAL", o / 1000, i / 1000),
        Some(250), None, "b"));
    pane.set_text(&out);
    pane.refresh();
}

/// Bordered, blocking help viewer (crust Popup): ESC / q / ENTER closes.
fn help() {
    let hdr = |s: &str| style::styled(s, Some(208), None, "b");
    let key = |s: &str| style::styled(&format!("  {:<10}", s), Some(46), None, "");
    let mut t = String::new();
    t.push_str(&format!(" {}\n", hdr("SESSIONS")));
    t.push_str(&format!("{}jump to it, or resume it in a new glass\n", key("Enter")));
    t.push_str(&format!("{}send a message on the bus\n", key("m")));
    t.push_str(&format!(" {}\n", hdr("INBOX")));
    t.push_str(&format!("{}open the item\n", key("o / Enter")));
    t.push_str(&format!("{}flag for deletion (D marks the row)\n", key("d")));
    t.push_str(&format!("{}delete the flagged files\n", key("<")));
    t.push_str(&format!(" {}\n", hdr("GLOBAL")));
    t.push_str(&format!("{}switch sessions / inbox\n", key("TAB")));
    t.push_str(&format!("{}select\n", key("↑ ↓")));
    t.push_str(&format!("{}today's token rollup (Esc back)\n", key("c")));
    t.push_str(&format!("{}this help (Esc / q / Enter closes)\n", key("?")));
    t.push_str(&format!("{}quit", key("q")));
    Popup::centered(50, 15, 231, 236).view(&t);
}

fn draw_footer(cols: u16, rows: u16, focus: Focus,
               flash: &str, in_rollup: bool, msg_to: &Option<(String, String)>,
               msg_buf: &str) {
    let mut pane = Pane::new(1, rows, cols, 1, 244, 236);
    let left = if let Some((_, tag)) = msg_to {
        format!(" msg → {}: {}_  (Enter send · Esc cancel)", tag, msg_buf)
    } else if !flash.is_empty() {
        style::fg(flash, 46)
    } else if in_rollup {
        " Esc back".to_string()
    } else {
        match focus {
            Focus::Sessions => " q quit · TAB inbox · ↑↓ · Enter jump/resume · m message · c today · ? help".to_string(),
            Focus::Inbox => " q quit · TAB sessions · ↑↓ · o open · d flag · < purge · ? help".to_string(),
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
            if b[i] & 0xC0 != 0x80 {
                n += 1; // count code points, not bytes (arrows, box chars)
            }
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

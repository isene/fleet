//! fleet — Claude Code mission control TUI.
//!
//! One screen for every session on the machine: who is working, who waits
//! for you, on which workspace, plus the inbox folders where handoffs land
//! (laptop scrots in ~, phone items in ~/.transfer; configurable).
//!
//! Battery posture: a 2 s tick while open. Each tick is one stat per
//! session file (tails are re-read only on mtime change) and one readdir
//! per inbox folder. The X property sweep is queued and read in one
//! round trip rather than two per window, and the /proc sweep runs every
//! 10 s rather than every tick. Together that is 21 wakeups a second
//! instead of 170. No daemons, nothing runs after q.

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
/// What a woken session is told to do. The bus hook hands a session its
/// mail on the next user prompt, so a message only ever gets read when
/// someone types into that session.
const WAKE_PROMPT: &str = "Check fleet messages";
/// Width of the SESSION column. Ten cut names like "corporate-int…"
/// and left the tag unreadable, which is the one thing a row is for.
const TAG_W: usize = 18;

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
        State::Capped => 196,
        State::Working => 46,
        State::Yours => 208,
        State::Idle => 69,    // alive but quiet: blue, never one of the greys
        State::Off => 60,
        State::Older => 240, // darker than off, but never the bar's 238
        State::Parked => 245, // grey, and lighter than older: set aside, not gone
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
        println!("Sessions with state (working / YOURS / CAPPED / idle / off), workspace and");
        println!("context size, plus the inbox folders where handoffs land.");
        println!("Config: ~/.fleetrc (see README).");
        return;
    }
    if std::env::args().skip(1).any(|a| a == "-v" || a == "--version") {
        println!("fleet {}", VERSION);
        return;
    }

    let mut cfg = Config::load();
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
        let scanned = sessions::scan(&cfg, &mut cache);
        let addrs: Vec<String> = scanned.iter()
            .filter(|s| s.tagged)
            .map(|s| s.tag.clone())
            .collect();
        for mut s in scanned {
            s.ws = s.pid.and_then(|p| sessions::window_ancestor(p, &map));
            println!(
                "{:<w$} {:<8} {:>6} ws={} ctx={} {:<8} {}",
                s.tag,
                s.state.label(),
                fmt_age(s.age_secs),
                s.ws.map(|w| (w + 1).to_string()).unwrap_or_else(|| "-".into()),
                s.ctx_k.map(|k| format!("{}k", k)).unwrap_or_else(|| "-".into()),
                s.model,
                s.prompt,
                w = TAG_W
            );
        }
        for i in inbox::scan(&cfg) {
            println!("inbox {:<8} {:>6} {}", i.label, fmt_age(i.age_secs), i.name);
        }
        // Messages still waiting on the bus, the same rows the TUI's
        // INBOX pane shows. Without these, --list looks empty while a
        // message from the phone sits undelivered.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        for l in inbox::pending() {
            let from = if l.from.is_empty() { "?".to_string() } else { l.from.clone() };
            let mark = if from == "?" || addrs.contains(&from) { "" } else { "?" };
            println!("msg   #{}{} → #{}  {:>6}  {}", from, mark, l.dest,
                     fmt_age(now.saturating_sub(l.ts)), l.text);
        }
        return;
    }

    Crust::init();
    let (mut cols, mut rows) = Crust::terminal_size();
    let wm = WinMap::connect();
    let mut focus = Focus::Sessions;
    let mut sel_s = 0usize;
    let mut sel_i = 0usize;
    // The selected session's transcript path. The list re-sorts on every
    // pass (state rank, then age), so a bare index drifts onto a different
    // session between redraws and Enter then jumps to the wrong one.
    let mut sel_path: Option<std::path::PathBuf> = None;
    let mut marked: Vec<std::path::PathBuf> = Vec::new();
    let mut marked_s: Vec<std::path::PathBuf> = Vec::new();
    let mut flash = String::new();
    let mut rates: (std::time::SystemTime, String) =
        (std::time::UNIX_EPOCH, String::new());
    // Files seen in relay/phone: new arrivals are laptop→phone sends,
    // logged here since no hook observes that direction. The baseline
    // pass logs nothing, or every restart would re-log leftovers.
    let mut phone_seen: Option<std::collections::HashSet<String>> = None;
    let mut rollup_rows: Option<Vec<rollup::Row>> = None;
    let mut msg_to: Option<(String, String)> = None; // (bus address, shown tag)
    let mut msg_buf = String::new();
    // A session to prod, set by Enter on an inbox message:
    // (tag, not before, give up). One that had to be resumed needs a few
    // seconds of Claude Code start-up before it can take a prompt.
    let mut wake: Option<(String, std::time::Instant, std::time::Instant)> = None;

    loop {
        let map = wm.as_ref().map(|w| w.refresh()).unwrap_or_default();
        let mut sess = sessions::scan(&cfg, &mut cache);
        for s in &mut sess {
            s.ws = s.pid.and_then(|p| sessions::window_ancestor(p, &map));
        }
        // Parking is for quiet sessions. One that has started working
        // again says so, and the flag goes with it.
        if !cfg.parked.is_empty() {
            let busy: Vec<String> = sess.iter()
                .filter(|s| s.state == State::Working && cfg.parked.contains(&s.tag))
                .map(|s| s.tag.clone())
                .collect();
            if !busy.is_empty() {
                for t in busy {
                    cfg.parked.remove(&t);
                }
                config::write_parked(&cfg.parked);
            }
        }
        let items = inbox::scan(&cfg);
        let pdir = config::home().join(".fleet/relay/phone");
        let names: Vec<String> = std::fs::read_dir(&pdir)
            .map(|it| {
                it.flatten()
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        match &mut phone_seen {
            None => phone_seen = Some(names.into_iter().collect()),
            Some(seen) => {
                for n in names {
                    if seen.insert(n.clone()) {
                        if let Ok(t) = std::fs::read_to_string(pdir.join(&n)) {
                            log_append("phone", &t);
                        }
                    }
                }
            }
        }
        let logs = inbox::pending();
        // Keep a flag as long as its file is still an inbox item or a
        // pending message; a delivered message drops its flag with it.
        marked.retain(|p| {
            items.iter().any(|i| &i.path == p)
                || logs.iter().any(|e| e.path.as_deref() == Some(p.as_path()))
        });
        // A flagged session that comes alive again is unflagged.
        marked_s.retain(|p| sess.iter().any(|s| {
            &s.path == p
                && matches!(s.state, State::Idle | State::Off | State::Older)
        }));
        // Follow the selected session to its new row. Gone (or nothing
        // selected yet) falls back to the index, clamped below.
        if let Some(p) = &sel_path {
            if let Some(i) = sess.iter().position(|s| &s.path == p) {
                sel_s = i;
            }
        }
        sel_s = sel_s.min(sess.len().saturating_sub(1));
        sel_i = sel_i.min((items.len() + logs.len()).saturating_sub(1));
        if items.is_empty() && logs.is_empty() && focus == Focus::Inbox {
            focus = Focus::Sessions;   // the last item vanished under us
        }

        // Type the prompt into a woken session once it has a window and
        // has had time to start. The X client list is only walked while a
        // wake is pending, so an idle fleet keeps doing nothing.
        if let Some((tag, not_before, give_up)) = wake.clone() {
            let now_i = std::time::Instant::now();
            if now_i >= not_before {
                let xid = sess.iter().find(|s| s.tag == tag)
                    .and_then(|s| s.pid)
                    .and_then(|pid| wm.as_ref()
                        .and_then(|c| sessions::window_ancestor(pid, &c.pid_windows())));
                if let Some(x) = xid {
                    type_prompt(x, WAKE_PROMPT);
                    flash = format!("{} asked to check its messages", tag);
                    wake = None;
                } else if now_i >= give_up {
                    flash = format!("{} never came up", tag);
                    wake = None;
                }
            }
        }

        let body = rows.saturating_sub(2) as usize;
        let inbox_h = (items.len() + logs.len() + 2).clamp(3, (body / 3).max(3));
        let sess_h = body.saturating_sub(inbox_h);

        refresh_rates(&mut rates);
        draw_header(cols, &sess, &items, &rates.1);
        if let Some(rows_r) = &rollup_rows {
            draw_rollup(cols, 2, body as u16, rows_r);
        } else {
            draw_sessions(cols, 2, sess_h as u16, &sess,
                          focus == Focus::Sessions, sel_s, &marked_s,
                          cfg.ctx_window_k);
            // The addresses the bus can actually deliver to: the tags of
            // bookmarked sessions. A message signed with anything else
            // carries a return address that leads nowhere.
            let addrs: Vec<&str> = sess.iter()
                .filter(|s| s.tagged)
                .map(|s| s.tag.as_str())
                .collect();
            draw_inbox(cols, 2 + sess_h as u16, inbox_h as u16, &items,
                       focus == Focus::Inbox, sel_i, &marked, &logs, &addrs);
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
                // No landing an empty inbox: the selection bar needs a row.
                if focus == Focus::Sessions && !(items.is_empty() && logs.is_empty()) {
                    focus = Focus::Inbox;
                } else {
                    focus = Focus::Sessions;
                }
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
                    if sel_i + 1 < items.len() + logs.len() {
                        sel_i += 1;
                    }
                }
            },
            Some("ENTER") | Some("o") => match focus {
                Focus::Sessions => {
                    if let Some(s) = sess.get(sel_s) {
                        // Same workspace: injecting tile's hotkey would
                        // TOGGLE to the previous workspace, so don't.
                        let cur = wm.as_ref().and_then(|w| w.current_desktop());
                        // Raise this session's own tab first: it may be
                        // one of several full-screen glasses stacked on
                        // the workspace, so switching there is not enough.
                        if let (Some(wmc), Some(pid)) = (wm.as_ref(), s.pid) {
                            let pw = wmc.pid_windows();
                            if let Some(xid) = sessions::window_ancestor(pid, &pw) {
                                wmc.activate(xid);
                            }
                        }
                        flash = match s.ws {
                            Some(w) if Some(w) == cur => {
                                format!("→ {} raised", s.tag)
                            }
                            Some(w) => jump(&s.tag, w),
                            None => resurrect(s, cfg.session_prefs.get(&s.tag)),
                        };
                    }
                }
                Focus::Inbox => {
                    if let Some(i) = items.get(sel_i) {
                        let _ = Command::new("xdg-open")
                            .arg(&i.path)
                            .stdin(Stdio::null())   // same pty leak as resurrect
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .spawn();
                        flash = format!("opened {}", i.name);
                    } else if let Some(e) = logs.get(sel_i - items.len()) {
                        // A message row: hand it to the session it is for.
                        // A live one is prodded now; one that is off or old
                        // is resumed first, then prodded when its window is
                        // up (the wake block at the top of the loop).
                        let dest = e.dest.clone();
                        let now_i = std::time::Instant::now();
                        let secs = std::time::Duration::from_secs;
                        match sess.iter().find(|s| s.tag == dest) {
                            Some(s) if s.ws.is_some() => {
                                wake = Some((dest, now_i, now_i + secs(30)));
                            }
                            Some(s) => {
                                flash = resurrect(s, cfg.session_prefs.get(&s.tag));
                                wake = Some((dest, now_i + secs(8), now_i + secs(90)));
                            }
                            None => flash = format!("no session tagged {}", dest),
                        }
                    }
                }
            },
            Some("k") | Some("K") if focus == Focus::Sessions => {
                if let Some(s) = sess.get(sel_s) {
                    flash = match s.pid {
                        Some(pid) => {
                            let hard = k == Some("K");
                            let sig = if hard { libc::SIGKILL } else { libc::SIGTERM };
                            if unsafe { libc::kill(pid as i32, sig) } == 0 {
                                format!("stopped {} (SIG{})", s.tag,
                                        if hard { "KILL" } else { "TERM" })
                            } else {
                                format!("kill {} failed", s.tag)
                            }
                        }
                        None => format!("{} is already off", s.tag),
                    };
                }
            }
            Some("d") if focus == Focus::Sessions => {
                if let Some(s) = sess.get(sel_s) {
                    if matches!(s.state, State::Idle | State::Off | State::Older) {
                        if let Some(pos) = marked_s.iter().position(|p| p == &s.path) {
                            marked_s.remove(pos);
                        } else {
                            marked_s.push(s.path.clone());
                        }
                        if sel_s + 1 < sess.len() {
                            sel_s += 1;
                        }
                    } else {
                        flash = "only idle or off sessions can be flagged".into();
                    }
                }
            }
            Some("d") if focus == Focus::Inbox => {
                // Flag whichever row is under the cursor: an inbox file,
                // or a pending message below them. `<` then deletes it.
                let path = if sel_i < items.len() {
                    items.get(sel_i).map(|i| i.path.clone())
                } else {
                    logs.get(sel_i - items.len())
                        .and_then(|e| e.path.clone())
                };
                if let Some(p) = path {
                    if let Some(pos) = marked.iter().position(|m| m == &p) {
                        marked.remove(pos);
                    } else {
                        marked.push(p);
                    }
                    if sel_i + 1 < items.len() + logs.len() {
                        sel_i += 1; // flag-and-advance, pointer style
                    }
                }
            }
            Some("<") => {
                if marked.is_empty() && marked_s.is_empty() {
                    flash = "nothing flagged for deletion (press d to flag)".into();
                } else {
                    let mut nf = 0;
                    for p in &marked {
                        if std::fs::remove_file(p).is_ok() {
                            nf += 1;
                        }
                    }
                    let mut ns = 0;
                    for p in &marked_s {
                        if std::fs::remove_file(p).is_ok() {
                            ns += 1;
                        }
                        let side = p.with_extension(""); // subagent sidecar dir
                        if side.is_dir() {
                            let _ = std::fs::remove_dir_all(&side);
                        }
                    }
                    marked.clear();
                    marked_s.clear();
                    flash = format!("deleted {} file(s), {} session(s)", nf, ns);
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
            Some("p") if focus == Focus::Sessions => {
                // Park a session, or wake it from parking. A parked one
                // stays in the list, out of the counts and at the bottom,
                // until it works again.
                if let Some(s) = sess.get(sel_s) {
                    let tag = s.tag.clone();
                    flash = if cfg.parked.remove(&tag) {
                        format!("{} is back in the list", tag)
                    } else {
                        cfg.parked.insert(tag.clone());
                        format!("{} parked", tag)
                    };
                    config::write_parked(&cfg.parked);
                }
            }
            Some("y") if focus == Focus::Sessions => {
                // Copy the session id, to hand another CC session so it
                // can read this one's transcript ("what happened in ...").
                if let Some(s) = sess.get(sel_s) {
                    crust::clipboard_copy(&s.id, "clipboard");
                    flash = format!("copied {} ({})", s.id, s.tag);
                }
            }
            Some("w") if focus == Focus::Sessions => {
                // Set the workspace this session's glass opens on. The
                // prompt is prefilled with the current value.
                if let Some(s) = sess.get(sel_s) {
                    let tag = s.tag.clone();
                    let mut pref =
                        cfg.session_prefs.get(&tag).cloned().unwrap_or_default();
                    let init = pref.ws.map(|w| (w + 1).to_string()).unwrap_or_default();
                    let mut p = Pane::new(1, rows, cols, 1, 231, 236);
                    let ans = p.ask_or_cancel(
                        &format!("workspace for {} (1-9, 0=10, - none): ", tag), &init);
                    if let Some(a) = ans {
                        let a = a.trim();
                        let ok = if a == "-" || a.is_empty() {
                            pref.ws = None;
                            true
                        } else if let Ok(n) = a.parse::<u32>() {
                            if n == 0 {
                                pref.ws = Some(9); // 0 = workspace 10
                                true
                            } else if (1..=10).contains(&n) {
                                pref.ws = Some(n - 1);
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        };
                        if ok {
                            config::write_session_pref(&tag, &pref);
                            cfg = Config::load();
                            flash = match pref.ws {
                                Some(w) => format!("{} opens on ws {}", tag, w + 1),
                                None => format!("{}: workspace cleared", tag),
                            };
                        } else {
                            flash = "workspace must be 1-10 (0=10, or -)".into();
                        }
                    }
                }
            }
            Some("b") if focus == Focus::Sessions => {
                // Pick this session's glass background with prism. prism
                // writes the hex to --out so its TUI keeps the terminal.
                if let Some(s) = sess.get(sel_s) {
                    let tag = s.tag.clone();
                    let out = std::env::temp_dir()
                        .join(format!("fleet-prism-{}", std::process::id()));
                    // Preload prism with the current colour so it shows
                    // what is already set.
                    let cur_bg = cfg.session_prefs.get(&tag).and_then(|pf| pf.bg.clone());
                    let mut args = vec![
                        "--pick".to_string(),
                        "--hex".to_string(),
                        format!("--out={}", out.display()),
                    ];
                    if let Some(b) = &cur_bg {
                        args.push(format!("#{}", b));
                    }
                    Crust::cleanup();
                    let _ = Command::new("prism").args(&args).status();
                    Crust::init();
                    Crust::clear_screen();
                    let (c, r) = Crust::terminal_size();
                    cols = c;
                    rows = r;
                    // prism writes "fg=#RRGGBB\nbg=#RRGGBB"; the picked
                    // colour is the fg slot.
                    let hex = std::fs::read_to_string(&out)
                        .ok()
                        .and_then(|s| {
                            s.lines()
                                .find_map(|l| l.strip_prefix("fg="))
                                .map(|h| h.trim().trim_start_matches('#').to_lowercase())
                        })
                        .filter(|s| s.len() == 6 && s.chars().all(|c| c.is_ascii_hexdigit()));
                    let _ = std::fs::remove_file(&out);
                    let mut pref =
                        cfg.session_prefs.get(&tag).cloned().unwrap_or_default();
                    match hex {
                        Some(h) => {
                            pref.bg = Some(h.clone());
                            config::write_session_pref(&tag, &pref);
                            cfg = Config::load();
                            flash = format!("{} bg set to #{}", tag, h);
                        }
                        None => flash = "no colour picked".into(),
                    }
                }
            }
            Some("c") => {
                rollup_rows = Some(rollup::today(&sessions::load_tags()));
            }
            Some("?") | Some("h") => help(),
            Some("M") => show_log(),
            Some("RESIZE") => {
                let (c, r) = Crust::terminal_size();
                cols = c;
                rows = r;
            }
            _ => {}
        }
        sel_path = sess.get(sel_s).map(|s| s.path.clone());
    }
    Crust::cleanup();
}

fn draw_header(cols: u16, sess: &[Session], items: &[inbox::Item], rates: &str) {
    // Darker than the column-header bars (236), so the two read apart.
    let mut pane = Pane::new(1, 1, cols, 1, 255, 234);
    let yours = sess.iter().filter(|s| s.state == State::Yours).count();
    let working = sess.iter().filter(|s| s.state == State::Working).count();
    let mut line = format!(" {}  ", style::bold("fleet"));
    line.push_str(&style::styled(&format!("{} YOURS", yours), Some(208), None, "b"));
    line.push_str(&format!("  ·  {} working  ·  {} sessions", working, sess.len()));
    line.push_str(&format!("  ·  inbox {}", items.len()));
    if !rates.is_empty() {
        line.push_str(&format!("  ·  {}", rates));
    }
    pad(&mut line, cols as usize);
    pane.set_text(&line);
    pane.refresh();
}

/// Usage percentages, statusline-style: "[4%/28%(51%) Fri 21:00]".
/// Read from the statusline's own OAuth usage cache; the running CC
/// sessions keep that file fresh, so fleet only stats it and re-reads
/// on mtime change. Never fetches anything itself.
fn refresh_rates(cache: &mut (std::time::SystemTime, String)) {
    let path = "/tmp/claude-oauth-usage-cache.json";
    let mtime = match std::fs::metadata(path).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return,
    };
    if mtime == cache.0 {
        return;
    }
    cache.0 = mtime;
    cache.1 = build_rates(path).unwrap_or_default();
}

fn rate_color(p: u64) -> u8 {
    if p < 50 { 65 } else if p < 80 { 136 } else { 124 }
}

fn build_rates(path: &str) -> Option<String> {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let (mut sess, mut week, mut fable, mut resets) = (None, None, None, None);
    for l in v["limits"].as_array()? {
        match l["kind"].as_str().unwrap_or("") {
            "session" => sess = l["percent"].as_u64(),
            "weekly_all" => {
                week = l["percent"].as_u64();
                resets = l["resets_at"].as_str().map(str::to_string);
            }
            "weekly_scoped" => {
                if l["scope"]["model"]["display_name"] == "Fable" {
                    fable = l["percent"].as_u64();
                }
            }
            _ => {}
        }
    }
    let g = |s: &str| style::fg(s, 242);
    let mut out = g("[");
    if let Some(p) = sess {
        out += &style::fg(&format!("{}%", p), rate_color(p));
    }
    if let Some(p) = week {
        out += &g("/");
        out += &style::fg(&format!("{}%", p), rate_color(p));
    }
    if let Some(p) = fable {
        out += &g("(");
        out += &style::fg(&format!("{}%", p), rate_color(p));
        out += &g(")");
    }
    if let Some(e) = resets.as_deref().and_then(rollup::iso_to_epoch) {
        let d = Command::new("date")
            .args(["-d", &format!("@{}", e), "+%a %H:%M"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if !d.is_empty() {
            out += &style::fg(&format!(" {}", d), 240);
        }
    }
    out += &g("]");
    Some(out)
}

/// Full-width bold header bar on a dark background, for readability.
fn header_bar(text: &str, cols: u16) -> String {
    let mut s = text.to_string();
    pad(&mut s, cols as usize);
    format!("{}\n", style::styled(&s, Some(250), Some(236), "b"))
}

/// Context size coloring, identical to the CC statusline's [NN%]: green
/// under 50 % of the window, yellow under 75 %, red 203 above. The window
/// is ~/.fleetrc ctx_window_k (default 1000).
fn ctx_color(k: u64, window_k: u64) -> u8 {
    let pct = k * 100 / window_k.max(1);
    if pct < 50 { 78 } else if pct < 75 { 220 } else { 203 }
}

fn draw_sessions(cols: u16, y: u16, h: u16, sess: &[Session], focused: bool,
                 sel: usize, marked: &[std::path::PathBuf], window_k: u64) {
    let mut pane = Pane::new(1, y, cols, h, 231, 0);
    let hdr = format!(
        " {:<w$}  {:<7}  {:>6}  {:>2}  {:>5}  {:<9}  {}",
        "SESSION", "STATE", "AGE", "WS", "CTX", "MODEL", "LAST PROMPT", w = TAG_W
    );
    let mut out = header_bar(&hdr, cols);
    let take = (h as usize).saturating_sub(1).min(sess.len());
    for (i, s) in sess.iter().take(take).enumerate() {
        let width = (cols as usize).saturating_sub(43 + TAG_W);
        // Colors follow the CC statusline: bookmark tags magenta 13,
        // model bold blue, context green/yellow/red, timestamps gray 242.
        let ctx = s.ctx_k.map(|k| format!("{}k", k)).unwrap_or_else(|| "·".into());
        let line = format!(
            " {}  {}  {}  {:>2}  {}  {}  {}",
            // Bookmarked tags magenta like the statusline; unbookmarked
            // sessions show their directory name in light gray.
            style::fg(&clip(&s.tag, TAG_W), if s.tagged { 13 } else { 250 }),
            style::styled(&format!("{:<7}", s.state.label()),
                          Some(state_color(s.state)), None,
                          if s.state == State::Yours { "b" } else { "" }),
            style::fg(&format!("{:>6}", fmt_age(s.age_secs)), 242),
            s.ws.map(|w| (w + 1).to_string()).unwrap_or_else(|| "·".into()),
            style::fg(&format!("{:>5}", ctx),
                      s.ctx_k.map(|k| ctx_color(k, window_k)).unwrap_or(242)),
            style::styled(&format!("{:<9}", clip(&s.model, 9)), Some(33), None, "b"),
            clip_end(&s.prompt, width.max(10))
        );
        // Delete-flagged: the whole row dark red, selection via the bar.
        let line = if marked.contains(&s.path) {
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

fn draw_inbox(cols: u16, y: u16, h: u16, items: &[inbox::Item],
              focused: bool, sel: usize, marked: &[std::path::PathBuf],
              logs: &[inbox::LogEntry], addrs: &[&str]) {
    let mut pane = Pane::new(1, y, cols, h, 231, 0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let take = (h as usize).saturating_sub(1).min(items.len());
    let room = (h as usize).saturating_sub(1 + take);
    // Message routes, precomputed so column 1 can size to the widest.
    let routes: Vec<String> = logs
        .iter()
        .take(room)
        .map(|e| {
            // A sender that is not a deliverable address gets a "?", the
            // same mark as an unsigned message: a reply to it goes into a
            // directory the receiving hook never reads. #rust signing
            // itself "fe2o3" is how this showed up.
            let from = if e.from.is_empty() { "?".to_string() } else { e.from.clone() };
            let known = addrs.iter().any(|a| *a == from);
            let mark = if from == "?" || known { "" } else { "?" };
            format!("#{}{} → #{}", from, mark, e.dest)
        })
        .collect();
    // One column-1 width shared by header, files and messages, so every
    // AGE and text column lines up. Clamped so a long route can't push
    // the text off the pane.
    let c1 = std::iter::once("INBOX".len())
        .chain(items.iter().take(take).map(|i| i.label.chars().count().min(8)))
        .chain(routes.iter().map(|r| r.chars().count()))
        .max()
        .unwrap_or(8)
        .clamp(8, 30);
    let txt_w = (cols as usize).saturating_sub(c1 + 11).max(10);
    let hdr = format!(" {:<c1$}  {:>6}  {}", "INBOX", "AGE", "FILE", c1 = c1);
    let mut out = header_bar(&hdr, cols);
    if items.is_empty() && logs.is_empty() {
        out.push_str(&style::dim("  nothing waiting"));
    }
    for (i, it) in items.iter().take(take).enumerate() {
        let label = format!("{:<c1$}", clip(&it.label, c1), c1 = c1);
        let line = format!(
            " {}  {}  {}",
            style::fg(&label, 13),
            style::fg(&format!("{:>6}", fmt_age(it.age_secs)), 242),
            clip_end(&it.name, txt_w)
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
    // Pending bus messages (awaiting delivery), dim, below the items.
    for (n, e) in logs.iter().take(room).enumerate() {
        // "#<from> → #<dest>   <age>   <text>" — the row reads from → to.
        let route = format!("{:<c1$}", clip(&routes[n], c1), c1 = c1);
        let line = format!(
            " {}  {:>6}  {}",
            route,
            fmt_age(now.saturating_sub(e.ts)),
            clip_end(&e.text, txt_w),
        );
        let flagged = e
            .path
            .as_deref()
            .map(|p| marked.iter().any(|m| m.as_path() == p))
            .unwrap_or(false);
        let row = if flagged {
            style::fg(crust::strip_ansi(&line).trim_end(), 88)
        } else {
            style::fg(&line, 245)
        };
        if focused && sel == take + n {
            out.push_str(&bg_keep(row.trim_end(), 238));
        } else {
            out.push_str(&row);
        }
        out.push('\n');
    }
    pane.set_text(out.trim_end_matches('\n'));
    pane.refresh();
}

/// Switch to the session's workspace by injecting tile's own hotkey
/// (frame supports XTEST). Falls back to naming the workspace.
fn jump(tag: &str, ws: u32) -> String {
    // tile binds Mod4+0 to workspace 10, Mod4+1..9 to the rest.
    let key = if ws == 9 { "super+0".to_string() } else { format!("super+{}", ws + 1) };
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

/// PATH lookup: glass's -e does a raw execve, no PATH search, and a
/// failed exec silently falls back to bare. Hand it absolute paths.
fn which(cmd: &str) -> Option<String> {
    for dir in std::env::var("PATH").ok()?.split(':') {
        let p = std::path::Path::new(dir).join(cmd);
        if p.is_file() {
            return Some(p.to_string_lossy().to_string());
        }
    }
    None
}

/// A session with no window on this display: resume it in a fresh glass.
/// Tagged sessions go through `c <tag>` (CC-sessions: path + auto-follow);
/// untagged ones get a plain resume in their own working directory.
fn resurrect(s: &Session, pref: Option<&config::SessionPref>) -> String {
    // Open on the tag's configured workspace: switch there first so tile
    // maps the new glass on it (tile places new windows on the current
    // workspace). No-op when the tag has no workspace set.
    if let Some(ws) = pref.and_then(|p| p.ws) {
        // tile binds Mod4+0 to workspace 10, Mod4+1..9 to the rest.
        let key = if ws == 9 { "super+0".to_string() } else { format!("super+{}", ws + 1) };
        let _ = Command::new("xdotool")
            .args(["key", &key])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    // Through setsid: the new glass gets its own session and process
    // group, so quitting fleet (or closing the terminal fleet runs in)
    // no longer takes the resumed session down with it.
    let mut c = Command::new("setsid");
    c.arg("glass");
    // Per-session background: glass reads GLASS_BG and overrides .glassrc.
    if let Some(bg) = pref.and_then(|p| p.bg.as_ref()) {
        c.env("GLASS_BG", bg);
    }
    // Resume by name only while the name belongs to this session alone.
    // `c <tag>` opens the first bookmark carrying the tag, so a name
    // left on a second, usually long-dead session opens that one and it
    // reads as fleet opening the session twice. The row's own id is the
    // thing you pressed, so fall back to it.
    let owns_tag = s.tagged && sessions::tag_owners(&s.tag) == [s.id.clone()];
    // A session on a gateway model (cck: Kimi through OpenRouter) carries
    // a provider/model name. Resume it through cck, which sets the same
    // endpoint and model again; c or claude would bring it back on the
    // default Claude model.
    let gateway = s.model.contains('/');
    if owns_tag {
        let resumer = if gateway { "cck" } else { "c" };
        let Some(bin) = which(resumer) else {
            return format!("session resumer '{}' not in PATH", resumer);
        };
        c.args(["-e", &bin, &s.tag]);
    } else {
        let resumer = if gateway { "cck" } else { "claude" };
        let Some(bin) = which(resumer) else {
            return format!("'{}' not in PATH", resumer);
        };
        c.args(["-e", &bin, "--resume", &s.id]);
        if !s.cwd.is_empty() {
            c.current_dir(&s.cwd);
        }
    }
    // stdin too: a child that inherits fleet's pty keeps it open for its
    // whole life, and glass only ends when the pty hangs up. With an
    // inherited stdin, quitting fleet left its glass black until every
    // session resumed from it had ended.
    match c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        Ok(_) => format!("resuming {} in a new glass", s.tag),
        Err(e) => format!("glass failed: {}", e),
    }
}

/// The full bus traffic log in a scrollable popup, newest first.
fn show_log() {
    let logs = inbox::log_tail(500);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut t = String::new();
    if logs.is_empty() {
        t.push_str(" no bus traffic logged yet");
    }
    for e in &logs {
        t.push_str(&format!(" {:>6}  → {}: {}\n",
                            fmt_age(now.saturating_sub(e.ts)), e.dest, e.text));
    }
    let (cols, rows) = Crust::terminal_size();
    let w = cols.saturating_sub(8).clamp(40, 110);
    let h = ((logs.len().max(1) + 2) as u16).clamp(5, rows.saturating_sub(4));
    Popup::centered(w, h, 231, 236).view(t.trim_end_matches('\n'));
}

/// Append one line to the bus log. Fleet logs only phone-bound sends it
/// observes; deliveries are logged by the receiving hook, so each
/// message lands in the log exactly once.
fn log_append(dest: &str, text: &str) {
    let line: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(120)
        .collect();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = config::home().join(".fleet/log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        use std::io::Write;
        let _ = writeln!(f, "{}\t{}\t{}", ts, dest, line);
    }
}

/// Type a line into a session's terminal and press Enter, as the user
/// would. Addressed at the window, so nothing has to be focused and the
/// keys can never land in fleet itself. glass masks the synthetic bit off
/// its event type (`and eax, 0x7F`), so it takes these as real presses.
fn type_prompt(xid: u32, text: &str) {
    let win = xid.to_string();
    let run = |args: &[&str]| {
        let _ = Command::new("xdotool")
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    };
    run(&["type", "--window", &win, "--delay", "12", text]);
    run(&["key", "--window", &win, "Return"]);
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
    t.push_str(&format!("{}park it: listed, uncounted, at the bottom\n", key("p")));
    t.push_str(&format!("{}copy its session id to the clipboard\n", key("y")));
    t.push_str(&format!("{}set the workspace it opens on\n", key("w")));
    t.push_str(&format!("{}pick its glass background (prism)\n", key("b")));
    t.push_str(&format!("{}stop the session: off (K forces)\n", key("k")));
    t.push_str(&format!("{}flag an idle/off session for deletion\n", key("d")));
    t.push_str(&format!(" {}\n", hdr("INBOX")));
    t.push_str(&format!("{}open a file; wake a msg's session\n", key("o / Enter")));
    t.push_str(&format!("{}flag the item for deletion\n", key("d")));
    t.push_str(&format!("{}on a msg row: clear that message\n", key("<")));
    t.push_str(&format!(" {}\n", hdr("GLOBAL")));
    t.push_str(&format!("{}switch sessions / inbox\n", key("TAB")));
    t.push_str(&format!("{}select\n", key("↑ ↓")));
    t.push_str(&format!("{}delete everything flagged\n", key("<")));
    t.push_str(&format!("{}message log popup (all bus traffic)\n", key("M")));
    t.push_str(&format!("{}today's token rollup (Esc back)\n", key("c")));
    t.push_str(&format!("{}this help (Esc / q / Enter closes)\n", key("?")));
    t.push_str(&format!("{}quit", key("q")));
    Popup::centered(50, 18, 231, 236).view(&t);
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
            Focus::Sessions => " q quit · TAB inbox · ↑↓ · Enter jump/resume · m message · p park · k stop · d flag · < purge · c today · ? help".to_string(),
            Focus::Inbox => " q quit · TAB sessions · ↑↓ · Enter open/wake · d flag file/msg · < delete flagged · M log · ? help".to_string(),
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

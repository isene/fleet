//! Finished turns, for the phone.
//!
//! At the end of each answer the hook writes the session's latest turn to
//! ~/.fleet/relay/done-<tag>.json. Syncthing carries the relay folder to
//! the fleet server, and the server hands the file to the phone's fleet
//! app, which rings for every finished turn or only for work that had an
//! ETA, as its two switches say.
//!
//! An answer with "ETA" in it means work goes on after it. The hook keeps
//! that line in ~/.fleet/eta/<session id>. The first later answer that has
//! no ETA and no words for running work ends it: that turn goes out as
//! kind "eta", with the ETA it had.
//!
//! fleet's ETA column shows the clock time of each session's open ETA,
//! read from ~/.fleet/eta.
//!
//! Battery: one small file write per finished answer, never on a timer,
//! and no event without a relay folder. The file lies beside the tag
//! folders, not in one, so the fleet-bus Stop hook's `relay/*/*` loop
//! never sees it.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Words that say work still runs. A draft that says so without an ETA is
/// sent back by the style hook, and its rewrite has the ETA again, so such
/// a draft must not count as the end of the work.
const RUNNING: [&str; 7] = [
    "still running", "still working", "still building", "still testing",
    "under test", "will report", "report when",
];

pub fn note(v: &Value, now: u64) {
    note_in(&crate::config::home(), v, now, &std::env::var("CC_RESUME_TAGS").unwrap_or_default());
}

fn note_in(home: &Path, v: &Value, now: u64, tags: &str) {
    let (Some(text), Some(id)) = (v["last_assistant_message"].as_str(), v["session_id"].as_str()) else {
        return;
    };
    let id = safe(id);
    let mark = home.join(".fleet/eta").join(&id);
    let mut kind = "turn";
    let mut eta = String::new();
    if let Some(line) = eta_line(text) {
        if std::fs::read_to_string(&mark).ok().as_deref() != Some(line.as_str()) {
            let _ = std::fs::create_dir_all(home.join(".fleet/eta"));
            let _ = std::fs::write(&mark, &line);
        }
    } else if !running(text) {
        if let Ok(old) = std::fs::read_to_string(&mark) {
            let _ = std::fs::remove_file(&mark);
            kind = "eta";
            eta = cut(&old, 140);
        }
    }
    let relay = home.join(".fleet/relay");
    if !relay.is_dir() {
        return;
    }
    let tag = safe(tags.split([',', ' ']).find(|t| !t.is_empty()).unwrap_or(""));
    let tag = if tag.is_empty() { id.chars().take(8).collect() } else { tag };
    let event = json!({"tag": tag, "t": now, "kind": kind, "eta": eta, "line": first_line(text)});
    let path = relay.join(format!("done-{}.json", tag));
    let tmp = relay.join(format!(".done-{}.tmp", tag));
    if std::fs::write(&tmp, event.to_string() + "\n").is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

pub fn eta_dir() -> PathBuf {
    crate::config::home().join(".fleet/eta")
}

/// The clock time of a session's open ETA, as "14:20", or "?" when it
/// gives none ("ETA unknown"). Empty when the session has no open ETA.
pub fn eta(id: &str) -> String {
    std::fs::read_to_string(eta_dir().join(safe(id)))
        .map(|l| eta_clock(&l).unwrap_or_else(|| "?".into()))
        .unwrap_or_default()
}

/// The clock time of the newest ETA in a text, for a session still at
/// work: most ETAs are said mid-turn ("while the scenarios run, ETA about
/// 12:31"), and the hook sees only the turn's last answer.
pub fn eta_in(text: &str) -> Option<String> {
    text.lines().rev().find_map(eta_clock)
}

fn safe(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}

/// The first line that gives an ETA, kept whole so its clock time stays.
fn eta_line(text: &str) -> Option<String> {
    text.lines().find(|l| eta_clock(l).is_some()).map(plain)
}

/// Verbs that give a finish time with a clock time after them: sessions
/// write "it all ends about 13:05" as often as "ETA about 13:05".
const FINISH: [&str; 8] = ["ends", "end", "finishes", "finish", "done", "ready", "completes", "lands"];

/// The time a line gives for work to end: a clock time or "unknown"
/// after the word ETA ("ETA about 14:20", "**ETA:** 9:05", "ETA unknown"
/// gives "?"), or a clock time after a finish verb and "about", "by",
/// "around", "before" or "at" ("ends about 13:05"). A line that only names
/// the word, as the phone's switch "ETA work done" or a quoted "ETA about
/// HH:MM", gives none, and so does "started about 12:00".
fn eta_clock(line: &str) -> Option<String> {
    let w: Vec<&str> = line.split(|c: char| !(c.is_alphanumeric() || c == ':'))
        .map(|t| t.trim_matches(':')).filter(|t| !t.is_empty()).collect();
    let is_clock = |t: &str| t.split_once(':').is_some_and(|(h, m)| {
        (1..=2).contains(&h.len()) && m.len() == 2 && h.bytes().chain(m.bytes()).all(|b| b.is_ascii_digit())
    });
    let at = |i: usize| w.get(i).copied().unwrap_or("");
    for i in 0..w.len() {
        let next = at(i + 1).to_lowercase();
        if w[i] == "ETA" {
            if next == "unknown" {
                return Some("?".into());
            }
            if is_clock(at(i + 1)) {
                return Some(at(i + 1).into());
            }
            if matches!(next.as_str(), "about" | "around" | "roughly") && is_clock(at(i + 2)) {
                return Some(at(i + 2).into());
            }
        } else if FINISH.contains(&w[i].to_lowercase().as_str())
            && matches!(next.as_str(), "about" | "around" | "by" | "before" | "at")
            && is_clock(at(i + 2))
        {
            return Some(at(i + 2).into());
        }
    }
    None
}

fn running(text: &str) -> bool {
    let low = text.to_lowercase();
    RUNNING.iter().any(|w| low.contains(w))
}

/// The answer's opening line, as the notification shows it.
fn first_line(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    cut(&plain(line), 140)
}

fn plain(line: &str) -> String {
    line.trim().trim_start_matches(['#', '-', '*', ' ']).replace("**", "").replace('`', "")
}

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max - 1).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stop(home: &Path, text: &str, t: u64) -> Value {
        note_in(home, &json!({"session_id": "s-1", "last_assistant_message": text}), t, "ci");
        let f = home.join(".fleet/relay/done-ci.json");
        serde_json::from_str(&std::fs::read_to_string(f).unwrap()).unwrap()
    }

    #[test]
    fn an_eta_then_a_plain_answer_is_the_end_of_the_work() {
        let home = std::env::temp_dir().join(format!("fleet-done-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".fleet/relay")).unwrap();
        let e = stop(&home, "The suite runs.\n\n- ETA about 14:20, from the last run.", 1);
        assert_eq!((e["kind"].as_str(), e["line"].as_str()), (Some("turn"), Some("The suite runs.")));
        // A draft about running work with no ETA is no end: the style hook sends it back.
        assert_eq!(stop(&home, "The suite is still running.", 2)["kind"], "turn");
        let e = stop(&home, "**All 675 tests pass.**", 3);
        assert_eq!(e["kind"], "eta");
        assert_eq!(e["eta"], "ETA about 14:20, from the last run.");
        assert_eq!(e["line"], "All 675 tests pass.");
        assert_eq!(e["t"], 3);
        // The work ended once; the next answer is a plain turn again.
        assert_eq!(stop(&home, "Done.", 4)["kind"], "turn");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn no_relay_folder_no_event_but_the_eta_is_kept() {
        let home = std::env::temp_dir().join(format!("fleet-done-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        note_in(&home, &json!({"session_id": "s", "last_assistant_message": "ETA about 14:20"}), 1, "ci");
        assert!(!home.join(".fleet/relay").exists());
        assert_eq!(eta_clock(&std::fs::read_to_string(home.join(".fleet/eta/s")).unwrap()).as_deref(), Some("14:20"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_newest_eta_in_a_text() {
        assert_eq!(eta_in("Tests pass, ETA about 11:28.\nThe docs next, ETA about 12:31.").as_deref(), Some("12:31"));
        assert_eq!(eta_in("Now three more checks."), None);
    }

    #[test]
    fn the_clock_a_line_gives() {
        let c = |l: &str| eta_clock(l);
        assert_eq!(c("ETA about 14:20, from the last run.").as_deref(), Some("14:20"));
        assert_eq!(c("At 10:00 it began; **ETA:** 9:05").as_deref(), Some("9:05"));
        assert_eq!(c("ETA unknown: this is its first run.").as_deref(), Some("?"));
        // As sessions wrote it on 2026-10-22, with no word ETA.
        assert_eq!(c("The faults are planted one at a time; it all ends about 13:05.").as_deref(), Some("13:05"));
        assert_eq!(c("The build should be done by 10:25.").as_deref(), Some("10:25"));
        assert_eq!(c("The run started about 12:00."), None);
    }

    #[test]
    fn eta_is_a_word_of_its_own() {
        assert!(eta_line("Builds now; ETA: 14:20").is_some());
        assert!(eta_line("- **ETA:** about 9:05").is_some());
        assert!(eta_line("ETA about 20 min").is_none());
        assert!(eta_line("It asks for \"ETA about HH:MM\".").is_none());
        assert!(eta_line("ETA unknown: the run has no earlier length").is_some());
        assert!(eta_line("The BETA build and the ETAs").is_none());
        // A false alarm seen on 2026-10-21: the name of the phone's switch.
        assert!(eta_line("Two switches, \"ETA work done\" and \"Every finished turn\".").is_none());
        assert_eq!(cut("abcdef", 4), "abc…");
    }
}

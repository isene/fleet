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
//! Battery: one small file write per finished answer, never on a timer,
//! and none at all without a relay folder. The file lies beside the tag
//! folders, not in one, so the fleet-bus Stop hook's `relay/*/*` loop
//! never sees it.

use serde_json::{json, Value};
use std::path::Path;

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
    let relay = home.join(".fleet/relay");
    let (Some(text), Some(id)) = (v["last_assistant_message"].as_str(), v["session_id"].as_str()) else {
        return;
    };
    if !relay.is_dir() {
        return;
    }
    let id = safe(id);
    let tag = safe(tags.split([',', ' ']).find(|t| !t.is_empty()).unwrap_or(""));
    let tag = if tag.is_empty() { id.chars().take(8).collect() } else { tag };
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
            eta = old;
        }
    }
    let event = json!({"tag": tag, "t": now, "kind": kind, "eta": eta, "line": first_line(text)});
    let path = relay.join(format!("done-{}.json", tag));
    let tmp = relay.join(format!(".done-{}.tmp", tag));
    if std::fs::write(&tmp, event.to_string() + "\n").is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn safe(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}

/// Words that may follow "ETA" when an answer gives one: "ETA about
/// 14:20", "ETA unknown", "ETA in 5 min". A number counts too.
const ETA_NEXT: [&str; 7] = ["about", "around", "roughly", "unknown", "in", "under", "by"];

/// The first line that gives an ETA. A line that only names the word, as
/// the phone's switch "ETA work done", gives none.
fn eta_line(text: &str) -> Option<String> {
    text.lines().find(|l| gives_eta(l)).map(|l| cut(&plain(l), 140))
}

fn gives_eta(line: &str) -> bool {
    line.match_indices("ETA").any(|(i, _)| {
        let rest = &line[i + 3..];
        if line[..i].chars().next_back().is_some_and(char::is_alphanumeric)
            || rest.starts_with(char::is_alphanumeric)
        {
            return false;
        }
        let word = rest.trim_start_matches(|c: char| !c.is_alphanumeric())
            .split(|c: char| !c.is_alphanumeric()).next().unwrap_or("");
        word.starts_with(|c: char| c.is_ascii_digit()) || ETA_NEXT.contains(&word.to_lowercase().as_str())
    })
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
    fn no_relay_folder_no_file() {
        let home = std::env::temp_dir().join(format!("fleet-done-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        note_in(&home, &json!({"session_id": "s", "last_assistant_message": "ETA 5 min"}), 1, "ci");
        assert!(!home.join(".fleet").exists());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn eta_is_a_word_of_its_own() {
        assert!(eta_line("Builds now; ETA: 14:20").is_some());
        assert!(eta_line("- **ETA:** about 20 min").is_some());
        assert!(eta_line("ETA unknown: the run has no earlier length").is_some());
        assert!(eta_line("The BETA build and the ETAs").is_none());
        // A false alarm seen on 2026-10-21: the name of the phone's switch.
        assert!(eta_line("Two switches, \"ETA work done\" and \"Every finished turn\".").is_none());
        assert_eq!(cut("abcdef", 4), "abc…");
    }
}

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
use std::collections::HashSet;
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
            // A rename, since fleet reads the marks again only when the
            // folder's mtime moves, and a write in place leaves it alone.
            let tmp = home.join(".fleet/eta").join(format!(".{}.tmp", id));
            let _ = std::fs::create_dir_all(home.join(".fleet/eta"));
            if std::fs::write(&tmp, &line).is_ok() {
                let _ = std::fs::rename(&tmp, &mark);
            }
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

/// The session list for the phone's fleet app: what fleet's own screen
/// shows, in ~/.fleet/relay/sessions.json. fleet writes it when a state,
/// ETA, prompt, workspace, model or open item changes. Age and context
/// size ride along but write nothing alone, or a working session would
/// send a file every tick. Between writes it costs one string compare.
pub fn publish(sess: &[crate::sessions::Session], auto: &HashSet<String>, now: u64, last: &mut u64) {
    publish_in(&crate::config::home(), sess, auto, now, last);
}

/// `auto` holds the tags fleet wakes by itself (its » mark). The workspace
/// goes out counted from 1, as fleet's screen shows it.
///
/// Every 2 s tick hashes what the phone shows, about 4 µs for 15 sessions
/// (the JSON itself took 45 µs). Age and context size are left out of the
/// hash, so they alone never write the file.
fn publish_in(home: &Path, sess: &[crate::sessions::Session], auto: &HashSet<String>, now: u64, last: &mut u64) {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for s in sess {
        (&s.tag, auto.contains(&s.tag), s.state.label(), &s.eta, s.ws, &s.model, &s.prompt).hash(&mut h);
        s.moves.len().hash(&mut h);
        for m in &s.moves {
            (m.num, &m.text).hash(&mut h);
        }
    }
    let key = h.finish();
    if key == *last {
        return;
    }
    *last = key;
    let relay = home.join(".fleet/relay");
    if !relay.is_dir() {
        return;
    }
    let rows: Vec<Value> = sess.iter().map(|s| {
        let moves: Vec<Value> = s.moves.iter()
            .map(|m| json!({"num": m.num, "text": cut(&m.text, 300)}))
            .collect();
        json!({"tag": s.tag, "auto": auto.contains(&s.tag), "state": s.state.label(),
               "eta": s.eta, "ws": s.ws.map(|w| w + 1), "model": s.model,
               "prompt": cut(&s.prompt, 200), "moves": moves,
               "at": now.saturating_sub(s.age_secs), "ctx": s.ctx_k})
    }).collect();
    let tmp = relay.join(".sessions.tmp");
    if std::fs::write(&tmp, json!({"t": now, "sessions": rows}).to_string() + "\n").is_ok() {
        let _ = std::fs::rename(&tmp, relay.join("sessions.json"));
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

/// The latest ETA in a text, for a session still at work: most ETAs are
/// said mid-turn ("while the scenarios run, ETA about 12:31"), and the hook
/// sees only the turn's last answer.
pub fn eta_in(text: &str) -> Option<String> {
    text.lines().filter_map(eta_clock).max_by_key(|c| minutes(c))
}

fn safe(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}

/// The line that gives the latest ETA, kept whole so its clock time stays.
fn eta_line(text: &str) -> Option<String> {
    text.lines().filter_map(|l| eta_clock(l).map(|c| (minutes(&c), l)))
        .max_by_key(|(m, _)| *m).map(|(_, l)| plain(l))
}

/// Verbs that give a finish time with "at" and a clock time: "done at 14:20".
const FINISH: [&str; 8] = ["ends", "end", "finishes", "finish", "done", "ready", "completes", "lands"];

/// Words that make the clock time after them a start, as in "started about 12:00".
const START: [&str; 6] = ["started", "start", "began", "begun", "since", "from"];

/// The latest time a line gives for work to end, read as the style hook
/// reads it: a clock time after "ETA", "about", "around", "by", "before",
/// "until" or "~" ("ETA 9:05", "finish at about 15:02"), or after a finish
/// verb and "at" ("done at 14:20"). "ETA unknown" gives "?". A line that only
/// names the word, as the phone's switch "ETA work done", gives none, and so
/// do a quoted "ETA about 13:27" and "started about 12:00".
fn eta_clock(line: &str) -> Option<String> {
    let line = unquoted(line).replace('~', " about ");
    let w: Vec<&str> = line.split(|c: char| !(c.is_alphanumeric() || c == ':'))
        .map(|t| t.trim_matches(':')).filter(|t| !t.is_empty()).collect();
    let is_clock = |t: &str| t.split_once(':').is_some_and(|(h, m)| {
        (1..=2).contains(&h.len()) && m.len() == 2 && h.bytes().chain(m.bytes()).all(|b| b.is_ascii_digit())
    });
    let at = |i: usize| w.get(i).copied().unwrap_or("");
    let mut best: Option<&str> = None;
    let mut unknown = false;
    for i in 0..w.len() {
        let low = w[i].to_lowercase();
        let clock = if w[i] == "ETA" {
            unknown |= at(i + 1).eq_ignore_ascii_case("unknown");
            Some(at(i + 1))
        } else if matches!(low.as_str(), "about" | "around" | "roughly" | "by" | "before" | "until" | "till") {
            (!START.contains(&at(i.wrapping_sub(1)).to_lowercase().as_str())).then(|| at(i + 1))
        } else if FINISH.contains(&low.as_str()) && at(i + 1).eq_ignore_ascii_case("at") {
            Some(at(i + 2))
        } else {
            None
        };
        if let Some(c) = clock.filter(|c| is_clock(c)) {
            if best.is_none_or(|b| minutes(c) >= minutes(b)) {
                best = Some(c);
            }
        }
    }
    best.map(String::from).or(unknown.then(|| "?".into()))
}

/// Minutes since midnight of "14:20"; "?" counts as 0.
fn minutes(clock: &str) -> u32 {
    clock.split_once(':').and_then(|(h, m)| Some(h.parse::<u32>().ok()? * 60 + m.parse::<u32>().ok()?)).unwrap_or(0)
}

/// The line with its quotes cut out: a session that quotes another
/// session's "ETA about 13:27" gives no ETA of its own.
fn unquoted(line: &str) -> String {
    let mut open = None;
    line.chars().filter(|&c| {
        let close = match c { '“' => Some('”'), '"' | '`' => Some(c), _ => None };
        match open {
            Some(end) if c == end => { open = None; false }
            Some(_) => false,
            None if close.is_some() => { open = close; false }
            None => true,
        }
    }).collect()
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
    fn the_session_list_is_written_when_a_state_changes_and_not_for_age() {
        let home = std::env::temp_dir().join(format!("fleet-view-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".fleet/relay")).unwrap();
        let file = home.join(".fleet/relay/sessions.json");
        let one = |state, age_secs| crate::sessions::Session {
            id: "s".into(), tag: "ci".into(), cwd: String::new(), path: PathBuf::new(),
            state, tagged: true, age_secs, model: "opus".into(), prompt: "Run the suite".into(),
            pid: None, ws: Some(3), ctx_k: Some(120), at_prompt: false,
            moves: vec![crate::open::Open { num: 2, at: 0, text: "Restart kastrup?".into() }],
            eta: "14:20".into(),
        };
        let mut last = 0;
        let auto: HashSet<String> = ["ci".to_string()].into();
        publish_in(&home, &[one(crate::sessions::State::Working, 5)], &auto, 100, &mut last);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let s = &v["sessions"][0];
        assert_eq!((s["tag"].as_str(), s["state"].as_str(), s["eta"].as_str()), (Some("ci"), Some("working"), Some("14:20")));
        // Workspace 3 counted from 0 is the 4 on fleet's screen.
        assert_eq!((s["ws"].as_u64(), s["at"].as_u64(), s["ctx"].as_u64()), (Some(4), Some(95), Some(120)));
        assert_eq!(s["auto"], true);
        assert_eq!(s["moves"][0]["num"], 2);
        // An older age alone writes nothing.
        std::fs::remove_file(&file).unwrap();
        publish_in(&home, &[one(crate::sessions::State::Working, 50)], &auto, 145, &mut last);
        assert!(!file.exists());
        // The answer is in: written again.
        publish_in(&home, &[one(crate::sessions::State::Yours, 0)], &auto, 150, &mut last);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!((v["t"].as_u64(), v["sessions"][0]["state"].as_str()), (Some(150), Some("YOURS")));
        // Auto-wake switched off with `a`: written again.
        publish_in(&home, &[one(crate::sessions::State::Yours, 0)], &HashSet::new(), 160, &mut last);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!((v["t"].as_u64(), v["sessions"][0]["auto"].as_bool()), (Some(160), Some(false)));
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
    fn a_new_eta_moves_the_folder_mtime() {
        // Seen on 2026-10-22: ci moved its ETA from 13:16 to 13:27, and
        // fleet went on showing 13:16, since its cache waits for the folder.
        let home = std::env::temp_dir().join(format!("fleet-done-mtime-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let say = |text: &str| note_in(&home, &json!({"session_id": "s", "last_assistant_message": text}), 1, "ci");
        say("ETA about 13:16");
        let dir = home.join(".fleet/eta");
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
        std::fs::File::open(&dir).unwrap().set_modified(old).unwrap();
        say("ETA about 13:27");
        assert_ne!(std::fs::metadata(&dir).unwrap().modified().unwrap(), old);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temp file left");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_latest_eta_in_a_text() {
        assert_eq!(eta_in("Tests pass, ETA about 11:28.\nThe docs next, ETA about 12:31.").as_deref(), Some("12:31"));
        assert_eq!(eta_in("Now three more checks."), None);
        assert_eq!(eta_in("Scores at about 15:20.\nci's runs arrive at about 15:05.").as_deref(), Some("15:20"));
        assert_eq!(eta_line("Runs end about 15:02.\nScores follow about 15:20.").as_deref(), Some("Scores follow about 15:20."));
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
        // As DI wrote it on 2026-10-22: "at about", and two times in one line.
        assert_eq!(c("All five columns finish at about 15:02, and the scores follow at about 15:20.").as_deref(), Some("15:20"));
        assert_eq!(c("Done at 14:20, ~14:35 at the latest.").as_deref(), Some("14:35"));
        assert_eq!(c("gpt-6.1-sol finished at 14:44, so my 16:00 guess was too late."), None);
        // Seen on 2026-10-22: one session quoting another's ETA got that ETA.
        assert_eq!(c("ci's own answer says \"ETA about 13:27\", while fleet shows 13:16"), None);
        assert_eq!(c("It said “ETA about 13:27” and `ends about 13:30`."), None);
        assert_eq!(c("He asked \"why\"; ETA about 14:20.").as_deref(), Some("14:20"));
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

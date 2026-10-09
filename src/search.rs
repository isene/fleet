//! Search what every session has said: your prompts and Claude's answers
//! in every transcript under ~/.claude/projects.
//!
//! It runs only when asked for. The transcripts are big (2 GB on the
//! machine this was written on) and nearly all of it is tool output, so
//! a line is parsed only when a word of the query is in it. The newest
//! files are read first, and reading stops once enough is found that no
//! older file can add to it.

use crate::config::home;
use crate::{rollup, sessions};
use crust::style;
use memchr::{memchr, memmem, memrchr};
use serde_json::Value;
use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Read this much of a file at a time. A line longer than this (a tool
/// result with a whole file in it) makes the buffer grow for that line.
const CHUNK: usize = 4 << 20;

/// One message with every word of the query in it.
pub struct Hit {
    /// The transcript it is in.
    pub path: PathBuf,
    /// The session id: the transcript's file name.
    pub id: String,
    /// When it was said, in seconds since 1970.
    pub at: u64,
    /// You said it. False when Claude did.
    pub user: bool,
    /// All of what was said.
    pub text: String,
}

/// The words of a query, in small letters. A message is a hit when all
/// of them are in it, in any order and any case.
pub fn words(query: &str) -> Vec<String> {
    query.split_whitespace().map(|w| w.to_lowercase()).collect()
}

/// What to look for in the raw file, with its A to Z made small: the
/// longest word of the query. A quote is written differently in the
/// file, so a word with one is no use, and with no other word every
/// line is parsed.
///
/// The quick search folds A to Z only. A word with other letters is
/// looked for twice: as typed small, and in capitals. That finds
/// "blåbær", "Blåbær" and "BLÅBÆR", and misses "blÅbær", which nobody
/// writes. Parsing every line instead took four times as long.
fn needles(words: &[String]) -> Vec<Vec<u8>> {
    let Some(word) = words.iter()
        .filter(|w| !w.contains(['"', '\\']))
        .max_by_key(|w| w.len()) else { return Vec::new() };
    let mut out = vec![word.as_bytes().to_vec()];
    let capitals = word.to_uppercase().to_ascii_lowercase().into_bytes();
    if capitals != out[0] {
        out.push(capitals);
    }
    out
}

fn secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The newest `max` messages with all the words of `query` in them,
/// newest first.
pub fn run(query: &str, max: usize) -> Vec<Hit> {
    run_in(&home().join(".claude/projects"), query, max)
}

pub fn run_in(projects: &Path, query: &str, max: usize) -> Vec<Hit> {
    let words = words(query);
    if words.is_empty() || max == 0 {
        return Vec::new();
    }
    // One transcript per session, straight under its project folder.
    // What lies deeper belongs to subagents, which talk to no one.
    let mut files: Vec<(u64, PathBuf)> = Vec::new();
    for d in std::fs::read_dir(projects).into_iter().flatten().flatten() {
        for f in std::fs::read_dir(d.path()).into_iter().flatten().flatten() {
            let path = f.path();
            if path.extension().map(|e| e != "jsonl").unwrap_or(true) {
                continue;
            }
            match f.metadata() {
                Ok(m) if m.is_file() => files.push((m.modified().map(secs).unwrap_or(0), path)),
                _ => {}
            }
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    let needles = needles(&words);
    let mut hits: Vec<Hit> = Vec::new();
    for (mtime, path) in files {
        // Nothing in a file is newer than the file. Once the list is
        // full, a file older than its last row has nothing to add.
        if hits.len() >= max && hits.last().is_some_and(|h| mtime < h.at) {
            break;
        }
        hits.extend(in_file(&path, mtime, &words, &needles, max));
        hits.sort_by(|a, b| b.at.cmp(&a.at));
        hits.truncate(max);
    }
    hits
}

/// The newest `max` hits of one transcript.
fn in_file(path: &Path, mtime: u64, words: &[String], needles: &[Vec<u8>], max: usize) -> Vec<Hit> {
    let Ok(mut f) = std::fs::File::open(path) else { return Vec::new() };
    let id = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let finders: Vec<memmem::Finder> = needles.iter().map(|n| memmem::Finder::new(n)).collect();
    let mut lines: Vec<(usize, usize)> = Vec::new();
    let mut raw: Vec<u8> = Vec::new();
    let mut low: Vec<u8> = Vec::new();
    // A file runs from old to new, so the last `max` hits are the newest.
    let mut kept: VecDeque<Hit> = VecDeque::new();
    let mut first = true;
    loop {
        let had = raw.len();
        raw.resize(had + CHUNK, 0);
        let mut got = 0;
        while got < CHUNK {
            match f.read(&mut raw[had + got..]) {
                Ok(0) | Err(_) => break,
                Ok(n) => got += n,
            }
        }
        raw.truncate(had + got);
        let eof = got < CHUNK;
        if first {
            first = false;
            if headless(&raw) {
                return Vec::new();
            }
        }
        // Whole lines only. The cut-off rest waits for the next read.
        let end = if eof {
            raw.len()
        } else {
            match memrchr(b'\n', &raw) {
                Some(i) => i + 1,
                None => continue,
            }
        };
        let block = &raw[..end];
        let mut take = |line: &[u8]| {
            if let Some((at, user, text)) = hit(line, words) {
                kept.push_back(Hit { path: path.to_path_buf(), id: id.clone(), at: at.unwrap_or(mtime), user, text });
                if kept.len() > max {
                    kept.pop_front();
                }
            }
        };
        if finders.is_empty() {
            block.split(|b| *b == b'\n').for_each(&mut take);
        } else {
            low.clear();
            low.extend_from_slice(block);
            low.make_ascii_lowercase();
            // The lines a needle is in, each once and in the file's order.
            lines.clear();
            for find in &finders {
                let mut from = 0;
                while from < low.len() {
                    let Some(i) = find.find(&low[from..]) else { break };
                    let pos = from + i;
                    let start = memrchr(b'\n', &low[..pos]).map_or(0, |i| i + 1);
                    let stop = memchr(b'\n', &low[pos..]).map_or(low.len(), |i| pos + i);
                    lines.push((start, stop));
                    from = stop + 1;
                }
            }
            lines.sort_unstable();
            lines.dedup();
            for (start, stop) in &lines {
                take(&block[*start..*stop]);
            }
        }
        raw.drain(..end);
        if eof {
            break;
        }
    }
    kept.into()
}

/// A run of `claude -p` from a script: no one sat in it, and fleet does
/// not list it. Its first record says how it was started.
fn headless(head: &[u8]) -> bool {
    const KEY: &[u8] = b"\"entrypoint\":\"";
    memmem::find(head, KEY).is_some_and(|i| head[i + KEY.len()..].starts_with(b"sdk-cli"))
}

/// When, who and what, if this line of a transcript is something said
/// with every word in it.
fn hit(line: &[u8], words: &[String]) -> Option<(Option<u64>, bool, String)> {
    // The answer of a tool: by far the biggest lines, and nothing said.
    if memmem::find(line, b"\"tool_use_id\":\"").is_some() {
        return None;
    }
    let v: Value = serde_json::from_slice(line).ok()?;
    let (user, text) = said(&v)?;
    let low = text.to_lowercase();
    if !words.iter().all(|w| low.contains(w.as_str())) {
        return None;
    }
    let at = v["timestamp"].as_str().and_then(rollup::iso_to_epoch);
    Some((at, user, text))
}

/// What a record says to the other side: your prompt, or the text of
/// Claude's answer. Not its thinking, not a tool call, not what a
/// subagent wrote, and not the notes Claude Code adds for itself.
fn said(v: &Value) -> Option<(bool, String)> {
    if v["isSidechain"] == true || v["isMeta"] == true || v["isCompactSummary"] == true {
        return None;
    }
    let content = &v["message"]["content"];
    let user = match v["type"].as_str()? {
        "user" => true,
        "assistant" => false,
        _ => return None,
    };
    let blocks: Vec<&str> = match content {
        Value::String(s) if user => vec![s.as_str()],
        Value::Array(a) => a.iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect(),
        _ => return None,
    };
    let kept: Vec<&str> = blocks.into_iter()
        .map(str::trim)
        .filter(|t| !t.is_empty() && !(user && sessions::not_the_user(t)))
        .collect();
    if kept.is_empty() {
        return None;
    }
    Some((user, kept.join("\n\n")))
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Where the words are in a line of letters, as (first, one past last),
/// from left to right and never one inside another.
fn places(line: &[char], words: &[String]) -> Vec<(usize, usize)> {
    let low: Vec<char> = line.iter().map(|c| lower(*c)).collect();
    let words: Vec<Vec<char>> = words.iter()
        .map(|w| w.chars().map(lower).collect::<Vec<char>>())
        .filter(|w| !w.is_empty())
        .collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < low.len() {
        match words.iter().filter(|w| low[i..].starts_with(w)).map(|w| w.len()).max() {
            Some(n) => {
                out.push((i, i + n));
                i += n;
            }
            None => i += 1,
        }
    }
    out
}

/// One line of a message, at most `room` letters, around the first
/// place a word of the query is in it.
pub fn snippet(text: &str, words: &[String], room: usize) -> String {
    let flat: Vec<char> = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().collect();
    if flat.len() <= room {
        return flat.into_iter().collect();
    }
    let first = places(&flat, words).first().map_or(0, |p| p.0);
    // A few words before it, so the line reads as part of a sentence,
    // and a start on a word where there is one.
    let mut start = first.saturating_sub(room / 4);
    while start > 0 && start < first && flat[start - 1] != ' ' {
        start += 1;
    }
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    let stop = (start + room.saturating_sub(2)).min(flat.len());
    out.extend(&flat[start..stop]);
    if stop < flat.len() {
        out.push('…');
    }
    out
}

/// The line with the words of the query in bold yellow.
pub fn marked(line: &str, words: &[String]) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut at = 0;
    for (a, b) in places(&chars, words) {
        out.extend(&chars[at..a]);
        out.push_str(&style::styled(&chars[a..b].iter().collect::<String>(), Some(226), None, "b"));
        at = b;
    }
    out.extend(&chars[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(at: &str, text: &str) -> String {
        serde_json::json!({"type": "user", "timestamp": at, "entrypoint": "cli",
                           "message": {"role": "user", "content": text}}).to_string()
    }

    fn claude(at: &str, text: &str) -> String {
        serde_json::json!({"type": "assistant", "timestamp": at,
            "message": {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "a kart in my thoughts"},
                {"type": "text", "text": text},
                {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "kart --tool-call"}}]}}).to_string()
    }

    fn tool(at: &str, text: &str) -> String {
        serde_json::json!({"type": "user", "timestamp": at,
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": text}]}}).to_string()
    }

    /// A projects folder with two sessions and one script run in it.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fleet-search-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("-home-a")).unwrap();
        std::fs::create_dir_all(dir.join("-home-b/one/subagents")).unwrap();
        std::fs::write(dir.join("-home-a/one.jsonl"), [
            user("2026-10-01T10:00:00.000Z", "Could we make a game called Kart?"),
            claude("2026-10-01T10:05:00.000Z", "The kart game is there.\nEight karts on four tracks."),
            tool("2026-10-01T10:06:00.000Z", "kart kart kart: the output of a tool"),
            user("2026-10-01T10:07:00.000Z", "<system-reminder>kart is a word in a note</system-reminder>"),
            serde_json::json!({"type": "user", "timestamp": "2026-10-01T10:08:00.000Z", "isMeta": true,
                               "message": {"content": "the kart skill, loaded by Claude Code"}}).to_string(),
        ].join("\n") + "\n").unwrap();
        std::fs::write(dir.join("-home-b/two.jsonl"), [
            user("2026-10-02T09:00:00.000Z", "Blåbær til frokost, og en KART over byen"),
            claude("2026-10-02T09:01:00.000Z", "She said \"kart\" twice."),
            user("2026-09-30T09:00:00.000Z", "Ærlig talt, og ÆRLIG ment"),
        ].join("\n")).unwrap();
        std::fs::write(dir.join("-home-b/one/subagents/agent.jsonl"),
                       user("2026-10-03T09:00:00.000Z", "kart, said to a subagent")).unwrap();
        std::fs::write(dir.join("-home-b/script.jsonl"),
            serde_json::json!({"type": "user", "timestamp": "2026-10-04T09:00:00.000Z", "entrypoint": "sdk-cli",
                               "message": {"content": "kart, asked by a script"}}).to_string()).unwrap();
        dir
    }

    fn texts(hits: &[Hit]) -> Vec<&str> {
        hits.iter().map(|h| h.text.as_str()).collect()
    }

    #[test]
    fn what_was_said_is_found_newest_first() {
        let dir = scratch("said");
        let hits = run_in(&dir, "kart", 10);
        assert_eq!(texts(&hits), [
            "She said \"kart\" twice.",
            "Blåbær til frokost, og en KART over byen",
            "The kart game is there.\nEight karts on four tracks.",
            "Could we make a game called Kart?",
        ], "no tool output, no note, no skill text, no subagent, no script run");
        assert_eq!(hits[0].id, "two");
        assert!(!hits[0].user && hits[1].user);
        assert_eq!(hits[3].at, 1790848800, "2026-10-01 10:00 UTC");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_word_must_be_there_in_any_order_and_case() {
        let dir = scratch("words");
        assert_eq!(texts(&run_in(&dir, "TRACKS kart", 10)), ["The kart game is there.\nEight karts on four tracks."]);
        assert!(run_in(&dir, "kart submarine", 10).is_empty());
        assert!(run_in(&dir, "   ", 10).is_empty());
        // Letters beyond a to z: small, with a capital first, all capitals.
        assert_eq!(texts(&run_in(&dir, "BLÅBÆR", 10)), ["Blåbær til frokost, og en KART over byen"]);
        assert_eq!(texts(&run_in(&dir, "ærlig", 10)), ["Ærlig talt, og ÆRLIG ment"]);
        assert_eq!(needles(&words("Blåbær")), ["blåbær".as_bytes().to_vec(), "blÅbÆr".as_bytes().to_vec()]);
        assert_eq!(needles(&words("a kart")), [b"kart".to_vec()]);
        // A quote is written differently in the file: every line is parsed.
        assert!(needles(&words("\"kart\"")).is_empty());
        assert_eq!(texts(&run_in(&dir, "\"kart\"", 10)), ["She said \"kart\" twice."]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_full_list_keeps_the_newest() {
        let dir = scratch("max");
        assert_eq!(texts(&run_in(&dir, "kart", 2)), [
            "She said \"kart\" twice.",
            "Blåbær til frokost, og en KART over byen",
        ]);
        assert_eq!(texts(&run_in(&dir, "kart", 3)).len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_line_longer_than_one_read_is_still_one_line() {
        let dir = scratch("long");
        let big = "x".repeat(CHUNK + 1000);
        std::fs::write(dir.join("-home-a/big.jsonl"), [
            tool("2026-10-05T08:00:00.000Z", &big),
            user("2026-10-05T08:01:00.000Z", "a zeppelin after the long line"),
            claude("2026-10-05T08:02:00.000Z", &format!("{} and a zeppelin at the end", big)),
        ].join("\n")).unwrap();
        let hits = run_in(&dir, "zeppelin", 10);
        assert_eq!(hits.len(), 2);
        assert!(hits[0].text.ends_with("a zeppelin at the end") && !hits[0].user);
        assert_eq!(hits[1].text, "a zeppelin after the long line");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_line_shown_is_around_the_word() {
        let w = words("zeppelin");
        assert_eq!(snippet("A  short\nmessage", &w, 40), "A short message");
        let long = format!("{} then the Zeppelin rose over the town {}", "word ".repeat(30), "tail ".repeat(30));
        let s = snippet(&long, &w, 40);
        assert!(s.starts_with('…') && s.ends_with('…'), "{s}");
        assert!(s.contains("the Zeppelin rose"), "{s}");
        assert!(s.chars().count() <= 40, "{s}");
        // A word that is nowhere: the start of the message.
        assert!(snippet(&long, &words("submarine"), 40).starts_with("word word"));
    }

    #[test]
    fn the_words_are_marked_where_they_stand() {
        let plain = |s: &str| crust::strip_ansi(s);
        let m = marked("A Kart and a kart-track", &words("kart track"));
        assert_eq!(plain(&m), "A Kart and a kart-track");
        assert_eq!(m.matches(style::RESET).count(), 3, "{m:?}");
        assert_eq!(marked("nothing here", &words("kart")), "nothing here");
    }
}

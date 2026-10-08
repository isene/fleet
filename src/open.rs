//! Open items: what each session still waits for the user to do or decide.
//!
//! A Stop hook (`fleet --hook`) reads every finished answer and keeps one
//! small file per session, ~/.fleet/open/<session id>. An answer opens an
//! item with a numbered row of its decisions table or a "Your move 7:"
//! line, and closes items with a line "Closed: 3, 5". fleet lists the
//! files, so an item stays until its session closes it or the user
//! deletes it. A later answer, a new prompt or a restart no longer wipes
//! it, and a session that forgets to close one leaves a row the user can
//! see and delete.
//!
//! A number is used once per session, so "3 y" from the user means the
//! same item a week later. The hook sends an answer back once when a
//! number is missing or taken, and names the numbers that are free.
//!
//! Battery: the hook runs once per finished answer, never on a timer. The
//! list costs fleet one stat of the folder per pass, and a read only after
//! a file in it changed.

use crate::config::home;
use serde_json::Value;
use std::path::PathBuf;

pub fn dir() -> PathBuf {
    home().join(".fleet/open")
}

fn file(id: &str) -> PathBuf {
    let safe: String = id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    dir().join(safe)
}

/// One open item, as fleet lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Open {
    pub num: u32,
    pub at: u64, // when it was opened, epoch seconds
    pub text: String,
}

struct Item {
    num: u32,
    turn: u32, // the turn whose answer opened it
    at: u64,
    text: String,
}

/// One session's file. `next` is the lowest number never used, and `base`
/// is what it was when the current turn began: a new item takes a number
/// from `base` up. `turn` counts the user's prompts that led to an answer
/// with items, and `prompt` is the id of the last one.
struct Ledger {
    next: u32,
    base: u32,
    turn: u32,
    prompt: String,
    items: Vec<Item>,
}

/// What one answer says about open items.
#[derive(Default, Debug, PartialEq)]
struct Said {
    items: Vec<(Option<u32>, String)>,
    closed: Vec<u32>,
    faults: Vec<String>,
}

fn keep(text: &str) -> String {
    text.replace('\t', " ").chars().take(240).collect()
}

fn short(text: &str) -> String {
    text.chars().take(40).collect()
}

/// What an item is about: the question, without the recommendation that
/// may change while it stays open.
fn subject(text: &str) -> &str {
    text.split(" → ").next().unwrap_or(text)
}

/// Read an answer for open items. A decisions table is one with a
/// "Question" column; its row reads "question → recommendation" and takes
/// its number from the first column. A move is a line that starts with
/// "Your move 7:" or "Your move:", as a list item or in bold or bare. A
/// mention further into a line is not one, and neither is code.
fn said(answer: &str) -> Said {
    let mut out = Said::default();
    let mut question: Option<usize> = None; // its column, inside a decisions table
    let mut fenced = false;
    for l in answer.lines() {
        let l = l.trim();
        if l.starts_with("```") || l.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if let Some(row) = l.strip_prefix('|') {
            let cells: Vec<&str> = row.trim_end_matches('|').split('|').map(str::trim).collect();
            match question {
                None => question = cells.iter().position(|c| c.eq_ignore_ascii_case("question")),
                Some(q) if q < cells.len() && cells[q].chars().any(|c| c != '-' && c != ':') => {
                    let mut s = cells[q].to_string();
                    if let Some(rec) = cells.get(q + 1).filter(|r| !r.is_empty()) {
                        s.push_str(" → ");
                        s.push_str(rec);
                    }
                    let num = if q > 0 { cells[0].trim_matches('*').parse().ok() } else { None };
                    out.items.push((num, keep(&s)));
                }
                Some(_) => {} // the rule under the header, or a short row
            }
            continue;
        }
        question = None;
        let l = l.trim_start_matches(|c: char| c == '-' || c == '*' || c.is_whitespace());
        let after = |s: &str| s.trim_start_matches(|c: char| c == '*' || c.is_whitespace()).trim_end().to_string();
        if let Some(rest) = l.strip_prefix("Closed:") {
            // Numbers and nothing else. A line that mixes in words closes
            // nothing: a row that stays is seen, one closed by a number
            // in a remark is a question lost.
            let rest = after(rest);
            let words: Vec<&str> = rest
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|w| !w.is_empty() && *w != "and")
                .collect();
            let nums: Vec<u32> = words.iter()
                .filter_map(|w| w.trim_end_matches('.').parse().ok())
                .collect();
            if nums.len() == words.len() {
                out.closed.extend(nums);
            } else if words[0].trim_end_matches('.').parse::<u32>().is_ok() {
                out.faults.push(format!("the line \"Closed: {}\" must hold numbers only", short(&rest)));
            }
            continue;
        }
        let Some((num, rest)) = l.strip_prefix("Your move").and_then(|r| r.split_once(':')) else { continue };
        let num = num.trim();
        if !num.is_empty() && num.parse::<u32>().is_err() {
            continue; // "Your move lines go last: ..." is a sentence
        }
        let rest = after(rest);
        if !rest.is_empty() {
            out.items.push((num.parse().ok(), keep(&rest)));
        }
    }
    out
}

impl Ledger {
    fn new() -> Ledger {
        Ledger { next: 1, base: 1, turn: 0, prompt: String::new(), items: Vec::new() }
    }

    fn read(id: &str) -> Ledger {
        let mut l = Ledger::new();
        let Ok(text) = std::fs::read_to_string(file(id)) else { return l };
        let mut lines = text.lines();
        let mut head = lines.next().unwrap_or("").split('\t');
        l.next = head.next().and_then(|n| n.parse().ok()).unwrap_or(1);
        l.base = head.next().and_then(|n| n.parse().ok()).unwrap_or(l.next);
        l.turn = head.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        l.prompt = head.next().unwrap_or("").to_string();
        for line in lines {
            let mut f = line.splitn(4, '\t');
            let mut n = || f.next().and_then(|v| v.parse::<u64>().ok());
            if let (Some(num), Some(turn), Some(at)) = (n(), n(), n()) {
                let num = num as u32;
                l.next = l.next.max(num + 1);
                l.items.push(Item { num, turn: turn as u32, at, text: f.next().unwrap_or("").to_string() });
            }
        }
        l
    }

    /// Write the file whole, through a temp file, so a reader never sees
    /// half of it and the folder's mtime tells fleet to read again.
    fn write(&self, id: &str) {
        let mut out = format!("{}\t{}\t{}\t{}\n", self.next, self.base, self.turn, self.prompt);
        for i in &self.items {
            out.push_str(&format!("{}\t{}\t{}\t{}\n", i.num, i.turn, i.at, i.text));
        }
        let path = file(id);
        let tmp = path.with_extension("tmp");
        let _ = std::fs::create_dir_all(dir());
        if std::fs::write(&tmp, out).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// Take one finished answer in. `prompt` is the id of the user's
    /// prompt it answers. `again` says a hook sent the last answer back,
    /// so this one belongs to the same turn whatever its id says.
    ///
    /// Returns what was wrong with the answer's numbers. An item with a
    /// fault is still kept, under the next free number, and nothing is
    /// dropped that the answer does not close: a row too many is seen
    /// and can be deleted, a question lost is not seen at all.
    fn apply(&mut self, said: &Said, prompt: &str, again: bool, now: u64) -> Vec<String> {
        if !(again || (!prompt.is_empty() && prompt == self.prompt)) {
            self.turn += 1;
            self.base = self.next;
            self.prompt = prompt.to_string();
        }
        let turn = self.turn;
        self.items.retain(|i| !said.closed.contains(&i.num));
        let mut faults = said.faults.clone();
        let mut late: Vec<&String> = Vec::new(); // items to number once the rest are in
        for (num, text) in &said.items {
            let Some(n) = *num else {
                // A rewrite may leave out the number fleet gave it.
                if !self.items.iter().any(|i| i.text == *text) {
                    faults.push(format!("\"{}\" has no number", short(text)));
                    late.push(text);
                }
                continue;
            };
            match self.items.iter_mut().find(|i| i.num == n) {
                // The same item again: listed once more, its recommendation
                // changed, or reworded in the rewrite of this turn's answer.
                Some(i) if i.turn == turn || subject(&i.text) == subject(text) => i.text = text.clone(),
                Some(i) => {
                    faults.push(format!("number {} is open as \"{}\"", n, short(&i.text)));
                    late.push(text);
                }
                None if n < self.base => {
                    faults.push(format!("number {} was used before", n));
                    late.push(text);
                }
                None => {
                    self.items.push(Item { num: n, turn, at: now, text: text.clone() });
                    self.next = self.next.max(n + 1);
                }
            }
        }
        for text in late {
            if !self.items.iter().any(|i| i.text == *text) {
                self.items.push(Item { num: self.next, turn, at: now, text: text.clone() });
                self.next += 1;
            }
        }
        faults
    }
}

/// A session's open items, lowest number first.
pub fn load(id: &str) -> Vec<Open> {
    let mut items: Vec<Open> = Ledger::read(id).items.into_iter()
        .map(|i| Open { num: i.num, at: i.at, text: i.text })
        .collect();
    items.sort_by_key(|i| i.num);
    items
}

/// The user deletes an item by hand.
pub fn delete(id: &str, num: u32) {
    let mut l = Ledger::read(id);
    l.items.retain(|i| i.num != num);
    l.write(id);
}

/// A session's transcript is gone, and its list goes with it.
pub fn forget(id: &str) {
    let _ = std::fs::remove_file(file(id));
}

/// The Stop hook. Takes the hook's JSON, updates the session's file, and
/// returns the line to print when the answer has to be written again.
/// It sends an answer back once per turn at most, and an answer that
/// opens and closes nothing touches no file.
pub fn hook(input: &str, now: u64) -> Option<String> {
    let v: Value = serde_json::from_str(input).ok()?;
    if v["hook_event_name"] != "Stop" {
        return None;
    }
    let id = v["session_id"].as_str()?;
    let said = said(v["last_assistant_message"].as_str()?);
    if said.items.is_empty() && said.closed.is_empty() && said.faults.is_empty() {
        return None;
    }
    let again = v["stop_hook_active"].as_bool().unwrap_or(false);
    let mut l = Ledger::read(id);
    let faults = l.apply(&said, v["prompt_id"].as_str().unwrap_or(""), again, now);
    l.write(id);
    if faults.is_empty() || again {
        return None;
    }
    let open: Vec<String> = l.items.iter()
        .filter(|i| i.turn != l.turn)
        .map(|i| format!("{} ({})", i.num, short(&i.text)))
        .collect();
    let reason = format!(
        "fleet lists what you still ask of the user, and this answer does not fit its list: {}. \
         Each decisions row has a number in its # column, and each action reads \
         \"Your move N: ...\". A number is used once in a session: new items in this answer \
         take numbers from {} up. Open from before: {}. An open item keeps its number and \
         its question. Close one that is answered, done or dropped with a line \
         \"Closed: N, M\" (numbers only) above the moves. Write the answer again with the \
         numbers put right.",
        faults.join("; "),
        l.base,
        if open.is_empty() { "none".to_string() } else { open.join(", ") },
    );
    Some(serde_json::json!({"decision": "block", "reason": reason}).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Take answers in, one per prompt, and return the faults of the last.
    fn turns(l: &mut Ledger, answers: &[&str]) -> Vec<String> {
        let mut faults = Vec::new();
        for a in answers {
            let prompt = format!("p{}", l.turn + 1);
            faults = l.apply(&said(a), &prompt, false, 100);
        }
        faults
    }

    fn open(l: &Ledger) -> Vec<(u32, &str)> {
        l.items.iter().map(|i| (i.num, i.text.as_str())).collect()
    }

    #[test]
    fn a_move_line_is_found_in_any_dress() {
        let a = "Done.\n\n- **Your move 4:** ship it?\nYour move: pick a name\n  * Your move:  \n";
        assert_eq!(said(a).items, vec![
            (Some(4), "ship it?".to_string()), (None, "pick a name".to_string())]);
    }

    #[test]
    fn a_mention_is_not_a_move() {
        assert_eq!(said("It lists every \"Your move:\" line."), Said::default());
        assert_eq!(said("Your move lines go last: after the table."), Said::default());
        assert_eq!(said("```\nYour move 3: run it\nClosed: 1\n```"), Said::default(), "code");
    }

    #[test]
    fn each_row_of_a_decisions_table_is_an_item() {
        let a = "Built.\n\n| # | Question | My recommendation |\n|---|:--|---|\n\
                 | 7 | Ship it? | Yes: the tests pass |\n| 8 | Rename it? | |\n\n\
                 Your move 9: run the script\n";
        assert_eq!(said(a).items, vec![
            (Some(7), "Ship it? → Yes: the tests pass".to_string()),
            (Some(8), "Rename it?".to_string()),
            (Some(9), "run the script".to_string())]);
        assert_eq!(said("| File | Size |\n|---|---|\n| a.rs | 12 |\n"), Said::default(),
                   "no Question column, no items");
    }

    #[test]
    fn a_closed_line_takes_numbers_only() {
        assert_eq!(said("- Closed: 3, 5").closed, vec![3, 5]);
        assert_eq!(said("**Closed:** 4 and 6.").closed, vec![4, 6]);
        // A number in a remark closes nothing, and the session is told.
        let s = said("Closed: 3 (2 of 3 tests pass)");
        assert!(s.closed.is_empty());
        assert_eq!(s.faults.len(), 1);
        assert_eq!(said("Closed: the ticket, at last."), Said::default(), "a sentence");
    }

    #[test]
    fn an_item_stays_open_until_it_is_closed() {
        let mut l = Ledger::new();
        // Later answers that say nothing of it, as after an auto-wake.
        assert!(turns(&mut l, &["Your move 1: run it", "Read two messages."]).is_empty());
        assert_eq!(open(&l), vec![(1, "run it")]);
        assert!(turns(&mut l, &["Closed: 1, 9\nYour move 2: restart"]).is_empty());
        assert_eq!(open(&l), vec![(2, "restart")]);
    }

    #[test]
    fn a_number_is_used_once() {
        let mut l = Ledger::new();
        turns(&mut l, &["Your move 1: run it\nYour move 2: look", "Closed: 1"]);
        // 1 is closed and 2 is open: neither is free for a new item.
        let faults = turns(&mut l, &["Your move 1: again\nYour move 2: other\nYour move: bare"]);
        assert_eq!(faults.len(), 3);
        // Nothing asked is lost: the three take the next free numbers.
        assert_eq!(open(&l), vec![(2, "look"), (3, "again"), (4, "other"), (5, "bare")]);
        // An open item listed again, word for word, is fine.
        assert!(turns(&mut l, &["Your move 2: look"]).is_empty());
        assert_eq!(l.items.len(), 4);
    }

    #[test]
    fn an_open_question_may_change_its_recommendation() {
        let table = |rec: &str| format!("| # | Question | Rec |\n|--|--|--|\n| 1 | Ship it? | {} |", rec);
        let mut l = Ledger::new();
        assert!(turns(&mut l, &[&table("Yes"), &table("No: a test fails")]).is_empty());
        assert_eq!(open(&l), vec![(1, "Ship it? → No: a test fails")]);
    }

    #[test]
    fn a_rewrite_updates_its_first_try() {
        let mut l = Ledger::new();
        turns(&mut l, &["Your move 1: old"]);
        // The first try had a bare move, and the hook sent it back.
        assert_eq!(l.apply(&said("Your move: run it"), "p2", false, 100).len(), 1);
        assert_eq!(open(&l), vec![(1, "old"), (2, "run it")]);
        // The rewrite numbers and rewords it. Its prompt id may differ.
        assert!(l.apply(&said("Your move 2: run the script"), "", true, 100).is_empty());
        assert_eq!(open(&l), vec![(1, "old"), (2, "run the script")]);
        assert_eq!(l.next, 3);
    }

    #[test]
    fn a_second_answer_in_a_turn_keeps_the_first_ones_items() {
        // A bus message lands at the end of a turn: the session goes on
        // and answers again, about the message alone.
        let mut l = Ledger::new();
        l.apply(&said("Your move 1: run it"), "p1", false, 100);
        assert!(l.apply(&said("Read one message."), "p1", true, 100).is_empty());
        assert_eq!(open(&l), vec![(1, "run it")]);
    }

    #[test]
    fn the_hook_sends_a_bare_item_back_once() {
        let id = format!("test-hook-{}", std::process::id());
        let stop = |answer: &str, again: bool| serde_json::json!({
            "hook_event_name": "Stop", "session_id": id, "prompt_id": "p1",
            "stop_hook_active": again, "last_assistant_message": answer}).to_string();
        assert_eq!(hook(&stop("All done.", false), 100), None);
        assert!(!file(&id).exists(), "an answer with no items writes nothing");
        let out = hook(&stop("Your move: run it", false), 100).unwrap();
        assert!(out.contains("\"block\"") && out.contains("from 1 up"), "{}", out);
        assert_eq!(hook(&stop("Your move: run it", true), 200), None, "never twice");
        assert_eq!(load(&id), vec![Open { num: 1, at: 100, text: "run it".into() }]);
        let other = serde_json::json!({"hook_event_name": "SubagentStop", "session_id": id,
            "last_assistant_message": "Your move 5: x"}).to_string();
        assert_eq!(hook(&other, 100), None);
        delete(&id, 1);
        assert!(load(&id).is_empty());
        assert_eq!(Ledger::read(&id).next, 2, "a deleted number is not handed out again");
        forget(&id);
        assert!(!file(&id).exists());
    }
}

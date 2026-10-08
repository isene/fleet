//! Open items: what each session still waits for the user to do or decide.
//!
//! A hook (`fleet --hook`) reads every finished answer and keeps one
//! small file per session, ~/.fleet/open/<session id>. An answer opens an
//! item with a numbered row of its decisions table or a "Your move 7:"
//! line, and closes items with a line "Closed: 3, 5". fleet lists the
//! files, so an item stays until its session closes it or the user
//! deletes it. A later answer, a new prompt or a restart no longer wipes
//! it, and a session that forgets to close one leaves a row the user can
//! see and delete.
//!
//! A closed item's number is free again, so the numbers stay small: a new
//! item takes the lowest one that is not open. The hook sends an answer
//! back once when a number is missing or taken, and names the first free
//! one.
//!
//! The same hook reads each prompt of the user. A line that starts with
//! an open item's number, as in "3 y", answers that item, and fleet hides
//! it while the session works. At the end of the turn it shows again,
//! unless the answer closed it.
//!
//! Battery: the hook runs once per prompt and once per finished answer,
//! never on a timer. The list costs fleet one stat of the folder per pass,
//! and a read only after a file in it changed.

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

/// One session's file. `turn` counts the user's prompts that led to an
/// answer with items, and `prompt` is the id of the last one. `answered`
/// names the items the user's last prompt answered: they stay in the file
/// and out of the list until the turn ends.
struct Ledger {
    turn: u32,
    prompt: String,
    answered: Vec<u32>,
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

/// The numbers a prompt of the user answers: each line that starts with
/// one, as in "3 y", "3: the second" or a bare "3". "3rd" and "3.5" start
/// a sentence.
fn answers(prompt: &str) -> Vec<u32> {
    prompt.lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let end = l.find(|c: char| !c.is_ascii_digit()).unwrap_or(l.len());
            let mut rest = l[end..].chars();
            let sentence = match rest.next() {
                Some(c) if c.is_alphanumeric() => true,
                Some('.') | Some(',') => rest.next().is_some_and(|c| c.is_ascii_digit()),
                _ => false,
            };
            if sentence { None } else { l[..end].parse().ok() }
        })
        .collect()
}

impl Ledger {
    fn new() -> Ledger {
        Ledger { turn: 0, prompt: String::new(), answered: Vec::new(), items: Vec::new() }
    }

    fn read(id: &str) -> Ledger {
        let mut l = Ledger::new();
        let Ok(text) = std::fs::read_to_string(file(id)) else { return l };
        let mut lines = text.lines();
        let head: Vec<&str> = lines.next().unwrap_or("").split('\t').collect();
        // A file from before v0.3.48 starts with two numbers that are no longer used.
        let head = &head[if head.len() == 4 { 2 } else { 0 }..];
        l.turn = head.first().and_then(|n| n.parse().ok()).unwrap_or(0);
        l.prompt = head.get(1).unwrap_or(&"").to_string();
        l.answered = head.get(2).unwrap_or(&"").split(',').filter_map(|n| n.parse().ok()).collect();
        for line in lines {
            let mut f = line.splitn(4, '\t');
            let mut n = || f.next().and_then(|v| v.parse::<u64>().ok());
            if let (Some(num), Some(turn), Some(at)) = (n(), n(), n()) {
                l.items.push(Item { num: num as u32, turn: turn as u32, at, text: f.next().unwrap_or("").to_string() });
            }
        }
        l
    }

    /// The lowest number that no open item has and `skip` does not name.
    fn free(&self, skip: &[u32]) -> u32 {
        (1..).find(|n| !skip.contains(n) && !self.items.iter().any(|i| i.num == *n)).unwrap_or(0)
    }

    /// Write the file whole, through a temp file, so a reader never sees
    /// half of it and the folder's mtime tells fleet to read again.
    fn write(&self, id: &str) {
        let answered: Vec<String> = self.answered.iter().map(|n| n.to_string()).collect();
        let mut out = format!("{}\t{}\t{}\n", self.turn, self.prompt, answered.join(","));
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
    /// fault is still kept, under the lowest free number, and nothing is
    /// dropped that the answer does not close: a row too many is seen
    /// and can be deleted, a question lost is not seen at all.
    ///
    /// A number the answer closes is free from the next answer on. In
    /// one answer "Closed: 3" and a new item 3 would be two questions
    /// under one number, right after the user wrote "3 y".
    fn apply(&mut self, said: &Said, prompt: &str, again: bool, now: u64) -> Vec<String> {
        if !(again || (!prompt.is_empty() && prompt == self.prompt)) {
            self.turn += 1;
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
                None if said.closed.contains(&n) => {
                    faults.push(format!("number {} is closed in this answer and free from the next one", n));
                    late.push(text);
                }
                None => self.items.push(Item { num: n, turn, at: now, text: text.clone() }),
            }
        }
        for text in late {
            if !self.items.iter().any(|i| i.text == *text) {
                let num = self.free(&said.closed);
                self.items.push(Item { num, turn, at: now, text: text.clone() });
            }
        }
        faults
    }
}

/// A session's open items, lowest number first. An item the user has
/// answered is left out while the session works on it, unless `all` asks
/// for it: the session itself must still see it, to close it.
pub fn load(id: &str, all: bool) -> Vec<Open> {
    let l = Ledger::read(id);
    let mut items: Vec<Open> = l.items.into_iter()
        .filter(|i| all || !l.answered.contains(&i.num))
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

/// The hook, at two moments. At a prompt of the user it hides the items
/// the prompt answers. At the end of an answer it takes the answer in,
/// shows what was hidden and not closed, and returns the line to print
/// when the answer has to be written again. It sends an answer back once
/// per turn at most, and it writes a file only when something changed.
pub fn hook(input: &str, now: u64) -> Option<String> {
    let v: Value = serde_json::from_str(input).ok()?;
    let id = v["session_id"].as_str()?;
    if v["hook_event_name"] == "UserPromptSubmit" {
        let mut l = Ledger::read(id);
        let mut nums = answers(v["prompt"].as_str()?);
        nums.retain(|n| l.items.iter().any(|i| i.num == *n));
        if nums != l.answered {
            l.answered = nums;
            l.write(id);
        }
        return None;
    }
    if v["hook_event_name"] != "Stop" {
        return None;
    }
    let said = said(v["last_assistant_message"].as_str()?);
    let mut l = Ledger::read(id);
    let hidden = !l.answered.is_empty();
    l.answered.clear();
    if said.items.is_empty() && said.closed.is_empty() && said.faults.is_empty() {
        if hidden {
            l.write(id);
        }
        return None;
    }
    let again = v["stop_hook_active"].as_bool().unwrap_or(false);
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
         \"Your move N: ...\". A new item takes the lowest number that is not open, \
         from {} up. Open from before: {}. An open item keeps its number and \
         its question. Close one that is answered, done or dropped with a line \
         \"Closed: N, M\" (numbers only) above the moves. A number closed in this answer \
         is free from the next answer on. Write the answer again with the numbers put right.",
        faults.join("; "),
        (1..).find(|n| !said.closed.contains(n) && !l.items.iter().any(|i| i.num == *n && i.turn != l.turn))
            .unwrap_or(0),
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
    fn a_closed_number_is_free_again() {
        let mut l = Ledger::new();
        turns(&mut l, &["Your move 1: run it\nYour move 2: look", "Closed: 1"]);
        // 1 is closed and free for a new item. 2 is open and taken.
        let faults = turns(&mut l, &["Your move 1: again\nYour move 2: other\nYour move: bare"]);
        assert_eq!(faults.len(), 2);
        // Nothing asked is lost: the two take the lowest free numbers.
        assert_eq!(open(&l), vec![(2, "look"), (1, "again"), (3, "other"), (4, "bare")]);
        // An open item listed again, word for word, is fine.
        assert!(turns(&mut l, &["Your move 2: look"]).is_empty());
        assert_eq!(l.items.len(), 4);
    }

    #[test]
    fn a_number_closed_in_an_answer_waits_for_the_next() {
        let mut l = Ledger::new();
        turns(&mut l, &["Your move 1: run it"]);
        // The user wrote "1 y": a new item 1 in the answer to that would mix the two up.
        assert_eq!(turns(&mut l, &["Closed: 1\nYour move 1: restart"]).len(), 1);
        assert_eq!(open(&l), vec![(2, "restart")]);
        assert!(turns(&mut l, &["Your move 1: look"]).is_empty());
        assert_eq!(open(&l), vec![(2, "restart"), (1, "look")]);
    }

    #[test]
    fn an_answered_item_is_hidden_until_the_turn_ends() {
        assert_eq!(answers("1 y\n2nd try failed\n3.5 is the size\n  4: the second\n5.\n6\nSee 7"), vec![1, 4, 5, 6]);
        let id = format!("test-answer-{}", std::process::id());
        let event = |name: &str, key: &str, text: &str, prompt: &str| serde_json::json!({
            "hook_event_name": name, "session_id": id, "prompt_id": prompt, key: text}).to_string();
        let stop = |text: &str, prompt: &str| hook(&event("Stop", "last_assistant_message", text, prompt), 100);
        let says = |text: &str, prompt: &str| hook(&event("UserPromptSubmit", "prompt", text, prompt), 100);
        let listed = || load(&id, false).iter().map(|i| i.num).collect::<Vec<u32>>();
        stop("Your move 1: run it\nYour move 2: look\nYour move 3: wait", "p1");
        // 1 is answered. 9 is not open, and "2nd" starts a sentence.
        assert_eq!(says("1 y\n2nd try failed\n9 n", "p2"), None);
        assert_eq!(listed(), vec![2, 3]);
        assert_eq!(load(&id, true).len(), 3, "the session still sees it");
        // The answer closes nothing, so 1 shows again.
        stop("Looked at it.", "p2");
        assert_eq!(listed(), vec![1, 2, 3]);
        says("Fine.\n2: yes\n3", "p3");
        assert_eq!(listed(), vec![1]);
        stop("Closed: 2", "p3");
        assert_eq!(listed(), vec![1, 3]);
        // A prompt with no answer in it shows what an unfinished turn left hidden.
        says("3 y", "p4");
        says("And one more thing.", "p5");
        assert_eq!(listed(), vec![1, 3]);
        forget(&id);
    }

    #[test]
    fn a_file_from_an_older_fleet_is_read() {
        let id = format!("test-old-{}", std::process::id());
        let _ = std::fs::create_dir_all(dir());
        std::fs::write(file(&id), "5\t4\t7\tp9\n3\t2\t100\told item\n").unwrap();
        let l = Ledger::read(&id);
        assert_eq!((l.turn, l.prompt.as_str(), l.answered.len()), (7, "p9", 0));
        assert_eq!(open(&l), vec![(3, "old item")]);
        forget(&id);
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
        assert_eq!(load(&id, false), vec![Open { num: 1, at: 100, text: "run it".into() }]);
        let other = serde_json::json!({"hook_event_name": "SubagentStop", "session_id": id,
            "last_assistant_message": "Your move 5: x"}).to_string();
        assert_eq!(hook(&other, 100), None);
        delete(&id, 1);
        assert!(load(&id, false).is_empty());
        assert_eq!(hook(&stop("Your move 1: look", false), 300), None, "a deleted number is free again");
        assert_eq!(load(&id, false).len(), 1);
        forget(&id);
        assert!(!file(&id).exists());
    }
}

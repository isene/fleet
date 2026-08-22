//! Session discovery and state, read from ~/.claude/projects transcripts.
//!
//! Each session is one .jsonl file. Only its TAIL is read (last 64 KB),
//! and only when the file's mtime changed since the last refresh, so an
//! idle fleet does one stat per session per tick and no reads at all.

use crate::config::{home, Config};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const TAIL: u64 = 262144;

#[derive(Clone, Copy, PartialEq)]
pub enum State {
    Capped,  // refused by a usage limit; needs a model switch or credits
    Working, // Claude has the turn (or a tool is running)
    Yours,   // the answer is in; waiting for the user
    Idle,    // alive but nothing has happened for idle_mins
    Off,     // no claude process runs this session
    Older,   // bookmarked but beyond the recent window: curated history
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Capped => "CAPPED",
            State::Working => "working",
            State::Yours => "YOURS",
            State::Idle => "idle",
            State::Off => "off",
            State::Older => "older",
        }
    }
    fn rank(self) -> u8 {
        match self {
            State::Capped => 0,
            State::Yours => 1,
            State::Working => 2,
            State::Idle => 3,
            State::Off => 4,
            State::Older => 5,
        }
    }
}

#[allow(dead_code)] // id/cwd feed the coming message bus and jump features
pub struct Session {
    pub id: String,
    pub tag: String,
    pub cwd: String,
    pub path: PathBuf, // the transcript .jsonl

    pub state: State,
    pub tagged: bool, // tag comes from a CC-sessions bookmark
    pub age_secs: u64,
    pub model: String,
    pub prompt: String,
    pub pid: Option<u32>,
    pub ws: Option<u32>,
    pub ctx_k: Option<u64>,
}

#[derive(Clone, Default)]
struct TailInfo {
    last: char, // 'u' user, 'a' assistant text, 't' assistant tool_use
    capped: bool, // newest line is a usage-limit refusal from the client
    model: String,
    prompt: String,
    cwd: String,
    ctx_k: Option<u64>,
}

pub struct Cache {
    tails: HashMap<PathBuf, (SystemTime, TailInfo)>,
    /// The last full /proc sweep, and when it ran. Sweeping every pass
    /// costs one comm read per process on the machine, ~400 of them
    /// every two seconds, to notice a session that starts once an hour.
    procs: HashMap<String, u32>,
    procs_at: SystemTime,
}

impl Cache {
    pub fn new() -> Cache {
        Cache {
            tails: HashMap::new(),
            procs: HashMap::new(),
            procs_at: SystemTime::UNIX_EPOCH,
        }
    }
}

/// How stale the process map may get. A session that starts is noticed
/// within this; one that dies is noticed at once, since the cheap check
/// below stats each known pid every pass.
const PROC_SWEEP_SECS: u64 = 10;

pub fn scan(cfg: &Config, cache: &mut Cache) -> Vec<Session> {
    let now = SystemTime::now();
    let tags = load_tags();
    let procs = {
        let stale = now.duration_since(cache.procs_at)
            .map(|d| d.as_secs() >= PROC_SWEEP_SECS)
            .unwrap_or(true);
        if stale {
            cache.procs = claude_procs();
            cache.procs_at = now;
        } else {
            // Between sweeps, only drop the ones that have gone: one
            // stat per known session, not one read per process alive.
            cache.procs.retain(|_, pid| {
                Path::new(&format!("/proc/{}", pid)).exists()
            });
        }
        cache.procs.clone()
    };
    let mut out = Vec::new();
    let projects = home().join(".claude/projects");
    let dirs = match std::fs::read_dir(&projects) {
        Ok(d) => d,
        Err(_) => return out,
    };
    for d in dirs.flatten() {
        let files = match std::fs::read_dir(d.path()) {
            Ok(f) => f,
            Err(_) => continue,
        };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().map(|e| e != "jsonl").unwrap_or(true) {
                continue;
            }
            let md = match f.metadata() {
                Ok(m) if m.is_file() => m,
                _ => continue,
            };
            let mtime = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let age = now
                .duration_since(mtime)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let id = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            // Beyond the recent window only BOOKMARKED sessions stay,
            // listed as "older": the curated list without the uncurated
            // old strays.
            let recent = age <= cfg.recent_days * 86400;
            if !recent && !tags.contains_key(&id) {
                continue;
            }
            let info = cached_tail(cache, &path, mtime);
            let pid = procs.get(&id).copied();
            let state = if !recent {
                State::Older
            } else if pid.is_none() {
                State::Off
            } else if info.capped {
                State::Capped
            } else if age > cfg.idle_mins * 60 {
                State::Idle
            } else if info.last == 'a' {
                State::Yours
            } else {
                // A session that LOOKS working may in fact wait on the
                // user: sudo below it waits for the fingerprint, and a
                // pending tool call with no child process for a while is
                // a permission dialog. Both are YOURS.
                let kids = pid.map(descendants).unwrap_or_default();
                if kids.iter().any(|c| c == "sudo") {
                    State::Yours
                } else if kids.is_empty() && info.last == 't' && age > 10 {
                    State::Yours
                } else {
                    State::Working
                }
            };
            let tagged = tags.contains_key(&id);
            let tag = tags.get(&id).cloned().unwrap_or_else(|| {
                Path::new(&info.cwd)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "?".into())
            });
            out.push(Session {
                id,
                tag,
                tagged,
                path: path.clone(),
                cwd: info.cwd.clone(),
                state,
                age_secs: age,
                model: info.model.clone(),
                prompt: info.prompt.clone(),
                pid,
                ws: None,
                ctx_k: info.ctx_k,
            });
        }
    }
    out.sort_by_key(|s| (s.state.rank(), s.age_secs));
    out
}

fn cached_tail(cache: &mut Cache, path: &Path, mtime: SystemTime) -> TailInfo {
    if let Some((t, info)) = cache.tails.get(path) {
        if *t == mtime {
            return info.clone();
        }
    }
    let info = read_tail(path).unwrap_or_default();
    cache.tails.insert(path.to_path_buf(), (mtime, info.clone()));
    info
}

fn read_tail(path: &Path) -> Option<TailInfo> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = String::new();
    f.read_to_string(&mut buf).ok()?;
    let mut lines: Vec<&str> = buf.lines().collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0); // the first line is almost surely cut mid-record
    }
    let mut info = TailInfo::default();
    for line in lines.iter().rev() {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let typ = v["type"].as_str().unwrap_or("");
        if info.cwd.is_empty() {
            if let Some(c) = v["cwd"].as_str() {
                info.cwd = c.to_string();
            }
        }
        if typ == "assistant" {
            let msg = &v["message"];
            if info.last == '\0' {
                let tools = msg["content"]
                    .as_array()
                    .map(|a| a.iter().any(|b| b["type"] == "tool_use"))
                    .unwrap_or(false);
                info.last = if tools { 't' } else { 'a' };
                // A client-written refusal ("<synthetic>") ends the
                // transcript when a usage limit blocks the model. The
                // session then waits for /model or for credits, which
                // nothing else in the row would show.
                if msg["model"].as_str().map(|m| m.starts_with('<')).unwrap_or(false) {
                    let txt = msg["content"]
                        .as_array()
                        .and_then(|a| a.iter().find(|b| b["type"] == "text"))
                        .and_then(|b| b["text"].as_str())
                        .unwrap_or("")
                        .to_lowercase();
                    info.capped = txt.contains("usage credits")
                        || txt.contains("usage limit")
                        || txt.contains("limit reached");
                }
            }
            if info.model.is_empty() {
                if let Some(m) = msg["model"].as_str() {
                    // "<synthetic>" is a line the client wrote itself (a
                    // usage-limit refusal, an interrupt notice). No model
                    // served it, so keep looking for the one that did.
                    if !m.starts_with('<') {
                        info.model = short_model(m);
                    }
                }
            }
            if info.ctx_k.is_none() {
                let u = &msg["usage"];
                let total = ["input_tokens", "cache_read_input_tokens",
                             "cache_creation_input_tokens", "output_tokens"]
                    .iter()
                    .filter_map(|k| u[*k].as_u64())
                    .sum::<u64>();
                if total > 0 {
                    info.ctx_k = Some(total / 1000);
                }
            }
        } else if typ == "user" {
            if info.last == '\0' {
                info.last = 'u';
            }
            if info.prompt.is_empty() {
                if let Some(p) = user_text(&v["message"]["content"]) {
                    info.prompt = p;
                }
            }
        }
        if info.last != '\0' && !info.model.is_empty() && !info.prompt.is_empty()
            && !info.cwd.is_empty() && info.ctx_k.is_some()
        {
            break;
        }
    }
    Some(info)
}

/// A real user prompt: string content, or a text block in the array.
/// Tool results are also type "user" but carry no text block.
fn user_text(content: &Value) -> Option<String> {
    let text = if let Some(s) = content.as_str() {
        s.to_string()
    } else {
        content
            .as_array()?
            .iter()
            .find(|b| b["type"] == "text")?["text"]
            .as_str()?
            .to_string()
    };
    let clean = text.replace('\n', " ");
    let clean = clean.trim();
    if clean.starts_with('<') {
        return None; // system-reminder / command wrapper, not the user
    }
    // Hook feedback and interrupts are typed as user entries but are not
    // the user's prompt; skip them so an earlier real prompt surfaces.
    for noise in ["Stop hook feedback:", "[Request interrupted", "Caveat:",
                  "Base directory for this skill:", "# ",
                  "This session is being continued from"] {
        if clean.starts_with(noise) {
            return None;
        }
    }
    Some(clean.chars().take(240).collect())
}

fn short_model(m: &str) -> String {
    let m = m.strip_prefix("claude-").unwrap_or(m);
    m.split('-').next().unwrap_or(m).to_string()
}

/// {session-uuid → pid} for every running `claude --resume <uuid>`.
fn claude_procs() -> HashMap<String, u32> {
    let mut out = HashMap::new();
    let proc_dir = match std::fs::read_dir("/proc") {
        Ok(d) => d,
        Err(_) => return out,
    };
    for e in proc_dir.flatten() {
        let name = e.file_name();
        let pid: u32 = match name.to_string_lossy().parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let comm = std::fs::read_to_string(e.path().join("comm")).unwrap_or_default();
        if comm.trim() != "claude" {
            continue;
        }
        let cmd = std::fs::read(e.path().join("cmdline")).unwrap_or_default();
        let args: Vec<String> = cmd
            .split(|b| *b == 0)
            .map(|s| String::from_utf8_lossy(s).to_string())
            .collect();
        for w in args.windows(2) {
            if w[0] == "--resume" {
                out.insert(w[1].clone(), pid);
            }
        }
    }
    out
}

/// pid → nearest ancestor (self included) present in the window map.
/// claude runs inside bare inside glass; glass owns the X window.
pub fn window_ancestor(pid: u32, winmap: &HashMap<u32, u32>) -> Option<u32> {
    let mut cur = pid;
    for _ in 0..10 {
        if let Some(ws) = winmap.get(&cur) {
            if *ws != u32::MAX {
                return Some(*ws);
            }
        }
        cur = ppid(cur)?;
        if cur <= 1 {
            return None;
        }
    }
    None
}

/// comm names of every descendant process, all threads' children walked.
/// Only called for the handful of live working sessions per tick.
fn descendants(pid: u32) -> Vec<String> {
    let mut out = Vec::new();
    let mut queue = vec![pid];
    let mut steps = 0;
    while let Some(p) = queue.pop() {
        steps += 1;
        if steps > 64 {
            break;
        }
        let tasks = match std::fs::read_dir(format!("/proc/{}/task", p)) {
            Ok(t) => t,
            Err(_) => continue,
        };
        for t in tasks.flatten() {
            let children = t.path().join("children");
            let s = std::fs::read_to_string(children).unwrap_or_default();
            for c in s.split_whitespace() {
                if let Ok(cp) = c.parse::<u32>() {
                    let comm =
                        std::fs::read_to_string(format!("/proc/{}/comm", cp))
                            .map(|s| s.trim().to_string())
                            .unwrap_or_default();
                    out.push(comm);
                    queue.push(cp);
                }
            }
        }
    }
    out
}

fn ppid(pid: u32) -> Option<u32> {
    let status = std::fs::read_to_string(format!("/proc/{}/status", pid)).ok()?;
    for line in status.lines() {
        if let Some(v) = line.strip_prefix("PPid:") {
            return v.trim().parse().ok();
        }
    }
    None
}

pub fn load_tags() -> HashMap<String, String> {
    let mut out = HashMap::new();
    let path = home().join(".cc-sessions/bookmarks.json");
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return out,
    };
    let v: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return out,
    };
    if let Some(map) = v["sessions"].as_object() {
        for (id, entry) in map {
            if let Some(tag) = entry["tags"].as_array().and_then(|t| t.first()) {
                if let Some(s) = tag.as_str() {
                    out.insert(id.clone(), s.to_string());
                }
            }
        }
    }
    out
}

//! Today's token rollup, per session. On-demand only (the `c` key or
//! `--today`): it reads every transcript touched today in full, which
//! is too much work for the 2 s tick.

use crate::config::home;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Row {
    pub tag: String,
    pub out_tokens: u64,
    pub in_tokens: u64, // fresh input only, cache reads excluded
    pub turns: u64,
}

pub fn today(tags: &HashMap<String, String>) -> Vec<Row> {
    let midnight = local_midnight_epoch();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut per: HashMap<String, Row> = HashMap::new();
    let projects = home().join(".claude/projects");
    let dirs = match std::fs::read_dir(&projects) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
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
            let mtime = md
                .modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if mtime < midnight {
                continue; // untouched today: cannot hold a today entry
            }
            let id = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let tag = tags.get(&id).cloned().unwrap_or(id);
            let row = per.entry(tag.clone()).or_insert(Row {
                tag,
                out_tokens: 0,
                in_tokens: 0,
                turns: 0,
            });
            let file = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(_) => continue,
            };
            for line in BufReader::new(file).lines().flatten() {
                if !line.contains("\"usage\"") {
                    continue;
                }
                let v: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if v["type"] != "assistant" {
                    continue;
                }
                let ts = v["timestamp"].as_str().and_then(iso_to_epoch).unwrap_or(0);
                if ts < midnight || ts > now + 3600 {
                    continue;
                }
                let u = &v["message"]["usage"];
                row.out_tokens += u["output_tokens"].as_u64().unwrap_or(0);
                row.in_tokens += u["input_tokens"].as_u64().unwrap_or(0);
                row.turns += 1;
            }
        }
    }
    let mut out: Vec<Row> = per.into_values().filter(|r| r.turns > 0).collect();
    out.sort_by(|a, b| b.out_tokens.cmp(&a.out_tokens));
    out
}

/// Epoch of local midnight. One `date` fork per invocation, which is
/// on-demand only; parsing timezone rules ourselves is not worth it.
fn local_midnight_epoch() -> u64 {
    std::process::Command::new("date")
        .args(["-d", "today 00:00", "+%s"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// "2026-08-17T15:04:05.123Z" -> epoch seconds. UTC times only.
pub fn iso_to_epoch(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<u64>().ok();
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hh, mm, ss) = (num(11..13)?, num(14..16)?, num(17..19)?);
    // days from civil (Howard Hinnant), epoch 1970-01-01
    let y = y as i64 - if m <= 2 { 1 } else { 0 };
    let era = y.div_euclid(400);
    let yoe = (y - era * 400) as u64;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe as i64 - 719468;
    Some((days as u64) * 86400 + hh * 3600 + mm * 60 + ss)
}

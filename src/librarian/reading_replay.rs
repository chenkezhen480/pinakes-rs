//! Replays the reads a real run recorded (its counting proxy's `llm_bodies.jsonl`) through
//! today's checks: what each reply comes to now beside what it came to then. A change to the
//! checks shows on the model's own replies, the same every time, without a run (2026-10-22: one
//! run's answers move as much as a change does). Set `PINAKES_TEST_REPLAY` to run folders separated
//! by `;` (`compare/results/<run>`) and run with `--nocapture`; skipped without it.
//!
//! "Then" is read off the run itself: a reply was used when the document's next read starts
//! after its window, not used when the next read starts at the same line.

use std::collections::HashMap;

use serde_json::Value;

use super::*;
use crate::librarian::edit::PageAudience;

struct Recorded {
    doc: String,
    title: String,
    thread: String,
    folder: String,
    cursor: usize,
    end: usize,
    limit: usize,
    lines: Vec<String>,
    reply: Value,
}

fn between<'a>(s: &'a str, a: &str, b: &str) -> Option<&'a str> {
    let i = s.find(a)? + a.len();
    let j = s[i..].find(b)? + i;
    Some(&s[i..j])
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(|b| b["text"].as_str()).collect(),
        _ => String::new(),
    }
}

fn json_of(v: &Value) -> Option<Value> {
    match v {
        Value::String(s) => serde_json::from_str(s).ok(),
        v => Some(v.clone()),
    }
}

/// One recorded read: the window as the message showed it, and the reply's tool input.
fn parse(line: &str) -> Option<Recorded> {
    let r: Value = serde_json::from_str(line).ok()?;
    let req = json_of(&r["request"])?;
    let system = text_of(&req["system"]);
    if !system
        .chars()
        .take(300)
        .collect::<String>()
        .contains("reading a document")
    {
        return None;
    }
    let msg = text_of(&req["messages"][0]["content"]);
    let reply = json_of(&r["reply"])?;
    let input = reply["content"]
        .as_array()?
        .iter()
        .find(|b| b["type"] == "tool_use")?["input"]
        .clone();
    let head = between(&msg, "\n\nLines ", ".\n\n```text\n")?;
    let (range, after) = head.split_once("; ")?;
    let nums: Vec<usize> = range
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|s| s.parse().ok())
        .collect();
    let (cursor, end, total) = (*nums.first()?, *nums.get(1)?, *nums.get(2)?);
    let mut lines = vec![String::new(); total];
    for l in between(&msg, "```text\n", "\n```")?.lines() {
        if let Some((n, t)) = l.split_once('|')
            && let Ok(n) = n.trim().parse::<usize>()
            && (1..=total).contains(&n)
        {
            lines[n - 1] = t.strip_prefix(' ').unwrap_or(t).to_owned();
        }
    }
    Some(Recorded {
        doc: between(&msg, "Id: ", ",")?.to_owned(),
        title: between(&msg, "Title: ", "\n")?.to_owned(),
        thread: between(&msg, "## Main thread\n\n", "\n\n## Read so far")?.to_owned(),
        folder: between(&msg, "New pages of this document are in `", "/`")?.to_owned(),
        cursor,
        end,
        limit: if after.contains("goes on after") {
            total
        } else {
            end
        },
        lines,
        reply: input,
    })
}

/// What a reply comes to under today's checks.
fn now(rec: &Recorded) -> String {
    let path = PagePath::parse(&format!("{}/x", rec.folder)).expect("a page folder");
    let target = Target::new(path.kind().expect("a kind"), PageAudience::of(&path)).unwrap();
    let Ok(reply) = serde_json::from_value::<ReadReply>(rec.reply.clone()) else {
        return "unreadable".into();
    };
    let asked = reply.steps.len();
    let lines: Vec<&str> = rec.lines.iter().map(String::as_str).collect();
    let context = number_sources(&format!("{}\n{}", rec.title, rec.thread), None, 1, None);
    match check_read(
        &target, &lines, &context, rec.cursor, rec.end, rec.limit, reply,
    ) {
        Ok(None) => "more".into(),
        Ok(Some(read)) if read.steps.len() == asked => "taken whole".into(),
        Ok(Some(_)) => "good steps kept".into(),
        Err(why) => {
            // The reason without its step number and lines: what kind of refusal it is.
            let why = why.split_once(": ").map_or(why.as_str(), |(_, w)| w);
            format!("refused: {}", why.chars().take(48).collect::<String>())
        }
    }
}

#[test]
fn recorded_reads_replay() {
    let Ok(dirs) = std::env::var("PINAKES_TEST_REPLAY") else {
        return;
    };
    for dir in dirs.split(';').filter(|d| !d.is_empty()) {
        let text =
            std::fs::read_to_string(format!("{dir}/llm_bodies.jsonl")).expect("a run folder");
        let reads: Vec<Recorded> = text.lines().filter_map(parse).collect();
        assert!(!reads.is_empty(), "{dir}: no reads recorded");
        let mut table: HashMap<(String, String), usize> = HashMap::new();
        let mut changed = Vec::new();
        for (i, rec) in reads.iter().enumerate() {
            let then = match reads[i + 1..].iter().find(|r| r.doc == rec.doc) {
                Some(next) if next.cursor > rec.cursor => "used",
                Some(_) => "not used",
                None => "last",
            };
            let now = now(rec);
            if then != "not used" && now.starts_with("refused") {
                changed.push(format!(
                    "{} lines {}-{}: {now}",
                    rec.title, rec.cursor, rec.end
                ));
            }
            *table.entry((then.to_owned(), now)).or_default() += 1;
        }
        let mut rows: Vec<_> = table.into_iter().collect();
        rows.sort();
        println!("\n== {dir}: {} reads", reads.len());
        for ((then, now), n) in rows {
            println!("  then {then:9} now {now:60} {n}");
        }
        println!("  used then, refused now (a new check's catch, or its false refusal):");
        for c in &changed {
            println!("    {c}");
        }
    }
}

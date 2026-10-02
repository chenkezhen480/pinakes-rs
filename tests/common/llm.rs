//! A scripted LLM for the librarian: each test says what the model replies at each step, and
//! can look at every prompt it was sent. Like the real APIs' structured outputs, every reply is
//! checked against the prompt's JSON schema; a reply that doesn't fit fails the call.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pinakes::librarian::prompts;
use pinakes::llm::{Llm, LlmClient, Prompt};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Plan,
    Verify,
    Describe,
    VerifyDescribe,
    /// Recognizing which earlier document an upload is.
    Match,
    /// Reading a document from the cursor to the end of one concept.
    Read,
}

type Reply = dyn Fn(Step, &str, usize) -> String + Send + Sync;

/// A reply that makes the call fail, as an unreachable model does.
pub const UNREACHABLE: &str = "!unreachable";

/// Answers each prompt with `reply(step, user message, how many calls of this step came
/// before)`.
pub struct FakeLlm {
    reply: Box<Reply>,
    calls: Mutex<Vec<(Step, String)>>,
}

impl FakeLlm {
    pub fn new(reply: impl Fn(Step, &str, usize) -> String + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            reply: Box::new(reply),
            calls: Mutex::new(Vec::new()),
        })
    }

    pub fn llm(self: &Arc<Self>) -> Llm {
        self.clone()
    }

    /// The user messages of every call of one step, in order.
    pub fn calls(&self, step: Step) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(s, _)| *s == step)
            .map(|(_, u)| u.clone())
            .collect()
    }

    pub fn clear(&self) {
        self.calls.lock().unwrap().clear();
    }
}

#[async_trait]
impl LlmClient for FakeLlm {
    async fn complete(&self, prompt: &Prompt) -> anyhow::Result<String> {
        let step = match prompt.system.as_str() {
            s if s == prompts::INGEST => Step::Plan,
            s if s == prompts::VERIFY => Step::Verify,
            s if s == prompts::DESCRIBE => Step::Describe,
            s if s == prompts::VERIFY_DESCRIBE => Step::VerifyDescribe,
            s if s == prompts::MATCH => Step::Match,
            s if s == prompts::READ => Step::Read,
            _ => anyhow::bail!("unknown prompt"),
        };
        let n = {
            let mut calls = self.calls.lock().unwrap();
            let n = calls.iter().filter(|(s, _)| *s == step).count();
            calls.push((step, prompt.user.clone()));
            n
        };
        let reply = (self.reply)(step, &prompt.user, n);
        if reply == UNREACHABLE {
            anyhow::bail!("connection refused");
        }
        let value: Value = serde_json::from_str(&reply)
            .map_err(|e| anyhow::anyhow!("the scripted reply is not JSON: {e}: {reply}"))?;
        if let Err(e) = fits(&prompt.schema, &value, "$") {
            panic!("the scripted {step:?} reply doesn't fit the prompt's schema: {e}\n{reply}");
        }
        Ok(reply)
    }
}

/// Whether a value fits the subset of JSON Schema structured outputs use.
pub fn fits(schema: &Value, v: &Value, at: &str) -> Result<(), String> {
    if let Some(any) = schema["anyOf"].as_array() {
        return if any.iter().any(|s| fits(s, v, at).is_ok()) {
            Ok(())
        } else {
            Err(format!("{at}: fits none of anyOf"))
        };
    }
    if let Some(options) = schema["enum"].as_array()
        && !options.contains(v)
    {
        return Err(format!("{at}: {v} is not one of {options:?}"));
    }
    match schema["type"].as_str() {
        Some("object") => {
            let o = v.as_object().ok_or(format!("{at}: not an object"))?;
            let props = schema["properties"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            for k in schema["required"].as_array().into_iter().flatten() {
                let k = k.as_str().unwrap();
                if !o.contains_key(k) {
                    return Err(format!("{at}: {k} is missing"));
                }
            }
            for (k, val) in o {
                let sub = props.get(k).ok_or(format!("{at}: {k} is not allowed"))?;
                fits(sub, val, &format!("{at}.{k}"))?;
            }
            Ok(())
        }
        Some("array") => {
            let a = v.as_array().ok_or(format!("{at}: not an array"))?;
            for (i, item) in a.iter().enumerate() {
                fits(&schema["items"], item, &format!("{at}[{i}]"))?;
            }
            Ok(())
        }
        Some("string") if v.is_string() => Ok(()),
        Some("integer") if v.is_u64() || v.is_i64() => Ok(()),
        Some("boolean") if v.is_boolean() => Ok(()),
        Some("null") if v.is_null() => Ok(()),
        Some(t) => Err(format!("{at}: {v} is not a {t}")),
        None => Ok(()),
    }
}

/// The edit numbers a verify message asks about.
pub fn asked_edits(msg: &str) -> Vec<usize> {
    let tail = msg
        .rsplit("numbered as shown (")
        .next()
        .and_then(|t| t.split(')').next())
        .unwrap_or("");
    tail.split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect()
}

/// Verdicts accepting every edit asked about.
pub fn all_ok(msg: &str) -> String {
    let v: Vec<_> = asked_edits(msg)
        .into_iter()
        .map(|i| json!({ "edit": i, "ok": true, "reason": null }))
        .collect();
    json!({ "verdicts": v }).to_string()
}

/// Verdicts rejecting every edit asked about, with one reason.
pub fn all_rejected(msg: &str, reason: &str) -> String {
    let v: Vec<_> = asked_edits(msg)
        .into_iter()
        .map(|i| json!({ "edit": i, "ok": false, "reason": reason }))
        .collect();
    json!({ "verdicts": v }).to_string()
}

pub const OK: &str = r#"{"ok": true, "reason": null}"#;
pub const NO_EDITS: &str = r#"{"edits": []}"#;

/// A `create` edit with every field.
pub fn create(page: &str, title: &str, summary: &str, about: &[&str], body: &str) -> Value {
    json!({ "op": "create", "page": page, "title": title, "summary": summary,
            "aliases": [], "keywords": [], "about": about, "body": body })
}

/// An `update` edit with every field; set more with indexing.
pub fn update(page: &str, heading: Option<&str>, text: Option<&str>) -> Value {
    json!({ "op": "update", "page": page, "heading": heading, "text": text, "summary": null,
            "add_aliases": [], "add_keywords": [], "add_about": [] })
}

/// The page ids listed under "Related pages" in a plan message (page bodies, in code blocks,
/// may have headings of their own).
pub fn related_pages(msg: &str) -> Vec<String> {
    let section = msg
        .split("## Related pages")
        .nth(1)
        .and_then(|s| s.split("\n## Allowed anchors").next())
        .unwrap_or("");
    let mut fenced = false;
    let mut out = Vec::new();
    for line in section.lines() {
        if line.starts_with("```") {
            fenced = !fenced;
        } else if !fenced && let Some(p) = line.strip_prefix("### ") {
            out.push(p.to_owned());
        }
    }
    out
}

/// The record a plan or verify message shows (`## New content`).
pub fn record_of(msg: &str) -> Value {
    let block = msg
        .split("## New content\n\n```json\n")
        .nth(1)
        .and_then(|s| s.split("\n```").next())
        .expect("a record");
    serde_json::from_str(block).unwrap()
}

/// The folder a plan message says new pages go in.
pub fn folder(msg: &str) -> String {
    let after = msg
        .split("Pages of this job are in `")
        .nth(1)
        .or_else(|| msg.split("The only page this job may write is `").nth(1))
        .unwrap_or("");
    after
        .split('`')
        .next()
        .unwrap_or("")
        .trim_end_matches('/')
        .to_owned()
}

/// The schema definition a describe or verify-describe message shows.
pub fn definition(msg: &str) -> Value {
    let block = msg
        .split("## Schema definition\n\n```json\n")
        .nth(1)
        .and_then(|s| s.split("\n```").next())
        .expect("a definition");
    serde_json::from_str(block).unwrap()
}

/// A description written from the definition alone, as a model that adds nothing would:
/// the host's texts where it gave them, the type's name otherwise, and a body listing the
/// types.
pub fn describe_from_definition(msg: &str) -> String {
    let def = definition(msg);
    let schema = def["schema"].as_str().unwrap();
    let title = def["title"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{} graph", schema.replace('_', " ")));
    let text = |name: &str, t: &Value| {
        json!({
            "en": t["en"].as_str().unwrap_or(name),
            "zh": t["zh"].as_str().unwrap_or(name),
            "aliases": t["aliases"].as_array().cloned().unwrap_or_default(),
        })
    };
    let mut nodes = serde_json::Map::new();
    let mut body = format!("# {title}\n\n## Node types\n\n");
    for (name, n) in def["nodes"].as_object().unwrap() {
        nodes.insert(name.clone(), text(name, n));
        body.push_str(&format!(
            "- `{name}` (key `{}`)",
            n["key"].as_str().unwrap()
        ));
        let meaning: Vec<&str> = [n["en"].as_str(), n["zh"].as_str()]
            .into_iter()
            .flatten()
            .collect();
        if !meaning.is_empty() {
            body.push_str(&format!(": {}", meaning.join(" / ")));
        }
        body.push('\n');
    }
    let mut relations = serde_json::Map::new();
    if let Some(rels) = def["relations"].as_object().filter(|r| !r.is_empty()) {
        body.push_str("\n## Relation types\n\n");
        for (name, r) in rels {
            relations.insert(name.clone(), text(name, r));
            body.push_str(&format!(
                "- `{name}`: `{}` → `{}`",
                r["from"].as_str().unwrap(),
                r["to"].as_str().unwrap()
            ));
            if let Some(en) = r["en"].as_str() {
                body.push_str(&format!(", {en}"));
            }
            body.push('\n');
        }
    }
    json!({
        "title": title,
        "summary": def["summary"].as_str().map(str::to_owned)
            .unwrap_or_else(|| format!("The {schema} schema.")),
        "aliases": def["aliases"].as_array().cloned().unwrap_or_default(),
        "nodes": nodes,
        "relations": relations,
        "body": body,
    })
    .to_string()
}

/// A model that only describes schemas (from the definition) and plans nothing: what tests
/// that don't look at the librarian run with.
pub fn describer() -> Arc<FakeLlm> {
    FakeLlm::new(|step, msg, _| match step {
        Step::Plan => NO_EDITS.into(),
        Step::Verify => all_ok(msg),
        Step::Describe => describe_from_definition(msg),
        Step::VerifyDescribe => OK.into(),
        Step::Match => NOT_THE_SAME.into(),
        Step::Read => skip_to_end(msg),
    })
}

/// The numbered lines a read message shows, and whether they reach the end of the document.
pub fn shown(msg: &str) -> (Vec<(usize, String)>, bool) {
    let block = msg
        .split("## Text from line")
        .nth(1)
        .and_then(|s| s.split("```text\n").nth(1))
        .and_then(|s| s.split("```").next())
        .expect("the text shown");
    let lines = block
        .lines()
        .map(|l| {
            let (n, text) = l.split_once("| ").unwrap_or((l.trim_end_matches('|'), ""));
            (n.parse().unwrap(), text.to_owned())
        })
        .collect();
    (lines, msg.contains("this is the end of the document"))
}

/// The line a read message starts at.
pub fn cursor_of(msg: &str) -> usize {
    shown(msg).0[0].0
}

/// A read message's value after `label` (such as `Title: `), to the end of that line.
pub fn line_after<'a>(msg: &'a str, label: &str) -> &'a str {
    msg.split(label)
        .nth(1)
        .and_then(|s| s.lines().next())
        .unwrap_or("")
}

/// The folder a read message says new pages go in.
pub fn read_folder(msg: &str) -> String {
    msg.split("New pages of this document are in `")
        .nth(1)
        .and_then(|s| s.split("/`").next())
        .unwrap()
        .to_owned()
}

/// A read reply.
pub fn read_reply(
    action: &str,
    end: usize,
    title: Option<&str>,
    page: Option<&str>,
    thread: Option<&str>,
) -> String {
    json!({ "action": action, "end_line": end, "title": title, "page": page,
            "keywords": title.into_iter().collect::<Vec<_>>(), "thread": thread })
    .to_string()
}

/// A reader that skips everything it is shown, as a model that finds nothing to file would.
pub fn skip_to_end(msg: &str) -> String {
    let (lines, end) = shown(msg);
    if end {
        read_reply("skip", lines.last().unwrap().0, None, None, None)
    } else {
        read_reply("more", 0, None, None, None)
    }
}

/// A reader that takes each concept to start at a line `starts` says, and to run to the line
/// before the next one: one concept per read, `more` when the next start isn't shown yet.
/// Lines before the first start are skipped. `name` gives a concept's title and page name from
/// its first line.
pub fn reader(
    msg: &str,
    starts: impl Fn(&str) -> bool,
    name: impl Fn(&str) -> (String, String),
    thread: Option<&str>,
) -> String {
    let (lines, at_end) = shown(msg);
    let first = &lines[0];
    let next = lines[1..]
        .iter()
        .position(|(_, l)| starts(l))
        .map(|i| lines[i].0);
    if !starts(&first.1) {
        let Some(before) = lines.iter().position(|(_, l)| starts(l)) else {
            return skip_to_end(msg);
        };
        return read_reply("skip", lines[before - 1].0, None, None, thread);
    }
    let end = match next {
        Some(end) => end,
        None if at_end => lines.last().unwrap().0,
        None => return read_reply("more", 0, None, None, None),
    };
    let (title, page) = name(&first.1);
    let page = format!("{}/{page}", read_folder(msg));
    read_reply("concept", end, Some(&title), Some(&page), thread)
}

/// The concept a plan message's "This call" is about.
pub fn focus_title(msg: &str) -> Option<String> {
    let after = msg.split("## This call").nth(1)?;
    let quoted = after.split('"').nth(1)?;
    Some(quoted.to_owned())
}

/// A match answer: none of the candidates.
pub const NOT_THE_SAME: &str = r#"{"document": null, "reason": "a different document"}"#;

/// A match answer naming one candidate.
pub fn same_as(id: &str) -> String {
    serde_json::json!({ "document": id, "reason": "the same document, edited" }).to_string()
}

/// The candidate document ids a match message shows, in order.
pub fn candidates(msg: &str) -> Vec<String> {
    msg.lines()
        .filter_map(|l| l.strip_prefix("### Document "))
        .filter_map(|l| l.split(':').next())
        .map(str::to_owned)
        .collect()
}

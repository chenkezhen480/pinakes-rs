//! Reading a document like a person, one concept at a time (design, "Reading like a person,
//! one concept at a time" and "The reading is a loop code runs, not an agent").
//!
//! Code numbers the lines and runs the loop; the model decides only what each concept is, where
//! it ends and how its page reads. Each turn: read from the cursor (one structured call), check
//! the answer (the end line lies within what was shown, at or after the cursor), file the
//! concept (plan, code checks, verify, write), save where the reading is, and move the cursor
//! past the concept. Every line is part of a concept or skipped, so none is left unread; the
//! cursor only moves forward and each read is asked again once at most, so the loop ends.
//!
//! A new version keeps, without asking the model, every earlier step whose lines didn't change,
//! and reads again from each step the changes touch until it is back in step with the earlier
//! reading.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::Arc;

use anyhow::Context;
use serde::Deserialize;
use serde_json::json;

use super::documents::{self, Changes, LedgerStep, Reading, ReadingState};
use super::{
    CHANGES_CHARS, DocumentIn, DocumentMatch, Ingest, Librarian, MAX_QUERY_CHARS, Unit,
    apply_and_record, hold, nullable_string, object, page_id, parse_json, prompts, record_log,
    sources, strings,
};
use crate::identity::Scope;
use crate::jobs::JobResult;
use crate::librarian::edit::Target;
use crate::tenants::Tenant;
use crate::wiki::PagePath;

/// The smallest read window, in tokens, whatever the config says.
const MIN_WINDOW_TOKENS: usize = 500;
/// Most queries the related-page search is given for one concept.
const CONCEPT_QUERIES: usize = 3;
/// How many times the read window may grow, at most, for one long concept: by default up to
/// about 120,000 tokens, within today's models' context.
const WINDOW_GROWTH: usize = 6;
/// Most ledger lines one call is shown (the latest), so a long document's calls don't grow
/// with every step; the related pages show what came before.
const MAX_LEDGER_LINES: usize = 60;
/// Earlier steps shown on each side of the ones a read's lines were in.
const EARLIER_STEPS_AROUND: usize = 2;

/// The reply to one read.
#[derive(Deserialize)]
struct ReadReply {
    action: String,
    end_line: i64,
    title: Option<String>,
    page: Option<String>,
    #[serde(default)]
    keywords: Vec<String>,
    thread: Option<String>,
}

/// One read, checked: the last line it takes, and the concept, or none when skipped.
struct Read {
    end: usize,
    concept: Option<Concept>,
    thread: Option<String>,
}

struct Concept {
    title: String,
    page: String,
    keywords: Vec<String>,
}

/// The shape of a read's reply.
pub fn read_schema() -> serde_json::Value {
    object(&[
        (
            "action",
            json!({ "type": "string", "enum": ["concept", "skip", "more"] }),
        ),
        ("end_line", json!({ "type": "integer" })),
        ("title", nullable_string()),
        ("page", nullable_string()),
        ("keywords", strings()),
        ("thread", nullable_string()),
    ])
}

/// The version before the one being read, and how it differs.
struct Earlier {
    /// Its number.
    version: u32,
    changes: Changes,
    reading: Option<Reading>,
    /// The earlier steps the changes touch, by index.
    affected: HashSet<usize>,
}

impl Earlier {
    fn new(version: u32, old: &str, new: &str, reading: Option<Reading>) -> Self {
        let changes = Changes::new(old, new);
        let mut affected = HashSet::new();
        if let Some(r) = &reading {
            for line in changes.all_old_lines_touched() {
                if let Some(i) = step_of(r, line) {
                    affected.insert(i);
                }
            }
        }
        Self {
            version,
            changes,
            reading,
            affected,
        }
    }

    /// What changed in new lines `from..=to`, as a prompt section's text.
    fn changes_text(&self, from: usize, to: usize) -> Option<String> {
        self.changes.within(from, to, CHANGES_CHARS).map(|c| {
            format!(
                "`-` removed, `+` added:

```diff
{c}```"
            )
        })
    }

    /// The earlier step that can be kept as it is at new line `cursor`: one that starts there,
    /// that no change touches, and whose lines are all unchanged.
    fn kept_at(&self, cursor: usize) -> Option<LedgerStep> {
        let reading = self.reading.as_ref()?;
        let old = self.changes.old_line(cursor)?;
        let (i, s) = reading
            .steps
            .iter()
            .enumerate()
            .find(|(_, s)| s.from == old)?;
        let len = s.to - s.from;
        // A concept that got no page (held or refused) is read and filed again.
        let filed = s.concept.is_none() || s.page.is_some();
        let unchanged = filed
            && !self.affected.contains(&i)
            && (0..=len).all(|k| self.changes.old_line(cursor + k) == Some(old + k));
        unchanged.then(|| LedgerStep {
            from: cursor,
            to: cursor + len,
            concept: s.concept.clone(),
            page: s.page.clone(),
            cites: s.cites.clone(),
        })
    }

    /// The earlier steps (by index, in order) that held new lines `from..=to` or the lines
    /// they replace.
    fn steps_for(&self, from: usize, to: usize) -> Vec<usize> {
        let Some(reading) = &self.reading else {
            return Vec::new();
        };
        let mut old: Vec<usize> = (from..=to)
            .filter_map(|l| self.changes.old_line(l))
            .collect();
        old.extend(self.changes.old_lines_touched(from, to));
        let mut steps: Vec<usize> = old.iter().filter_map(|&l| step_of(reading, l)).collect();
        steps.sort_unstable();
        steps.dedup();
        steps
    }

    /// Those steps, with a few on each side, as ledger lines.
    fn ledger_near(&self, from: usize, to: usize) -> Option<Vec<String>> {
        let reading = self.reading.as_ref()?;
        let steps = self.steps_for(from, to);
        let first = steps.first()?.saturating_sub(EARLIER_STEPS_AROUND);
        let last = (steps.last()? + EARLIER_STEPS_AROUND).min(reading.steps.len() - 1);
        let part = Reading {
            thread: String::new(),
            steps: reading.steps[first..=last].to_vec(),
        };
        Some(part.ledger_lines(usize::MAX))
    }

    /// The pages of those steps.
    fn pages_for(&self, from: usize, to: usize) -> Vec<String> {
        let Some(reading) = &self.reading else {
            return Vec::new();
        };
        self.steps_for(from, to)
            .into_iter()
            .filter_map(|i| reading.steps[i].page.clone())
            .collect()
    }
}

/// The step of a reading that holds a line.
fn step_of(reading: &Reading, line: usize) -> Option<usize> {
    reading
        .steps
        .iter()
        .position(|s| (s.from..=s.to).contains(&line))
}

/// About how many tokens a line costs: a CJK character about one, other text about a quarter
/// of a token per character, plus the line number.
fn tokens(line: &str) -> usize {
    let tenths: usize = line
        .chars()
        .map(|c| if c as u32 >= 0x2E80 { 10 } else { 3 })
        .sum();
    tenths / 10 + 3
}

/// The last line (from 1) of a window of about `budget` tokens from `cursor`; at least one line.
fn window_end(lines: &[&str], cursor: usize, budget: usize) -> usize {
    let mut used = 0;
    let mut end = cursor;
    for (i, line) in lines[cursor - 1..].iter().enumerate() {
        used += tokens(line);
        if used > budget && i > 0 {
            break;
        }
        end = cursor + i;
    }
    end
}

/// The page a concept is filed as, if it is one this document may write.
fn concept_page(target: &Target, page: Option<&str>) -> Result<String, String> {
    let page = page
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .ok_or("a concept needs the page it is filed as")?;
    let path = PagePath::parse(page).map_err(|_| format!("{page:?} is not a valid page id"))?;
    if !target.allows(&path) {
        return Err(format!(
            "{page} is not in {}/, where this document's pages go",
            target.folder()
        ));
    }
    Ok(path.to_string())
}

/// What a new version's calls are told about the versions before it.
fn version_note(doc: &documents::Document, version: u32) -> String {
    format!(
        "This is version {version} of document {} ({}). The pages its earlier versions were \
         filed into cite them and come first under Related pages. Update them for what \
         changed: change what changed, mark what this version replaces or removes as \
         superseded, and create a page only for something no page covers. An earlier version \
         is not another source: where this version says something else, it replaces the \
         earlier text, so supersede it; never flag it as a contradiction. To change a claim, \
         use one `supersede` edit with the claim copied exactly from the page and the new \
         wording as its replacement, and don't also rewrite the section it is in.",
        doc.id, doc.title
    )
}

impl Librarian {
    /// Files a document (expertise) by reading it, one concept per step; each concept goes
    /// through steps 2 to 6 like a record. Resumes an unfinished reading of the same text.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn read_document(
        &self,
        tenant: &Arc<Tenant>,
        job: &Ingest,
        d: &DocumentIn,
        reader: &Scope,
        rules: &str,
        today: &str,
        result: &mut JobResult,
    ) -> anyhow::Result<()> {
        let target = &job.target;
        let hash = documents::content_hash(&d.content);
        let unfinished = tenant
            .store
            .reading_state(target.audience.key(), &hash)
            .await?
            .filter(|s| d.given.as_ref().is_none_or(|g| g.id == s.document.id));
        let mut state = match unfinished {
            Some(mut s) => {
                s.document.title.clone_from(&d.title);
                tracing::info!(document = %s.document.id, cursor = s.cursor, "resuming a reading");
                s
            }
            None => {
                let (found, how) = self.identify(tenant, target, reader, d).await?;
                ReadingState {
                    source: job.source.clone(),
                    content_hash: hash,
                    document: found,
                    how,
                    version: 0,
                    cursor: 1,
                    reading: Reading::default(),
                }
            }
        };
        let earlier_sources = match state.how {
            DocumentMatch::New => Vec::new(),
            _ => tenant.store.document_sources(&state.document.id).await?,
        };
        state.version = u32::try_from(earlier_sources.len()).unwrap_or(u32::MAX) + 1;
        tracing::info!(document = %state.document.id, how = ?state.how, version = state.version, "document identified");
        result.source = Some(format!("sources/{}", state.source));
        result.document = Some(state.document.id.clone());
        result.document_match = Some(state.how);
        result.version = Some(state.version);

        let earlier = self.earlier(tenant, &earlier_sources, &d.content).await?;
        if let Some(e) = &earlier {
            if e.changes.is_empty() {
                // The same text again: nothing to read, no model call; it is kept as a version.
                let reading = e.reading.clone();
                let _writer = tenant.wiki_writer.lock().await;
                record_log(
                    tenant,
                    &format!(
                        "ingest {}: nothing changed since the last version",
                        target.kind.as_str()
                    ),
                )
                .await?;
                let version = tenant
                    .store
                    .add_document_version(&state.document, &state.source, reading.as_ref())
                    .await?;
                result.version = Some(version);
                return Ok(());
            }
            if state.reading.steps.is_empty() && state.reading.thread.is_empty() {
                state.reading.thread = e
                    .reading
                    .as_ref()
                    .map(|r| r.thread.clone())
                    .unwrap_or_default();
            }
        }

        let lines: Vec<&str> = d.content.lines().collect();
        let mut first_sources = earlier_sources.clone();
        first_sources.push(state.source.clone());
        while state.cursor <= lines.len() {
            if let Some(kept) = earlier.as_ref().and_then(|e| e.kept_at(state.cursor)) {
                state.cursor = kept.to + 1;
                state.reading.steps.push(kept);
                continue;
            }
            let read = self
                .read_step(target, &state, &lines, earlier.as_ref())
                .await?;
            if let Some(t) = read
                .thread
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty())
            {
                state.reading.thread = t;
            }
            let (from, to) = (state.cursor, read.end);
            let step = match read.concept {
                None => LedgerStep {
                    from,
                    to,
                    concept: None,
                    page: None,
                    cites: None,
                },
                Some(c) => {
                    let cite = documents::cite(&state.source, from, to);
                    let filed = self
                        .file_concept(
                            tenant,
                            job,
                            reader,
                            rules,
                            today,
                            &state,
                            &lines,
                            (from, to),
                            &c,
                            &cite,
                            earlier.as_ref(),
                            &first_sources,
                            result,
                        )
                        .await?;
                    // A page whose edits were all held or refused isn't in the wiki: the
                    // ledger doesn't offer it to later steps to link to or continue.
                    LedgerStep {
                        from,
                        to,
                        concept: Some(c.title),
                        page: filed.then_some(c.page),
                        cites: Some(cite),
                    }
                }
            };
            tracing::info!(
                from,
                to,
                concept = step.concept.as_deref().unwrap_or("(skipped)"),
                page = step.page.as_deref().unwrap_or(""),
                "read a step"
            );
            state.reading.steps.push(step);
            state.cursor = to + 1;
            // Saved after every step, so a job that stops here resumes from the next line.
            tenant.store.put_reading_state(&state).await?;
        }

        // The version is recorded once it is filed (and its unfinished reading dropped), so a
        // failed job leaves the previous version as the one the next upload is compared with.
        let version = tenant
            .store
            .add_document_version(&state.document, &state.source, Some(&state.reading))
            .await?;
        result.version = Some(version);
        Ok(())
    }

    /// The last version filed before this one, compared with the new text.
    async fn earlier(
        &self,
        tenant: &Arc<Tenant>,
        sources_before: &[String],
        content: &str,
    ) -> anyhow::Result<Option<Earlier>> {
        let Some(last) = sources_before.last() else {
            return Ok(None);
        };
        let (dir, name) = (tenant.dir.clone(), last.clone());
        let old = tokio::task::spawn_blocking(move || sources::read(&dir, &name))
            .await??
            .and_then(|text| documents::content_of(&text))
            .unwrap_or_default();
        let reading = tenant.store.version_reading(last).await?;
        let version = u32::try_from(sources_before.len()).unwrap_or(u32::MAX);
        Ok(Some(Earlier::new(version, &old, content, reading)))
    }

    /// One read from the cursor: asked until the answer can be used, with a wider window when
    /// the concept runs past it, and once more with the reason when it can't be used.
    async fn read_step(
        &self,
        target: &Target,
        state: &ReadingState,
        lines: &[&str],
        earlier: Option<&Earlier>,
    ) -> anyhow::Result<Read> {
        let cursor = state.cursor;
        let mut budget = self.cfg.read_window_tokens.max(MIN_WINDOW_TOKENS);
        let most = budget.saturating_mul(WINDOW_GROWTH);
        let mut feedback: Option<String> = None;
        let mut rejected = 0;
        loop {
            let end = window_end(lines, cursor, budget);
            let msg = read_message(target, state, lines, end, earlier, feedback.as_deref());
            let reply = self
                .llm
                .complete(&self.prompt(prompts::READ, msg, read_schema()))
                .await
                .context("reading the document")?;
            let problem = match parse_json::<ReadReply>(&reply) {
                Err(e) => format!("the reply was not one JSON object of the required shape: {e}"),
                Ok(r) => {
                    let in_window = usize::try_from(r.end_line)
                        .ok()
                        .filter(|l| (cursor..=end).contains(l));
                    match (r.action.as_str(), in_window) {
                        ("more", _) if end < lines.len() && budget < most => {
                            // The concept goes on: the same read, with more of the text.
                            budget = budget.saturating_mul(2).min(most);
                            continue;
                        }
                        ("more", _) if end < lines.len() => "this is as much text as one read \
                                                              can show: end the concept at the \
                                                              best break within the lines shown; \
                                                              its rest is read next"
                            .to_owned(),
                        ("more", _) => format!(
                            "the text shown reaches the end of the document (line {end}), so \
                             there is no more to show: end the concept at or before it"
                        ),
                        ("skip" | "concept", None) => format!(
                            "end_line {} is not one of the lines shown ({cursor} to {end})",
                            r.end_line
                        ),
                        ("skip", Some(end)) => {
                            return Ok(Read {
                                end,
                                concept: None,
                                thread: r.thread,
                            });
                        }
                        ("concept", Some(end)) => {
                            let title = r.title.as_deref().map(str::trim).unwrap_or_default();
                            match concept_page(target, r.page.as_deref()) {
                                Ok(_) if title.is_empty() => "a concept needs its title".into(),
                                Ok(page) => {
                                    return Ok(Read {
                                        end,
                                        concept: Some(Concept {
                                            title: title.to_owned(),
                                            page,
                                            keywords: r.keywords,
                                        }),
                                        thread: r.thread,
                                    });
                                }
                                Err(e) => e,
                            }
                        }
                        (other, _) => format!("action {other:?} is not concept, skip or more"),
                    }
                }
            };
            rejected += 1;
            if rejected == 2 {
                anyhow::bail!(
                    "the librarian's reading from line {cursor} could not be used twice: {problem}"
                );
            }
            tracing::info!(cursor, %problem, "read rejected; asking again");
            feedback = Some(problem);
        }
    }

    /// Files one concept: steps 2 to 6 for its lines, with the document's main thread and
    /// ledger. Edits the verifier objects to are held at once, one proposal per concept citing
    /// its lines. Returns whether the concept's page is in the wiki afterwards.
    #[allow(clippy::too_many_arguments)]
    async fn file_concept(
        &self,
        tenant: &Arc<Tenant>,
        job: &Ingest,
        reader: &Scope,
        rules: &str,
        today: &str,
        state: &ReadingState,
        lines: &[&str],
        (from, to): (usize, usize),
        concept: &Concept,
        cite: &str,
        earlier: Option<&Earlier>,
        first_sources: &[String],
        result: &mut JobResult,
    ) -> anyhow::Result<bool> {
        let doc = &state.document;
        let text = lines[from - 1..to].join("\n");
        // What the caller sent (source_url, date, entities…), with this concept's lines in place
        // of the whole content.
        let mut rec = job.record.clone();
        if let Some(o) = rec.as_object_mut() {
            o.remove("content");
        }
        rec["document"] = json!(doc.id);
        rec["title"] = json!(doc.title);
        rec["version"] = json!(state.version);
        rec["lines"] = json!(format!("{from}-{to}"));
        rec["concept"] = json!(concept.title);
        rec["text"] = json!(text);
        let record = serde_json::to_string_pretty(&rec)?;
        let mut note = format!(
            "Document {} ({}), version {}, is read one concept at a time; this call files the \
             concept in lines {from}-{to}.\n\nMain thread: {}\n\nRead so far (lines → concept \
             → page):\n\n{}\n\n",
            doc.id,
            doc.title,
            state.version,
            if state.reading.thread.is_empty() {
                "(none yet)"
            } else {
                &state.reading.thread
            },
            if state.reading.steps.is_empty() {
                "Nothing yet: this is the first concept.".to_owned()
            } else {
                state.reading.ledger_lines(MAX_LEDGER_LINES).join("\n")
            }
        );
        let mut first_pages = vec![concept.page.clone()];
        if let Some(e) = earlier {
            note.push_str(&version_note(doc, state.version));
            note.push_str("\n\n");
            match e.changes_text(from, to) {
                Some(c) => {
                    let _ = write!(
                        note,
                        "What changed in these lines since version {}: {c}\n\n",
                        e.version
                    );
                }
                None => {
                    let _ = write!(
                        note,
                        "None of these lines changed since version {}: keep what their pages \
                         say, unless they disagree with the lines.\n\n",
                        e.version
                    );
                }
            }
            for p in e.pages_for(from, to) {
                if !first_pages.contains(&p) {
                    first_pages.push(p);
                }
            }
        }
        let focus = format!(
            "File the concept \"{}\" (lines {from}-{to}) as `{}`: update that page if it \
             exists, create it if not. Write a condensed account of these lines, never a copy. \
             The rest of the document is filed in steps of its own; link to the ledger's pages \
             where this concept refers to them.",
            concept.title, concept.page
        );
        let mut keywords = vec![concept.title.clone()];
        keywords.extend(concept.keywords.iter().take(10).cloned());
        let unit = Unit {
            record,
            queries: documents::chunks(
                &format!("{}\n{text}", concept.title),
                MAX_QUERY_CHARS,
                CONCEPT_QUERIES,
            ),
            keywords,
            note: Some(note),
            focus: Some(focus),
            first_pages,
        };
        let rejected_before = result.rejected.len();
        let (pass, held) = self
            .plan_unit(
                tenant,
                job,
                reader,
                rules,
                today,
                &unit,
                first_sources,
                cite,
                result,
            )
            .await?;
        let _writer = tenant.wiki_writer.lock().await;
        apply_and_record(
            tenant,
            &job.target,
            &job.writer,
            Some(cite),
            today,
            &pass,
            result,
            &format!("ingest {}", job.target.kind.as_str()),
        )
        .await?;
        // Filed: its page is in the wiki and none of the concept's edits to it were held or
        // refused (a plan with nothing to add to an existing page files it too).
        let untouched = held.iter().all(|h| page_id(&h.edit.page()) != concept.page)
            && result.rejected[rejected_before..]
                .iter()
                .all(|r| page_id(&r.page) != concept.page);
        // Held at once, as one proposal per concept citing its lines.
        if !held.is_empty() {
            let target = &job.target;
            let id = hold(tenant, target, &job.writer, Some(cite), held).await?;
            record_log(
                tenant,
                &format!("ingest {}: held as {id}", target.kind.as_str()),
            )
            .await?;
            // The job's first proposal; `GET /proposals` lists them all.
            result.proposal.get_or_insert(id);
        }
        let exists = tenant
            .snapshot()
            .await?
            .catalog
            .get(&concept.page)
            .is_some();
        Ok(exists && untouched)
    }
}

/// The message for one read: the document, its main thread and ledger, for a new version how
/// the earlier one was read and what changed, and the numbered text from the cursor.
fn read_message(
    target: &Target,
    state: &ReadingState,
    lines: &[&str],
    end: usize,
    earlier: Option<&Earlier>,
    feedback: Option<&str>,
) -> String {
    let doc = &state.document;
    let cursor = state.cursor;
    let mut msg = format!(
        "## The document\n\nTitle: {}\nId: {}, version {}, {} lines.\n\n## Main thread\n\n{}\n\n\
         ## Read so far\n\n{}\n\n",
        doc.title,
        doc.id,
        state.version,
        lines.len(),
        if state.reading.thread.is_empty() {
            "None yet: write it from the opening (`thread`)."
        } else {
            &state.reading.thread
        },
        if state.reading.steps.is_empty() {
            "Nothing yet: this is the start of the document.".to_owned()
        } else {
            state.reading.ledger_lines(MAX_LEDGER_LINES).join("\n")
        }
    );
    if let Some(e) = earlier {
        if let Some(lines) = e.ledger_near(cursor, end) {
            let _ = write!(
                msg,
                "## How version {} read these lines\n\nIn version {}'s line numbers:\n\n{}\n\n",
                e.version,
                e.version,
                lines.join("\n")
            );
        }
        if let Some(c) = e.changes_text(cursor, end) {
            let _ = write!(
                msg,
                "## What changed in the lines shown since version {}\n\n{c}\n\n",
                e.version
            );
        }
    }
    let _ = write!(
        msg,
        "## Where pages go\n\nNew pages of this document are in `{0}/`: a new page's id is \
         `{0}/<name>`, a short lowercase name of letters, digits and `-`.\n\n",
        target.folder()
    );
    let _ = write!(
        msg,
        "## Text from line {cursor}\n\nLines {cursor} to {end} of {}; {}.\n\n```text\n",
        lines.len(),
        if end < lines.len() {
            format!("the document goes on after line {end}")
        } else {
            "this is the end of the document".to_owned()
        }
    );
    for (i, line) in lines[cursor - 1..end].iter().enumerate() {
        let _ = writeln!(msg, "{}| {line}", cursor + i);
    }
    msg.push_str("```\n");
    if let Some(f) = feedback {
        let _ = write!(
            msg,
            "\n## Why the last answer was rejected\n\n{f}\n\nAnswer again for the text from \
             line {cursor}.\n"
        );
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_removed_step_is_seen_by_the_step_before_it() {
        let old = "a\nb\nc\nd\ne\nf\n";
        // The second concept (c, d) is removed.
        let new = "a\nb\ne\nf\n";
        let reading = Reading {
            thread: String::new(),
            steps: vec![step(1, 2, "one"), step(3, 4, "two"), step(5, 6, "three")],
        };
        let e = Earlier::new(1, old, new, Some(reading));
        assert!(e.kept_at(1).is_none(), "read again, to see the removal");
        assert!(e.changes.within(1, 2, 1_000).unwrap().contains("- c"));
        assert_eq!(e.pages_for(1, 2), vec!["expertise/one", "expertise/two"]);
        let kept = e.kept_at(3).unwrap();
        assert_eq!(
            (kept.from, kept.to, kept.page.as_deref()),
            (3, 4, Some("expertise/three"))
        );
    }

    #[test]
    fn a_concept_without_a_page_is_read_again() {
        let old = "a\nb\nc\nd\n";
        let new = "a\nb\nc\nD\n";
        let mut held = step(1, 2, "one");
        held.page = None;
        let reading = Reading {
            thread: String::new(),
            steps: vec![held, step(3, 4, "two")],
        };
        let e = Earlier::new(1, old, new, Some(reading));
        assert!(e.kept_at(1).is_none(), "its edits were held: filed again");
        let skipped = Reading {
            thread: String::new(),
            steps: vec![LedgerStep {
                from: 1,
                to: 2,
                concept: None,
                page: None,
                cites: None,
            }],
        };
        let e = Earlier::new(1, old, new, Some(skipped));
        assert!(e.kept_at(1).is_some(), "a skip is kept");
    }

    fn step(from: usize, to: usize, page: &str) -> LedgerStep {
        LedgerStep {
            from,
            to,
            concept: Some(page.into()),
            page: Some(format!("expertise/{page}")),
            cites: Some(documents::cite("v1.json", from, to)),
        }
    }

    #[test]
    fn windows_hold_about_the_budget() {
        let lines = vec!["一二三四五六七八九十"; 100];
        // About 13 tokens a line.
        assert_eq!(window_end(&lines, 1, 130), 10);
        assert_eq!(window_end(&lines, 95, 130), 100, "no further than the end");
        assert_eq!(window_end(&lines, 5, 1), 5, "at least one line");
    }

    #[test]
    fn unchanged_steps_are_kept_and_changed_ones_read_again() {
        let old = "a\nb\nc\nd\ne\nf\n";
        // c changes; a line is added after e.
        let new = "a\nb\nC\nd\ne\nx\nf\n";
        let reading = Reading {
            thread: "t".into(),
            steps: vec![step(1, 2, "one"), step(3, 4, "two"), step(5, 6, "three")],
        };
        let e = Earlier::new(1, old, new, Some(reading));
        let kept = e.kept_at(1).unwrap();
        assert_eq!((kept.from, kept.to), (1, 2));
        assert_eq!(
            kept.cites.as_deref(),
            Some("v1.json#L1-2"),
            "the old citation"
        );
        assert!(e.kept_at(3).is_none(), "its line c changed");
        assert!(e.kept_at(5).is_none(), "a line was added inside it");
        assert!(e.kept_at(2).is_none(), "not where a step starts");
        assert_eq!(e.pages_for(3, 3), vec!["expertise/two"]);
        assert_eq!(e.pages_for(5, 7), vec!["expertise/three"]);
    }

    #[test]
    fn steps_after_a_change_line_up_again() {
        // A line is added at the top: every old step moves down by one.
        let old = "a\nb\nc\nd\n";
        let new = "new\na\nb\nc\nd\n";
        let reading = Reading {
            thread: String::new(),
            steps: vec![step(1, 2, "one"), step(3, 4, "two")],
        };
        let e = Earlier::new(1, old, new, Some(reading));
        assert!(e.kept_at(1).is_none(), "the new line is read");
        // The insertion before line 1 touches the first step, which is read again.
        assert!(e.kept_at(2).is_none());
        let kept = e.kept_at(4).unwrap();
        assert_eq!((kept.from, kept.to), (4, 5), "moved down, kept");
    }

    #[test]
    fn concept_pages_stay_in_the_folder() {
        let t = Target::new(
            crate::kinds::Kind::Expertise,
            super::super::PageAudience::Tenant,
        )
        .unwrap();
        assert_eq!(
            concept_page(&t, Some(" expertise/returns ")).unwrap(),
            "expertise/returns"
        );
        assert!(concept_page(&t, Some("experience/returns")).is_err());
        assert!(concept_page(&t, Some("expertise/../x")).is_err());
        assert!(concept_page(&t, None).is_err());
    }
}

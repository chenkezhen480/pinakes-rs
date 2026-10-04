//! Reading a document like a person (design, "Reading like a person, one concept at a time",
//! "The reading is a loop code runs, not an agent" and "Filing in batches, the text read once").
//!
//! Code numbers the lines and runs the loop; the model decides what the concepts are, where
//! each ends, and what each says. Each turn reads one window from the cursor (one structured
//! call): every complete concept in it, with its extract, and the lines to skip. Code checks the
//! answer (steps in order, within what was shown, numbers in each extract found in its lines),
//! files the window's concepts as one batch from their extracts, saves where the reading is,
//! and moves the cursor past the last complete concept. Every line is part of a concept or
//! skipped, so none is left unread; the cursor only moves forward and each read is asked again
//! three times at most, so the loop ends.
//!
//! A new version keeps, without asking the model, every earlier step whose lines didn't change,
//! and reads again only up to the next step it can keep.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::Arc;

use anyhow::Context;
use serde::Deserialize;
use serde_json::json;

use super::batch::Item;
use super::documents::{self, Changes, LedgerStep, Reading, ReadingState};
use super::{
    ATTEMPTS, Document, DocumentIn, DocumentMatch, Ingest, Librarian, Opening, nullable_string,
    object, page_id, parse_json, prompts, record_log, sources, strings,
};
use crate::identity::Scope;
use crate::jobs::JobResult;
use crate::librarian::edit::Target;
use crate::tenants::Tenant;
use crate::wiki::PagePath;

/// The window of the first read of an upload without an id, which recognizes it: enough for
/// its main thread and first concept. If the upload is a new version, this read is dropped,
/// so it is kept small; if it is a new document, it is the reading's first window.
const OPENING_WINDOW_TOKENS: usize = 2_000;
/// The smallest read window, in tokens, whatever the config says.
const MIN_WINDOW_TOKENS: usize = 500;
/// How many times the read window may grow, at most, for one long concept: by default up to
/// about 120,000 tokens, within today's models' context.
const WINDOW_GROWTH: usize = 6;
/// Most ledger lines one call is shown (the latest), so a long document's calls don't grow
/// with every step; the related pages show what came before.
const MAX_LEDGER_LINES: usize = 60;
/// Earlier steps shown on each side of the ones a read's lines were in.
const EARLIER_STEPS_AROUND: usize = 2;

/// The reply to one read: the steps of the window, in order.
#[derive(Deserialize)]
struct ReadReply {
    #[serde(default)]
    steps: Vec<StepReply>,
    #[serde(default)]
    more: bool,
    thread: Option<String>,
}

#[derive(Deserialize)]
struct StepReply {
    action: String,
    end_line: i64,
    title: Option<String>,
    page: Option<String>,
    #[serde(default)]
    keywords: Vec<String>,
    extract: Option<String>,
}

/// One window read, checked: its steps in order, and any new main thread.
struct Read {
    steps: Vec<ReadStep>,
    thread: Option<String>,
}

/// One step: the last line it takes, and its concept, or none when skipped.
struct ReadStep {
    end: usize,
    concept: Option<Concept>,
}

struct Concept {
    title: String,
    page: String,
    keywords: Vec<String>,
    /// What the lines say, in the librarian's words, with numbers as the lines have them.
    extract: String,
}

/// The shape of a read's reply.
pub fn read_schema() -> serde_json::Value {
    let step = object(&[
        (
            "action",
            json!({ "type": "string", "enum": ["concept", "skip"] }),
        ),
        ("end_line", json!({ "type": "integer" })),
        ("title", nullable_string()),
        ("page", nullable_string()),
        ("keywords", strings()),
        ("extract", nullable_string()),
    ]);
    object(&[
        ("steps", json!({ "type": "array", "items": step })),
        ("more", json!({ "type": "boolean" })),
        ("thread", nullable_string()),
    ])
}

/// The numbers in a text: digit runs, with decimals, thousands separators dropped and leading
/// zeros ignored.
fn numbers(text: &str) -> Vec<String> {
    let text = text.replace([',', '，'], "");
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in text.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() || (c == '.' && !cur.is_empty()) {
            cur.push(c);
        } else if !cur.is_empty() {
            // 1.50 is 1.5, and 007 is 7.
            let mut n = cur.trim_end_matches('.');
            if n.contains('.') {
                n = n.trim_end_matches('0').trim_end_matches('.');
            }
            let n = n.trim_start_matches('0');
            let n = if n.is_empty() || n.starts_with('.') {
                format!("0{n}")
            } else {
                n.to_owned()
            };
            if !out.contains(&n) {
                out.push(n);
            }
            cur.clear();
        }
    }
    out
}

/// The numbers an extract has that the text it was read from doesn't.
fn numbers_missing(extract: &str, lines: &str) -> Vec<String> {
    let source = numbers(lines);
    numbers(extract)
        .into_iter()
        .filter(|n| !source.contains(n))
        .collect()
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
        self.changes
            .within(from, to)
            .map(|c| format!("`-` removed, `+` added:\n\n```diff\n{c}```"))
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

    /// The last line to read from `cursor` before a step that can be kept as it is; the last
    /// line of the text when none follows.
    fn read_up_to(&self, cursor: usize, last: usize) -> usize {
        (cursor + 1..=last)
            .find(|&l| self.kept_at(l).is_some())
            .map_or(last, |l| l - 1)
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
    /// Files a document (expertise) by reading it, window by window; the concepts each window
    /// holds are filed as one batch from their extracts. Resumes an unfinished reading of the
    /// same text.
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
        let lines: Vec<&str> = d.content.lines().collect();
        // The first read of an upload recognized by it, when it is new: the reading's first turn.
        let mut first: Option<Read> = None;
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
                let mut s = ReadingState {
                    source: job.source.clone(),
                    content_hash: hash,
                    document: Document {
                        id: documents::new_document_id(),
                        audience: target.audience.key().to_owned(),
                        title: d.title.clone(),
                    },
                    how: DocumentMatch::New,
                    version: 1,
                    cursor: 1,
                    reading: Reading::default(),
                };
                match &d.given {
                    Some(given) => {
                        s.document = Document {
                            title: d.title.clone(),
                            ..given.clone()
                        };
                        s.how = DocumentMatch::Given;
                    }
                    None => {
                        // Read first: what the first read says the document is recognizes it,
                        // never cut pieces of its text. For a new document that read is the
                        // reading's first turn.
                        let read = self
                            .read_window(
                                target,
                                &s,
                                &lines,
                                lines.len(),
                                None,
                                OPENING_WINDOW_TOKENS.min(self.cfg.read_window_tokens),
                            )
                            .await?;
                        let mut terms = Vec::new();
                        if let Some(c) = read.steps.iter().find_map(|s| s.concept.as_ref()) {
                            terms.push(c.title.clone());
                            terms.extend(c.keywords.iter().cloned());
                        }
                        let last = read.steps.last().map_or(0, |s| s.end);
                        let opening = Opening {
                            thread: read.thread.clone().unwrap_or_default(),
                            terms,
                            text: lines[..last].join("\n"),
                        };
                        let new = s.document.clone();
                        let (found, how) = self
                            .identify(tenant, reader, &d.title, new, &opening)
                            .await?;
                        (s.document, s.how) = (found, how);
                        if how == DocumentMatch::New {
                            first = Some(read);
                        }
                    }
                }
                s
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

        let mut first_sources = earlier_sources.clone();
        first_sources.push(state.source.clone());
        while state.cursor <= lines.len() {
            if let Some(kept) = earlier.as_ref().and_then(|e| e.kept_at(state.cursor)) {
                state.cursor = kept.to + 1;
                state.reading.steps.push(kept);
                continue;
            }
            let limit = earlier
                .as_ref()
                .map_or(lines.len(), |e| e.read_up_to(state.cursor, lines.len()));
            let read = match first.take() {
                Some(read) => read,
                None => {
                    self.read_window(
                        target,
                        &state,
                        &lines,
                        limit,
                        earlier.as_ref(),
                        self.cfg.read_window_tokens,
                    )
                    .await?
                }
            };
            if let Some(t) = read
                .thread
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty())
            {
                state.reading.thread = t;
            }

            // The window's concepts, filed as one batch from their extracts.
            let note = document_note(&state, earlier.as_ref());
            let mut spans = Vec::new();
            let mut from = state.cursor;
            for step in &read.steps {
                spans.push((from, step.end));
                from = step.end + 1;
            }
            let mut items = Vec::new();
            let mut item_of_step = Vec::new();
            for (step, &(from, to)) in read.steps.iter().zip(&spans) {
                item_of_step.push(step.concept.as_ref().map(|c| {
                    items.push(concept_item(job, &state, earlier.as_ref(), (from, to), c));
                    items.len() - 1
                }));
            }
            let filed = if items.is_empty() {
                Vec::new()
            } else {
                self.file_batch(
                    tenant,
                    target,
                    &job.writer,
                    reader,
                    rules,
                    today,
                    Some(&note),
                    &first_sources,
                    &items,
                )
                .await?
            };
            let catalog = tenant.snapshot().await?;
            for f in &filed {
                result.pages_created.extend(f.created.iter().cloned());
                result.pages_updated.extend(f.updated.iter().cloned());
                result.rejected.extend(f.rejected.iter().cloned());
                if let Some(p) = &f.proposal {
                    // The job's first proposal; `GET /proposals` lists them all.
                    result.proposal.get_or_insert(p.clone());
                }
            }
            for ((step, &(from, to)), item) in read.steps.into_iter().zip(&spans).zip(item_of_step)
            {
                let ledger_step = match (step.concept, item) {
                    (Some(c), Some(i)) => {
                        // Filed: its page is in the wiki and none of its edits to that page was
                        // held or refused. Otherwise the ledger keeps it without a page, so
                        // later steps don't link to it and the next version files it again.
                        let filed = catalog.catalog.get(&c.page).is_some()
                            && !filed[i].not_written.contains(&page_id(&c.page));
                        LedgerStep {
                            from,
                            to,
                            concept: Some(c.title),
                            page: filed.then_some(c.page),
                            cites: Some(documents::cite(&state.source, from, to)),
                        }
                    }
                    _ => LedgerStep {
                        from,
                        to,
                        concept: None,
                        page: None,
                        cites: None,
                    },
                };
                tracing::info!(
                    from,
                    to,
                    concept = ledger_step.concept.as_deref().unwrap_or("(skipped)"),
                    page = ledger_step.page.as_deref().unwrap_or(""),
                    "read a step"
                );
                state.reading.steps.push(ledger_step);
                state.cursor = to + 1;
            }
            // Saved after every window, so a job that stops here resumes from the next line.
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

    /// One read from the cursor, up to line `limit` at most: asked until the answer can be
    /// used, with a wider window when not even one concept ends within it, and again with the
    /// reasons when it can't be used, up to `ATTEMPTS` tries.
    async fn read_window(
        &self,
        target: &Target,
        state: &ReadingState,
        lines: &[&str],
        limit: usize,
        earlier: Option<&Earlier>,
        window: usize,
    ) -> anyhow::Result<Read> {
        let cursor = state.cursor;
        let mut budget = window.max(MIN_WINDOW_TOKENS);
        let most = self
            .cfg
            .read_window_tokens
            .max(MIN_WINDOW_TOKENS)
            .saturating_mul(WINDOW_GROWTH);
        let mut feedback: Option<String> = None;
        let mut rejected = 0;
        loop {
            let end = window_end(lines, cursor, budget).min(limit);
            let msg = read_message(
                target,
                state,
                lines,
                end,
                limit,
                earlier,
                feedback.as_deref(),
            );
            // What an extract's numbers may come from besides the lines: the main thread, and
            // for a new version what changed (an extract says what a rule was before).
            let context = format!(
                "{}\n{}",
                state.reading.thread,
                earlier
                    .and_then(|e| e.changes_text(cursor, end))
                    .unwrap_or_default()
            );
            let reply = self
                .llm
                .complete(&self.prompt(prompts::READ, msg, read_schema()))
                .await
                .context("reading the document")?;
            let problem = match parse_json::<ReadReply>(&reply) {
                Err(e) => format!("the reply was not one JSON object of the required shape: {e}"),
                Ok(r) => {
                    match check_read(target, lines, &context, cursor, end, limit, r) {
                        Ok(Some(read)) => return Ok(read),
                        // Not one concept ends in the window: the same read, with more text.
                        Ok(None) if end < limit && budget < most => {
                            budget = budget.saturating_mul(2).min(most);
                            continue;
                        }
                        Ok(None) if end < limit => {
                            "this is as much text as one read can show: end \
                                                a concept at the best break within the lines \
                                                shown; its rest is read next"
                                .to_owned()
                        }
                        Ok(None) => format!(
                            "the lines shown reach line {end}, the last one to read now, so there \
                         is no more to show: end your last step at line {end}"
                        ),
                        Err(p) => p,
                    }
                }
            };
            rejected += 1;
            if rejected == ATTEMPTS {
                anyhow::bail!(
                    "the librarian's reading from line {cursor} could not be used in {ATTEMPTS} \
                     tries: {problem}"
                );
            }
            tracing::info!(cursor, %problem, "read rejected; asking again");
            feedback = Some(problem);
        }
    }
}

/// A read's reply checked against the window it was shown (lines `cursor..=end`, reading up to
/// `limit`): `None` when it asks for more text.
fn check_read(
    target: &Target,
    lines: &[&str],
    context: &str,
    cursor: usize,
    end: usize,
    limit: usize,
    r: ReadReply,
) -> Result<Option<Read>, String> {
    if r.steps.is_empty() {
        return if r.more {
            Ok(None)
        } else {
            Err("give the window's steps, or `more` when not even one concept ends in it".into())
        };
    }
    let read_text = format!(
        "{}\n{}\n{}",
        lines[cursor - 1..end].join("\n"),
        context,
        r.thread.as_deref().unwrap_or("")
    );
    let mut from = cursor;
    let mut steps = Vec::new();
    for (n, s) in r.steps.into_iter().enumerate() {
        let n = n + 1;
        let to = usize::try_from(s.end_line)
            .ok()
            .filter(|l| (from..=end).contains(l))
            .ok_or_else(|| {
                format!(
                    "step {n}: end_line {} is not between {from} and {end} (steps come in \
                     order, each after the last)",
                    s.end_line
                )
            })?;
        let concept = match s.action.as_str() {
            "skip" => None,
            "concept" => {
                let title = s.title.as_deref().map(str::trim).unwrap_or_default();
                if title.is_empty() {
                    return Err(format!("step {n}: a concept needs its title"));
                }
                let page = concept_page(target, s.page.as_deref())
                    .map_err(|e| format!("step {n}: {e}"))?;
                let extract = s.extract.as_deref().map(str::trim).unwrap_or_default();
                if extract.is_empty() {
                    return Err(format!("step {n}: a concept needs its extract"));
                }
                // Its numbers come from the text read (a date the document gives once is used
                // for every concept), never from nowhere.
                let missing = numbers_missing(extract, &read_text);
                if !missing.is_empty() {
                    return Err(format!(
                        "step {n}: the extract has {}, which the text shown doesn't; numbers, \
                         dates and amounts are written as the lines have them",
                        missing.join(", ")
                    ));
                }
                Some(Concept {
                    title: title.to_owned(),
                    page,
                    keywords: s.keywords,
                    extract: extract.to_owned(),
                })
            }
            other => return Err(format!("step {n}: action {other:?} is not concept or skip")),
        };
        steps.push(ReadStep { end: to, concept });
        from = to + 1;
    }
    if end == limit && from <= limit {
        return Err(format!(
            "every line up to {limit} is read now: lines {from}-{limit} are in no step"
        ));
    }
    Ok(Some(Read {
        steps,
        thread: r.thread,
    }))
}

/// What every item of a window shares: the document, its main thread and ledger, and for a new
/// version how to file changes.
fn document_note(state: &ReadingState, earlier: Option<&Earlier>) -> String {
    let doc = &state.document;
    let mut note = format!(
        "Document {} ({}), version {}, is read window by window; its items are the concepts \
         read in this window, each with its extract.\n\nMain thread: {}\n\nRead so far (lines \
         → concept → page):\n\n{}\n\n",
        doc.id,
        doc.title,
        state.version,
        if state.reading.thread.is_empty() {
            "(none yet)"
        } else {
            &state.reading.thread
        },
        if state.reading.steps.is_empty() {
            "Nothing yet: these are the first concepts.".to_owned()
        } else {
            state.reading.ledger_lines(MAX_LEDGER_LINES).join("\n")
        }
    );
    if earlier.is_some() {
        note.push_str(&version_note(doc, state.version));
        note.push('\n');
    }
    note
}

/// One concept as an item: the caller's record (source_url, date, entities…) without its
/// content, and the concept's extract in place of its lines.
fn concept_item(
    job: &Ingest,
    state: &ReadingState,
    earlier: Option<&Earlier>,
    (from, to): (usize, usize),
    c: &Concept,
) -> Item {
    let mut rec = job.record.clone();
    if let Some(o) = rec.as_object_mut() {
        o.remove("content");
    }
    rec["document"] = json!(state.document.id);
    rec["title"] = json!(state.document.title);
    rec["version"] = json!(state.version);
    rec["lines"] = json!(format!("{from}-{to}"));
    rec["concept"] = json!(c.title);
    rec["extract"] = json!(c.extract);
    let mut first_pages = vec![c.page.clone()];
    if let Some(e) = earlier {
        for p in e.pages_for(from, to) {
            if !first_pages.contains(&p) {
                first_pages.push(p);
            }
        }
    }
    let mut keywords = vec![c.title.clone()];
    keywords.extend(c.keywords.iter().take(10).cloned());
    Item {
        content: rec,
        // Searched by what the read says the concept is, never by pieces of its text.
        queries: vec![format!("{} {}", c.title, c.keywords.join(" "))],
        keywords,
        entities: job.entities.clone(),
        first_pages,
        read: Vec::new(),
        task: Some(format!(
            "File the concept \"{}\" (lines {from}-{to}) as `{}`: update that page if it exists, \
             create it if not, from its extract. If the extract holds something that belongs to \
             another concept, file that on that concept's page too: nothing in it is left out. \
             Link to the ledger's pages where this concept refers to them.",
            c.title, c.page
        )),
        page: Some(c.page.clone()),
        cite: documents::cite(&state.source, from, to),
    }
}

/// The message for one read: the document, its main thread and ledger, for a new version how
/// the earlier one read these lines and what changed, and the numbered text from the cursor.
fn read_message(
    target: &Target,
    state: &ReadingState,
    lines: &[&str],
    end: usize,
    limit: usize,
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
    let after = if end == lines.len() {
        "this is the end of the document".to_owned()
    } else if end == limit {
        format!(
            "the lines after {end} haven't changed and are kept as read before, so your last \
             step ends at line {end}"
        )
    } else {
        format!("the document goes on after line {end}")
    };
    let _ = write!(
        msg,
        "## Text from line {cursor}\n\nLines {cursor} to {end} of {}; {after}.\n\n```text\n",
        lines.len()
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
        assert!(e.changes.within(1, 2).unwrap().contains("- c"));
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
    fn extracts_keep_the_numbers_of_their_lines() {
        let lines = "运费险赔付寄回运费，上限 25 元；5,000 元以上需复核，费率 1.50%。";
        assert!(numbers_missing("上限 25 元，5000 元以上复核，费率 1.5%", lines).is_empty());
        assert_eq!(numbers_missing("上限 20 元", lines), vec!["20"]);
        assert_eq!(numbers("2026-11-01 起 7 天"), vec!["2026", "11", "1", "7"]);
    }

    #[test]
    fn a_new_version_reads_up_to_the_next_step_it_keeps() {
        let old = "a\nb\nc\nd\ne\nf\n";
        let new = "a\nB\nc\nd\ne\nf\n";
        let reading = Reading {
            thread: String::new(),
            steps: vec![step(1, 2, "one"), step(3, 4, "two"), step(5, 6, "three")],
        };
        let e = Earlier::new(1, old, new, Some(reading));
        assert!(e.kept_at(1).is_none());
        assert_eq!(e.read_up_to(1, 6), 2, "the step from line 3 is kept");
        assert_eq!(e.read_up_to(5, 6), 6);
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

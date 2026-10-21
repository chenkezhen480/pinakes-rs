//! Filing in batches (design, "Filing in batches, the text read once"). Records, and the
//! concepts a document's reading extracts, are items; a batch of them is filed by one write
//! call and one verify call, and each item is retried on its own: the items that pass are
//! written together, the ones that fail are sent again alone with their reasons, and an item
//! still failing after three retries is turned back without holding up the rest.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;

use anyhow::Context;
use serde::Deserialize;
use serde_json::json;

use super::edit::Target;
use super::{
    ATTEMPTS, Checked, Edit, HeldEdit, Librarian, MAX_PAGE_CHARS, Related, added_links,
    anchor_options, anchorable_pages, apply_and_record, check, cut, object, op, op_name, page_id,
    parse_json, plan_schema, prompts, record_log, string, strings,
};
use crate::identity::Scope;
use crate::jobs::{JobResult, Rejected};
use crate::tenants::Tenant;
use crate::wiki::Page;

/// Most items one write call files, so a reply stays a size the model writes well; a larger
/// batch is filed in rounds. The work is divided by items, never the text by size.
const ROUND_ITEMS: usize = 8;
/// Most related pages of each item shown whole; the rest are shown by title and summary. The
/// whole pages were most of what a write and a verify call sent (2026-10-20: 23 a round, 58%
/// of the input), while a concept mostly writes its own page and only links to the others.
const WHOLE_PAGES: usize = 3;
/// Most knowledge pages a record's session read that its item is shown and offered.
const MAX_READ_PAGES: usize = 8;
/// Told to an item sent back that came back without edits.
const DROPPED: &str = "- The last reply had no edits for this item, but it still has to be filed: correct the \
                       edits refused above instead of dropping them.";

/// One thing to file.
pub(super) struct Item {
    /// What the model files: the record, or a concept's extract, as JSON.
    pub content: serde_json::Value,
    /// What to search related pages with.
    pub queries: Vec<String>,
    pub keywords: Vec<String>,
    pub entities: Vec<String>,
    /// Pages shown first: a concept's page, and the pages an earlier version filed its lines
    /// into.
    pub first_pages: Vec<String>,
    /// For a record: the knowledge pages the caller's session read for the task. Those its
    /// pages may anchor to are shown first and named to the model.
    pub read: Vec<String>,
    /// What the item is for, when its content doesn't say it all (the page a concept goes to).
    pub task: Option<String>,
    /// The page the item must write (create it, or update it citing the item): the page the
    /// reading chose for a concept, which the ledger records.
    pub page: Option<String>,
    /// What the pages its edits write cite: the record, or the concept's lines in it.
    pub cite: String,
}

/// What became of one item.
#[derive(Debug, Default)]
pub(super) struct Filed {
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub rejected: Vec<Rejected>,
    /// The proposal its held edits went to.
    pub proposal: Option<String>,
    /// The proposal its held edits went to and that was approved at once (`auto_approve`).
    pub approved: Option<String>,
    /// Pages one of its edits wanted to write but that weren't written (held or refused).
    pub not_written: HashSet<String>,
}

/// The edits that file one item.
#[derive(Deserialize)]
struct ItemPlan {
    item: String,
    #[serde(default)]
    edits: Vec<PlanEdit>,
}

/// An edit of a batch plan: one of the plan's edits, or a concept's own new page given without
/// its text, which is the concept's extract. Asked to leave that text out of a `create`, the
/// model wrote it anyway for 339 of 461 pages (2026-10-20), so the op has no field for it.
#[derive(Deserialize)]
#[serde(untagged)]
enum PlanEdit {
    File(FileEdit),
    Edit(Edit),
}

#[derive(Deserialize)]
struct FileEdit {
    #[allow(dead_code)]
    op: FileOp,
    title: String,
    summary: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    about: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum FileOp {
    File,
}

impl PlanEdit {
    /// The edit it stands for, filing `item`: `file` creates the item's page from its extract
    /// under the title given.
    fn into_edit(self, item: &Item) -> Edit {
        match self {
            PlanEdit::Edit(e) => e,
            PlanEdit::File(f) => {
                let extract = item.content["extract"].as_str().unwrap_or_default();
                Edit::Create {
                    page: item.page.clone().unwrap_or_default(),
                    body: format!("# {}\n\n{extract}\n", f.title),
                    title: f.title,
                    summary: f.summary,
                    aliases: f.aliases,
                    keywords: f.keywords,
                    about: f.about,
                }
            }
        }
    }
}

#[derive(Deserialize)]
struct Plan {
    // Required: an empty answer (`{}`) is unreadable, not a plan without edits.
    items: Vec<ItemPlan>,
}

/// The edit plan for a batch: an entry per item, naming it, with the edits that file it, so an
/// item answers for itself (DeepSeek answered rounds of eight with one edit, 2026-10-20). A
/// list of entries rather than an object keyed by item: given that object, DeepSeek answered
/// the reply tool with an empty input, every time for some rounds (2026-10-20). With `file`
/// (every item a concept with its page), the edits may also be `file`.
pub fn batch_plan_schema(ids: &[String], file: bool) -> serde_json::Value {
    let mut entry = plan_schema();
    if file {
        entry["properties"]["edits"]["items"]["anyOf"]
            .as_array_mut()
            .expect("plan_schema lists its edits")
            .push(object(&[
                ("op", op("file")),
                ("title", string()),
                ("summary", string()),
                ("aliases", strings()),
                ("keywords", strings()),
                ("about", strings()),
            ]));
    }
    entry["properties"]
        .as_object_mut()
        .expect("plan_schema is an object")
        .insert("item".into(), json!({ "type": "string", "enum": ids }));
    entry["required"] = json!(["item", "edits"]);
    json!({
        "type": "object",
        "properties": { "items": { "type": "array", "items": entry } },
        "required": ["items"],
        "additionalProperties": false,
    })
}

fn id_of(i: usize) -> String {
    format!("i{}", i + 1)
}

/// One refused edit: its index in the round, whether code refused it (else the verifier), and
/// why.
type Refusal = (usize, bool, String);

/// What one write round came to, per item of the round.
enum Outcome {
    /// Every edit passed the checks and the verifier (or there were none).
    Passed,
    /// Waits for an item it depends on (it updates, links to or anchors to a page that item
    /// creates), which is held back this round.
    Waiting(usize),
    /// Why it was refused: by code (`true`) or by the verifier.
    Failed(Vec<Refusal>),
    /// None of its edits writes the page it must write.
    Missed(String),
}

impl Librarian {
    /// Files a batch of items for one target and writer: steps 2 to 6 for each. `note` is what
    /// every item shares (a document's main thread and ledger); `first_sources` are records
    /// whose pages come first among the related pages (a document's versions). Returns what
    /// became of each item, in order.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn file_batch(
        &self,
        tenant: &Arc<Tenant>,
        target: &Target,
        writer: &Scope,
        reader: &Scope,
        rules: &str,
        today: &str,
        note: Option<&str>,
        first_sources: &[String],
        items: &[Item],
    ) -> anyhow::Result<Vec<Filed>> {
        // Step 2: related pages and anchors, per item, without a model.
        let snap = tenant.snapshot().await?;
        // Node anchors are checked against the graph: without it the job fails rather than
        // having every node anchor refused.
        let graph = tenant.graph().await.context("anchors need the graph")?;
        let mut related: Vec<Related> = Vec::new();
        let mut anchors: Vec<Vec<String>> = Vec::new();
        // The pages a record's session read that its pages may anchor to come first.
        let mut reads: Vec<Vec<String>> = Vec::new();
        for item in items {
            let read = anchorable_pages(
                target,
                writer,
                &item.read,
                &snap.catalog,
                &graph,
                MAX_READ_PAGES,
            );
            let mut first = item.first_pages.clone();
            first.extend(
                read.iter()
                    .filter(|p| !item.first_pages.contains(p))
                    .cloned(),
            );
            let r = self
                .related(
                    tenant,
                    target,
                    reader,
                    &item.queries,
                    &item.keywords,
                    &item.entities,
                    first_sources,
                    &first,
                )
                .await?;
            reads.push(read);
            anchors.push(anchor_options(
                target,
                writer,
                &r,
                &item.entities,
                &snap.catalog,
                &graph,
            ));
            related.push(r);
        }

        let n = items.len();
        let mut out: Vec<Filed> = (0..n).map(|_| Filed::default()).collect();
        let mut tries = vec![0usize; n];
        let mut feedback: Vec<Vec<String>> = vec![Vec::new(); n];
        // The edits an item was last sent back for, and why: what it comes to if the model
        // then drops them instead of correcting them.
        let mut last: Vec<Vec<(Edit, bool, String)>> = vec![Vec::new(); n];
        let mut open: Vec<usize> = (0..n).collect();
        while !open.is_empty() {
            let round: Vec<usize> = open.iter().take(ROUND_ITEMS).copied().collect();
            let ids: Vec<String> = round.iter().map(|&i| id_of(i)).collect();
            let pages = pages_of(round.iter().map(|&i| &related[i]));
            let whole = whole_of(round.iter().map(|&i| &related[i]));

            // Step 3: one plan for the round.
            let msg = write_message(
                rules, target, today, note, items, &round, &related, &anchors, &reads, &pages,
                &whole, &feedback,
            );
            // A reply that can't be read is asked for again at once: DeepSeek now and then
            // answers the reply tool with an empty input (seen 2026-10-20), which says nothing
            // about the items, so it isn't counted against them unless every try is like it.
            let file = round
                .iter()
                .all(|&i| items[i].page.is_some() && items[i].content["extract"].is_string());
            let prompt = self.prompt(prompts::INGEST, msg, batch_plan_schema(&ids, file));
            let mut read = None;
            for _ in 0..ATTEMPTS {
                let reply = self
                    .llm
                    .complete(&prompt)
                    .await
                    .context("planning the edits")?;
                read = Some(parse_json::<Plan>(&reply));
                if matches!(read, Some(Ok(_))) {
                    break;
                }
            }
            let plan = match read.expect("asked at least once") {
                Ok(p) => p,
                Err(e) => {
                    let why =
                        format!("the reply was not one JSON object of the required shape: {e}");
                    for &i in &round {
                        tries[i] += 1;
                        feedback[i] = vec![why.clone()];
                        if tries[i] >= ATTEMPTS {
                            anyhow::bail!(
                                "the librarian's plan could not be read in {ATTEMPTS} tries: {e}"
                            );
                        }
                    }
                    continue;
                }
            };
            // The round's edits with the item each files: the items in order, and the pages
            // created first, so an item may change a page another item creates.
            let mut plan = plan;
            let mut tagged: Vec<(usize, Edit)> = round
                .iter()
                .flat_map(|&i| {
                    let id = id_of(i);
                    let mine: Vec<Edit> = plan
                        .items
                        .iter_mut()
                        .filter(|p| p.item == id)
                        .flat_map(|p| std::mem::take(&mut p.edits))
                        .map(|e| e.into_edit(&items[i]))
                        .collect();
                    mine.into_iter().map(move |e| (i, e))
                })
                .collect();
            tagged.sort_by_key(|(_, e)| !matches!(e, Edit::Create { .. }));
            let (owner, edits): (Vec<usize>, Vec<Edit>) = tagged.into_iter().unzip();
            let cites: Vec<Option<String>> =
                owner.iter().map(|&i| Some(items[i].cite.clone())).collect();

            // Step 4: the code checks and the verdicts, for the round at once.
            let checked = check(tenant, target, writer, &cites, today, &edits, true).await?;
            let shown = items_section(items, &round);
            let labels: Vec<String> = owner.iter().map(|&i| id_of(i)).collect();
            // The verifier sees whole only the pages the plan changes, the others by title and
            // summary (the related pages shown whole were a third of its input, 2026-10-20).
            let seen: HashSet<String> = edits.iter().map(|e| page_id(&e.page())).collect();
            let verdicts = self
                .verify(
                    rules,
                    target,
                    &shown,
                    note,
                    &pages,
                    Some(&seen),
                    &edits,
                    &labels,
                    &checked,
                )
                .await?;
            // A concept's page must be written by its item, even when it exists already: an item
            // that passed with nothing left its lines on no page (2026-10-20: a concept read onto
            // another document's page of the same name, or onto an earlier concept's page).
            let must_write = |i: usize| items[i].page.clone();
            let outcomes = settle_round(&round, &edits, &owner, &checked, &verdicts, &must_write);

            // Step 5: what passed is written now, together; what failed goes again.
            let mut pass: Vec<usize> = Vec::new();
            let mut finished: Vec<usize> = Vec::new();
            let mut finals: Vec<(usize, Vec<Refusal>)> = Vec::new();
            // Items whose last refused edits stand: sent back, they came back empty.
            let mut dropped: Vec<usize> = Vec::new();
            for (&i, outcome) in round.iter().zip(&outcomes) {
                match outcome {
                    // No edits after being sent back: the model dropped the item rather than
                    // correct it. Asked again; in the end its last refusals stand, so the record
                    // is held or refused as any item that keeps failing, never lost unseen.
                    Outcome::Passed if !last[i].is_empty() && !owner.contains(&i) => {
                        tries[i] += 1;
                        if tries[i] >= ATTEMPTS {
                            dropped.push(i);
                            finished.push(i);
                        } else {
                            if !feedback[i].iter().any(|f| f == DROPPED) {
                                feedback[i].push(DROPPED.to_owned());
                            }
                            tracing::info!(item = %id_of(i), "item sent back: no edits after a refusal");
                        }
                    }
                    Outcome::Passed => {
                        pass.push(i);
                        finished.push(i);
                    }
                    Outcome::Waiting(_) => {}
                    Outcome::Missed(why) => {
                        tries[i] += 1;
                        if tries[i] >= ATTEMPTS {
                            // Turned back: nothing of it is written, and what needs its pages
                            // goes with it.
                            let page = items[i].page.clone().unwrap_or_default();
                            out[i].not_written.insert(page_id(&page));
                            out[i].rejected.push(Rejected {
                                page,
                                reason: why.clone(),
                            });
                            let mine = (0..edits.len())
                                .filter(|&k| owner[k] == i)
                                .map(|k| (k, true, why.clone()))
                                .collect();
                            finals.push((i, mine));
                            finished.push(i);
                        } else {
                            feedback[i] = vec![format!("- {why}")];
                            tracing::info!(item = %id_of(i), reason = %why, "item sent back");
                        }
                    }
                    Outcome::Failed(why) => {
                        tries[i] += 1;
                        if tries[i] >= ATTEMPTS {
                            // Turned back: what passed of it is written, the rest refused or
                            // held.
                            finals.push((i, why.clone()));
                            pass.push(i);
                            finished.push(i);
                        } else {
                            feedback[i] = why
                                .iter()
                                .map(|(k, _, r)| {
                                    format!("- {} {}: {r}", op_name(&edits[*k]), edits[*k].page())
                                })
                                .collect();
                            last[i] = why
                                .iter()
                                .map(|(k, by_code, r)| (edits[*k].clone(), *by_code, r.clone()))
                                .collect();
                            tracing::info!(item = %id_of(i), reasons = ?feedback[i], "item sent back");
                        }
                    }
                }
            }
            // An item waiting on one turned back is written now too, and so on down the chain;
            // its edits that need what was turned back go with it, below.
            while let Some(i) = round.iter().zip(&outcomes).find_map(|(&i, o)| match o {
                Outcome::Waiting(on) if finished.contains(on) && !finished.contains(&i) => Some(i),
                _ => None,
            }) {
                pass.push(i);
                finished.push(i);
            }
            // An edit that needs a page whose creation was turned back (it updates, links to or
            // anchors to it) goes with it: held if that creation is held, refused if refused.
            let refused = loop {
                let gone: HashMap<String, bool> = finals
                    .iter()
                    .flat_map(|(_, why)| why.iter())
                    .filter(|(k, _, _)| matches!(edits[*k], Edit::Create { .. }))
                    .map(|(k, by_code, _)| (page_id(&edits[*k].page()), *by_code))
                    .collect();
                let refused: HashSet<usize> = finals
                    .iter()
                    .flat_map(|(_, why)| why.iter().map(|(k, _, _)| *k))
                    .collect();
                let next = (0..edits.len()).find_map(|k| {
                    if refused.contains(&k) || !pass.contains(&owner[k]) {
                        return None;
                    }
                    std::iter::once(edits[k].page())
                        .chain(added_links(&edits[k]))
                        .find_map(|p| gone.get(&page_id(&p)).copied())
                        .map(|by_code| (k, by_code))
                });
                let Some((k, by_code)) = next else {
                    break refused;
                };
                let reason = if by_code {
                    "it depends on a page whose creation was refused"
                } else {
                    "it depends on a page whose creation is held for review"
                };
                let entry = (k, by_code, reason.to_owned());
                match finals.iter_mut().find(|(i, _)| *i == owner[k]) {
                    Some((_, why)) => why.push(entry),
                    None => finals.push((owner[k], vec![entry])),
                }
            };
            let to_write: Vec<usize> = (0..edits.len())
                .filter(|&k| pass.contains(&owner[k]) && !refused.contains(&k))
                .collect();
            let _writer = tenant.lock_wiki().await?;
            if !to_write.is_empty() {
                let mut written = JobResult::default();
                let round_edits: Vec<Edit> = to_write.iter().map(|&k| edits[k].clone()).collect();
                let round_cites: Vec<Option<String>> =
                    to_write.iter().map(|&k| cites[k].clone()).collect();
                apply_and_record(
                    tenant,
                    target,
                    writer,
                    &round_cites,
                    today,
                    &round_edits,
                    &mut written,
                    &format!("ingest {}", target.kind.as_str()),
                )
                .await?;
                // Pages and refusals go back to the items whose edits they were.
                let item_of_page = |page: &str| {
                    to_write
                        .iter()
                        .find(|&&k| page_id(&edits[k].page()) == page_id(page))
                        .map(|&k| owner[k])
                };
                for p in written.pages_created {
                    if let Some(i) = item_of_page(&p) {
                        out[i].created.push(p);
                    }
                }
                for p in written.pages_updated {
                    if let Some(i) = item_of_page(&p) {
                        out[i].updated.push(p);
                    }
                }
                for r in written.rejected {
                    if let Some(i) = item_of_page(&r.page) {
                        out[i].not_written.insert(page_id(&r.page));
                        out[i].rejected.push(r);
                    }
                }
            }
            let settled = finals
                .into_iter()
                .map(|(i, why)| {
                    let why: Vec<(Edit, bool, String)> = why
                        .into_iter()
                        .map(|(k, by_code, r)| (edits[k].clone(), by_code, r))
                        .collect();
                    (i, why)
                })
                .chain(
                    dropped
                        .into_iter()
                        .map(|i| (i, std::mem::take(&mut last[i]))),
                )
                .collect::<Vec<_>>();
            for (i, why) in settled {
                let mut held = Vec::new();
                for (edit, by_code, reason) in why {
                    out[i].not_written.insert(page_id(&edit.page()));
                    if by_code {
                        out[i].rejected.push(Rejected {
                            page: edit.page(),
                            reason,
                        });
                    } else {
                        held.push(HeldEdit { edit, reason });
                    }
                }
                if !held.is_empty() {
                    let (id, approved) = self
                        .hold_or_approve(tenant, target, writer, Some(&items[i].cite), held)
                        .await?;
                    match approved {
                        // Written after all: the item's pages count as filed.
                        Some(written) => {
                            for p in written.pages_created {
                                out[i].not_written.remove(&page_id(&p));
                                out[i].created.push(p);
                            }
                            for p in written.pages_updated {
                                out[i].not_written.remove(&page_id(&p));
                                out[i].updated.push(p);
                            }
                            out[i].rejected.extend(written.rejected);
                            out[i].approved = Some(id);
                        }
                        None => {
                            record_log(
                                tenant,
                                &format!("ingest {}: held as {id}", target.kind.as_str()),
                            )
                            .await?;
                            out[i].proposal = Some(id);
                        }
                    }
                }
            }
            open.retain(|i| !finished.contains(i));
        }
        // A record that came to nothing at all says so in the log.
        let empty: Vec<&Item> = (0..n)
            .filter(|&i| {
                let f = &out[i];
                items[i].page.is_none()
                    && f.created.is_empty()
                    && f.updated.is_empty()
                    && f.rejected.is_empty()
                    && f.proposal.is_none()
                    && f.approved.is_none()
            })
            .map(|i| &items[i])
            .collect();
        if !empty.is_empty() {
            let _writer = tenant.lock_wiki().await?;
            for item in empty {
                record_log(
                    tenant,
                    &format!(
                        "ingest {}: nothing filed from {}",
                        target.kind.as_str(),
                        item.cite
                    ),
                )
                .await?;
            }
        }
        Ok(out)
    }
}

/// What each item of a round came to: its refused edits (by code, or by the verifier), or that
/// it waits on another item of the round whose new page it needs.
fn settle_round(
    round: &[usize],
    edits: &[Edit],
    owner: &[usize],
    checked: &Checked,
    verdicts: &HashMap<usize, Result<(), String>>,
    must_write: &dyn Fn(usize) -> Option<String>,
) -> Vec<Outcome> {
    let mut outcome: HashMap<usize, Outcome> = HashMap::new();
    for (k, (result, &item)) in checked.results.iter().zip(owner).enumerate() {
        let why = match (result, verdicts.get(&k)) {
            (Err(r), _) => (k, true, r.clone()),
            (Ok(()), Some(Err(r))) => (k, false, r.clone()),
            _ => continue,
        };
        match outcome
            .entry(item)
            .or_insert_with(|| Outcome::Failed(Vec::new()))
        {
            Outcome::Failed(all) => all.push(why),
            _ => unreachable!("only failures so far"),
        }
    }
    for &i in round {
        if let Some(page) = must_write(i)
            && !outcome.contains_key(&i)
            && !(0..edits.len()).any(|k| owner[k] == i && page_id(&edits[k].page()) == page)
        {
            outcome.insert(
                i,
                Outcome::Missed(format!(
                    "the concept is filed as `{page}`, as its task says: write that page from its \
                     extract (`file` it, or update it if it exists; link to it from other pages, \
                     never under another name)"
                )),
            );
        }
    }
    // An item that needs a page an item held back creates waits too, and so on down the chain.
    loop {
        let created_by: HashMap<String, usize> = (0..edits.len())
            .filter(|&k| matches!(edits[k], Edit::Create { .. }) && outcome.contains_key(&owner[k]))
            .map(|k| (page_id(&edits[k].page()), owner[k]))
            .collect();
        let next = round
            .iter()
            .filter(|i| !outcome.contains_key(i))
            .find_map(|&i| {
                (0..edits.len()).filter(|&k| owner[k] == i).find_map(|k| {
                    std::iter::once(edits[k].page())
                        .chain(added_links(&edits[k]))
                        .find_map(|p| created_by.get(&page_id(&p)).copied())
                        .map(|on| (i, on))
                })
            });
        let Some((i, on)) = next else {
            break;
        };
        outcome.insert(i, Outcome::Waiting(on));
    }
    round
        .iter()
        .map(|i| outcome.remove(i).unwrap_or(Outcome::Passed))
        .collect()
}

/// The pages a round shows, each once, in the order the items found them.
/// The pages a round shows whole: each item's first `WHOLE_PAGES` related pages (its own
/// page, an earlier version's pages, then the most related), the others by title and summary.
fn whole_of<'a>(related: impl Iterator<Item = &'a Related>) -> HashSet<String> {
    related
        .flat_map(|r| r.pages.iter().take(WHOLE_PAGES))
        .map(|p| page_id(p.path.as_str()))
        .collect()
}

fn pages_of<'a>(related: impl Iterator<Item = &'a Related>) -> Vec<Page> {
    let mut seen = HashSet::new();
    let mut pages = Vec::new();
    for r in related {
        for p in &r.pages {
            if seen.insert(p.path.clone()) {
                pages.push(p.clone());
            }
        }
    }
    pages
}

/// The round's items as a prompt section: each with its id and content.
fn items_section(items: &[Item], round: &[usize]) -> String {
    let mut msg = String::from("## Items\n\n");
    for &i in round {
        push_item(&mut msg, items, i);
    }
    msg
}

/// One item: its id, content and task.
fn push_item(msg: &mut String, items: &[Item], i: usize) {
    let content = serde_json::to_string_pretty(&items[i].content).unwrap_or_default();
    let _ = write!(msg, "### Item {}\n\n```json\n{content}\n```\n\n", id_of(i));
    if let Some(task) = &items[i].task {
        let _ = write!(msg, "Task: {task}\n\n");
    }
}

/// The pages shown: whole those of the job's kind and audience, which edits may change (and,
/// given `whole`, only those in it); the others as their id, title and summary, which is
/// enough to link to them or see they are about something else.
pub(super) fn push_shown_pages(
    msg: &mut String,
    title: &str,
    pages: &[Page],
    target: &Target,
    whole: Option<&HashSet<String>>,
) {
    let _ = write!(msg, "## {title}\n\n");
    if pages.is_empty() {
        msg.push_str("None: no page in the wiki is related yet.\n\n");
    }
    for page in pages {
        if target.allows(&page.path)
            && whole.is_none_or(|w| w.contains(&page_id(page.path.as_str())))
        {
            let text = cut(&page.render(), MAX_PAGE_CHARS);
            let _ = write!(msg, "### {}\n\n```markdown\n{text}\n```\n\n", page.path);
        } else {
            let _ = write!(
                msg,
                "### {}\n\n{} — {}\n\n",
                page.path,
                page.front.title.as_deref().unwrap_or(""),
                page.front.summary.as_deref().unwrap_or("")
            );
        }
    }
}

/// The write message for a round: what every item shares first (rules, where pages go, the
/// document, the pages), then each item with its related pages, anchors and, after a refusal,
/// why.
#[allow(clippy::too_many_arguments)]
fn write_message(
    rules: &str,
    target: &Target,
    today: &str,
    note: Option<&str>,
    items: &[Item],
    round: &[usize],
    related: &[Related],
    anchors: &[Vec<String>],
    reads: &[Vec<String>],
    pages: &[Page],
    whole: &HashSet<String>,
    feedback: &[Vec<String>],
) -> String {
    let mut msg = format!(
        "## Rules for {} pages\n\n{rules}\n\n## Where to write\n\n",
        target.kind.as_str()
    );
    if target.kind == crate::kinds::Kind::Preference {
        let _ = writeln!(
            msg,
            "The only page this job may write is `{}` (create it if it doesn't exist yet).",
            target.folder()
        );
    } else {
        let _ = writeln!(
            msg,
            "Pages of this job are in `{0}/`: a new page's id is `{0}/<name>`. Only pages in \
             that folder may be created or changed.",
            target.folder()
        );
    }
    let _ = write!(msg, "Today is {today}.\n\n");
    if let Some(note) = note {
        let _ = write!(msg, "## This document\n\n{note}\n\n");
    }
    push_shown_pages(&mut msg, "Pages", pages, target, Some(whole));
    // What the items' anchors share is said once: the node types are the tenant's, the same for
    // every item, and an item's anchor pages are mostly its related pages (the two lists were
    // over half of the items' text, 2026-10-20).
    let types = |i: usize| anchors[i].iter().find(|l| l.starts_with("- Node types:"));
    let shared = types(round[0]).filter(|t| round.iter().all(|&i| types(i) == Some(*t)));
    if let Some(t) = shared {
        let _ = write!(
            msg,
            "## Anchors\n\nNode types every item may anchor to: {}\n\n",
            t.trim_start_matches("- Node types: ")
        );
    }
    msg.push_str("## Items\n\n");
    for &i in round {
        push_item(&mut msg, items, i);
        if let Some(own) = &items[i].page
            && items[i].content["extract"].is_string()
        {
            let exists = related[i]
                .pages
                .iter()
                .any(|p| page_id(p.path.as_str()) == page_id(own));
            msg.push_str(if exists {
                "Its page exists: update it.\n\n"
            } else {
                "Its page is new: `file` creates it.\n\n"
            });
        }
        let related_ids = related[i]
            .pages
            .iter()
            .map(|p| p.path.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let related_ids = if related_ids.is_empty() {
            "none".to_owned()
        } else {
            related_ids
        };
        let lines: Vec<String> = anchors[i]
            .iter()
            .filter(|l| shared.is_none_or(|t| *l != t))
            .map(|l| match l.strip_prefix("- Pages: ") {
                Some(p) if p == related_ids => "- Pages: its related pages".to_owned(),
                _ => l.clone(),
            })
            .collect();
        let _ = write!(
            msg,
            "Related pages: {related_ids}\n\nAllowed anchors{}:\n{}\n\n",
            if shared.is_some() {
                " (and the node types above)"
            } else {
                ""
            },
            lines.join("\n")
        );
        if !reads[i].is_empty() {
            let _ = write!(
                msg,
                "Knowledge pages the caller read for this task: {}\n\n",
                reads[i].join(", ")
            );
        }
        if !feedback[i].is_empty() {
            let _ = write!(
                msg,
                "Why its last edits were rejected (send all of its edits again, fixed):\n{}\n\n",
                feedback[i].join("\n")
            );
        }
    }
    msg
}

/// One item's content: a record as received.
pub(super) fn record_item(job: &super::Ingest) -> Item {
    Item {
        content: job.record.clone(),
        queries: vec![job.query.clone()],
        keywords: job.keywords.clone(),
        entities: job.entities.clone(),
        first_pages: Vec::new(),
        read: job.read.clone(),
        task: None,
        page: None,
        cite: job.source.clone(),
    }
}

/// A batch key's parts, as one string: records filed together share all of them.
pub(super) fn batch_key(tenant: &str, target: &Target, writer: &Scope) -> String {
    let mut groups: Vec<&str> = writer.groups().iter().map(|g| g.dir.as_str()).collect();
    groups.sort_unstable();
    format!(
        "{tenant}|{}|{}|{}|{}",
        target.kind.as_str(),
        target.audience.key(),
        writer.user().map(|u| u.dir.as_str()).unwrap_or(""),
        groups.join(",")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiki::PagePath;

    #[test]
    fn the_batch_plan_names_its_items() {
        let s = batch_plan_schema(&["i1".into(), "i2".into()], false);
        let entry = &s["properties"]["items"]["items"];
        assert_eq!(entry["properties"]["item"]["enum"], json!(["i1", "i2"]));
        assert_eq!(entry["required"], json!(["item", "edits"]));
        assert_eq!(
            entry["properties"]["edits"],
            plan_schema()["properties"]["edits"]
        );
        // Concepts may also be filed with `file`, which has no text.
        let s = batch_plan_schema(&["i1".into()], true);
        let ops = &s["properties"]["items"]["items"]["properties"]["edits"]["items"]["anyOf"];
        let file = ops.as_array().unwrap().last().unwrap();
        assert_eq!(file["properties"]["op"]["enum"], json!(["file"]));
        assert!(file["properties"].get("body").is_none());
        let plan: Plan = serde_json::from_value(json!({ "items": [
            { "item": "i1", "edits": [{ "op": "supersede", "page": "expertise/a", "claim": "c",
                                         "replacement": null, "reason": "r" }] },
            { "item": "i2", "edits": [] } ] }))
        .unwrap();
        assert_eq!(plan.items[0].edits.len(), 1);
        assert!(plan.items[1].edits.is_empty());
        // `file` creates the item's page, its text the extract under the title.
        let mut plan: Plan = serde_json::from_value(json!({ "items": [
            { "item": "i1", "edits": [{ "op": "file", "title": "甲", "summary": "s",
                                         "aliases": [], "keywords": [], "about": [] }] } ] }))
        .unwrap();
        let item = Item {
            content: json!({ "extract": "甲的规定。" }),
            queries: Vec::new(),
            keywords: Vec::new(),
            entities: Vec::new(),
            first_pages: Vec::new(),
            read: Vec::new(),
            task: None,
            page: Some("expertise/jia".into()),
            cite: String::new(),
        };
        let edit = plan.items[0].edits.remove(0).into_edit(&item);
        assert!(
            matches!(&edit, Edit::Create { page, body, .. }
                if page == "expertise/jia" && body == "# 甲\n\n甲的规定。\n"),
            "{edit:?}"
        );
        // An empty answer is unreadable (asked for again), not a plan that files nothing.
        assert!(serde_json::from_value::<Plan>(json!({})).is_err());
    }

    #[test]
    fn each_item_shows_its_first_pages_whole() {
        let related = |names: &[&str]| Related {
            pages: names
                .iter()
                .map(|n| Page::new(PagePath::parse(&format!("expertise/{n}")).unwrap()))
                .collect(),
            entities: Vec::new(),
        };
        let a = related(&["a", "b", "c", "d", "e"]);
        let b = related(&["c", "f"]);
        let whole = whole_of([&a, &b].into_iter());
        let mut got: Vec<&str> = whole.iter().map(String::as_str).collect();
        got.sort();
        assert_eq!(
            got,
            ["expertise/a", "expertise/b", "expertise/c", "expertise/f"]
        );
        assert_eq!(
            pages_of([&a, &b].into_iter()).len(),
            6,
            "the rest by summary"
        );
    }

    fn create(page: &str, about: &str) -> Edit {
        Edit::Create {
            page: page.into(),
            title: "T".into(),
            summary: "S".into(),
            aliases: Vec::new(),
            keywords: Vec::new(),
            about: vec![about.into()],
            body: "# T".into(),
        }
    }

    /// An item that missed its page holds back what needs its other pages, down the chain.
    #[test]
    fn what_needs_a_missed_items_pages_waits() {
        let edits = [
            create("expertise/wrong-name", "commerce:order"),
            create("expertise/b", "expertise/wrong-name"),
            create("expertise/c", "expertise/b"),
            create("expertise/d", "commerce:order"),
        ];
        let checked = Checked {
            results: vec![Ok(()); 4],
            pages: Vec::new(),
            before: Vec::new(),
        };
        let must_write = |i: usize| (i == 0).then(|| "expertise/right".to_owned());
        let outcomes = settle_round(
            &[0, 1, 2, 3],
            &edits,
            &[0, 1, 2, 3],
            &checked,
            &HashMap::new(),
            &must_write,
        );
        assert!(matches!(outcomes[0], Outcome::Missed(_)));
        assert!(matches!(outcomes[1], Outcome::Waiting(0)));
        assert!(matches!(outcomes[2], Outcome::Waiting(1)));
        assert!(matches!(outcomes[3], Outcome::Passed));
    }
}

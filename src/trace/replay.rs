//! Replaying traces: each recorded search runs again with the current code and config, on the
//! wiki it ran on (rebuilt from the tenant's wiki history) or on today's wiki, and the report
//! lists what changed. Used by `pinakes-replay` and by tests.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, Lines};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;

use super::{Endpoint, TraceRecord};
use crate::config::SearchConfig;
use crate::embed::Embedder;
use crate::identity::Scope;
use crate::search::tracker::ReadState;
use crate::search::{SearchRequest, SearchSnapshot, search};
use crate::tenants::Tenants;
use crate::wiki::History;

/// Which wiki a replayed search runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WikiSource {
    /// The version the search ran on, from the wiki history; today's wiki when that version
    /// is no longer kept. Differences then come from the code or the config alone.
    Recorded,
    /// Today's wiki.
    Current,
}

/// Recorded wiki versions kept built during one replay; more are rebuilt as needed.
const CACHED_VERSIONS: usize = 8;

/// What changed in one slot.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SlotChange {
    pub slot: String,
    pub before: Vec<String>,
    pub after: Vec<String>,
    pub coverage_before: String,
    pub coverage_after: String,
}

/// One recorded search whose results differ now.
#[derive(Debug, Clone, Serialize)]
pub struct ReplayDiff {
    pub request_id: String,
    pub at: String,
    pub tenant: String,
    pub query: String,
    /// The search ran on another wiki than the recorded one, so a difference may come from the
    /// pages rather than from the search.
    pub wiki_changed: bool,
    /// The graph differs from the recorded one (graph data has no history, so replays always
    /// use today's graph).
    pub graph_changed: bool,
    /// The code and search settings that answered at the time.
    pub service_before: String,
    pub search_config_before: String,
    pub slots: Vec<SlotChange>,
    pub unmatched_before: Vec<String>,
    pub unmatched_after: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplayReport {
    /// The code and search settings of this replay.
    pub service: String,
    pub search_config: String,
    pub wiki: WikiSource,
    /// How many replayed searches each recorded service and search config answered.
    pub recorded_services: BTreeMap<String, usize>,
    pub recorded_search_configs: BTreeMap<String, usize>,
    pub searches: usize,
    pub same: usize,
    pub changed: usize,
    /// Reads, requests that no longer parse or validate, and tenants that no longer exist.
    pub skipped: usize,
    /// Searches that failed to run, with the reason.
    pub failed: Vec<(String, String)>,
    /// Searches that ran on another wiki than the recorded one.
    pub wiki_changed: usize,
    /// With `WikiSource::Recorded`: searches whose recorded version is no longer kept, which
    /// ran on today's wiki instead.
    pub wiki_missing: usize,
    /// Searches whose graph differs from the recorded one.
    pub graph_changed: usize,
    pub diffs: Vec<ReplayDiff>,
}

/// The trace files to read, in name order (so by day within a tenant). Files named directly
/// are always taken; inside folders (searched recursively) only `search-<date>.jsonl` files
/// of the last `last_days` days, today included, or all of them for `None`.
pub fn trace_files(paths: &[PathBuf], last_days: Option<u32>) -> anyhow::Result<Vec<PathBuf>> {
    let oldest = last_days.map(|n| super::today() - i64::from(n) + 1);
    let mut files = Vec::new();
    for p in paths {
        if p.is_file() {
            files.push(p.clone());
        } else {
            collect_files(p, oldest, &mut files)?;
        }
    }
    files.sort();
    Ok(files)
}

fn collect_files(dir: &Path, oldest: Option<i64>, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let p = entry?.path();
        if p.is_dir() {
            collect_files(&p, oldest, out)?;
            continue;
        }
        let day = p
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(super::file_day);
        if day.is_some_and(|d| oldest.is_none_or(|o| d >= o)) {
            out.push(p);
        }
    }
    Ok(())
}

/// Trace records read line by line from a list of files, so a replay holds one record at a
/// time however much it reads. Lines that don't parse and files that can't be opened are
/// counted, not fatal.
pub struct TraceReader {
    files: std::vec::IntoIter<PathBuf>,
    lines: Option<Lines<BufReader<File>>>,
    /// Lines that are not trace records.
    pub bad_lines: usize,
    /// Files or lines that could not be read, with the reason.
    pub errors: Vec<String>,
}

impl TraceReader {
    pub fn new(files: Vec<PathBuf>) -> Self {
        Self {
            files: files.into_iter(),
            lines: None,
            bad_lines: 0,
            errors: Vec::new(),
        }
    }
}

impl Iterator for TraceReader {
    type Item = TraceRecord;

    fn next(&mut self) -> Option<TraceRecord> {
        loop {
            let Some(lines) = &mut self.lines else {
                let file = self.files.next()?;
                match File::open(&file) {
                    Ok(f) => self.lines = Some(BufReader::new(f).lines()),
                    Err(e) => self.errors.push(format!("{}: {e}", file.display())),
                }
                continue;
            };
            match lines.next() {
                Some(Ok(line)) if line.trim().is_empty() => {}
                Some(Ok(line)) => match serde_json::from_str::<TraceRecord>(&line) {
                    Ok(record) => return Some(record),
                    Err(_) => self.bad_lines += 1,
                },
                Some(Err(e)) => {
                    self.errors.push(e.to_string());
                    self.lines = None;
                }
                None => self.lines = None,
            }
        }
    }
}

/// What a slot of a trace summary returned: page ids, or for relations `from type to` edges.
fn pages_of(summary: &Value, slot: &str) -> Vec<String> {
    if slot == "relations" {
        return summary[slot]["edges"]
            .as_array()
            .map(|edges| {
                edges
                    .iter()
                    .filter_map(|e| {
                        // `{from, type, to}`; traces written before 2026-10 hold arrays.
                        let parts = if e.is_object() {
                            ["from", "type", "to"]
                                .iter()
                                .filter_map(|k| e[*k].as_str().map(str::to_owned))
                                .collect()
                        } else {
                            strings(e)
                        };
                        (parts.len() == 3).then(|| parts.join(" "))
                    })
                    .collect()
            })
            .unwrap_or_default();
    }
    summary[slot]["hits"]
        .as_array()
        .map(|hits| {
            hits.iter()
                .filter_map(|h| h["page"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Runs every recorded search again and compares the results with the recording, one record
/// at a time; only the differences are kept. The read tracker is left out: a replay never
/// marks hits as read. `embedder` embeds queries in the meaning-based modes (and the pages of
/// recorded wiki versions, without storing their vectors); give the one `tenants` uses.
pub async fn replay(
    tenants: &Tenants,
    cfg: &SearchConfig,
    embedder: Option<&dyn Embedder>,
    wiki: WikiSource,
    records: impl IntoIterator<Item = TraceRecord>,
) -> anyhow::Result<ReplayReport> {
    let mut report = ReplayReport {
        service: crate::SERVICE.to_owned(),
        search_config: cfg.fingerprint(embedder.map(|e| e.model())),
        wiki,
        recorded_services: BTreeMap::new(),
        recorded_search_configs: BTreeMap::new(),
        searches: 0,
        same: 0,
        changed: 0,
        skipped: 0,
        failed: Vec::new(),
        wiki_changed: 0,
        wiki_missing: 0,
        graph_changed: 0,
        diffs: Vec::new(),
    };
    let mut recorded: HashMap<(String, String), Arc<SearchSnapshot>> = HashMap::new();
    let empty_graph = crate::graph::GraphSnapshot::empty().version;
    for record in records {
        if record.endpoint != Endpoint::Search {
            report.skipped += 1;
            continue;
        }
        let Ok(req) = serde_json::from_value::<SearchRequest>(record.request.clone()) else {
            report.skipped += 1;
            continue;
        };
        if req.validate().is_err() {
            report.skipped += 1;
            continue;
        }
        let mut missing = false;
        let run = async {
            let Some(tenant) = tenants.open(&record.tenant).await? else {
                return Ok(None);
            };
            let old = match (wiki, &record.snapshot) {
                (WikiSource::Recorded, Some(version)) => {
                    let key = (record.tenant.clone(), version.clone());
                    match recorded.get(&key) {
                        Some(s) => Some(s.clone()),
                        None => match History::new(&tenant.dir).load(version)? {
                            Some(pages) => {
                                if recorded.len() >= CACHED_VERSIONS {
                                    recorded.clear();
                                }
                                let mut s = SearchSnapshot::build(pages)?;
                                if let Some(embedder) = embedder {
                                    // Most parts are unchanged since: reuse today's vectors.
                                    use crate::search::vectors::{Earlier, build};
                                    let today = tenant.snapshot().await?;
                                    let earlier = match today.vectors.as_ref() {
                                        Some(v) if v.model == embedder.model() => {
                                            Earlier::Snapshot(v, &today.catalog)
                                        }
                                        _ => Earlier::Stored(
                                            tenant.store.embeddings(embedder.model()).await?,
                                        ),
                                    };
                                    let mut v = build(&s.catalog, earlier, embedder)?.vectors;
                                    // Searched as the service searches it: through the graph
                                    // index when there are enough rows, today's graph changed
                                    // for the version's pages (kept in memory only).
                                    crate::search::vectors::index(
                                        &mut v,
                                        &s.catalog,
                                        today.vectors.as_ref().map(|t| (t, &today.catalog)),
                                        None,
                                        cfg.vector_graph_rows,
                                    )?;
                                    s.vectors = Some(v);
                                }
                                let s = Arc::new(s);
                                recorded.insert(key, s.clone());
                                Some(s)
                            }
                            None => {
                                missing = true;
                                None
                            }
                        },
                    }
                }
                _ => None,
            };
            let snapshot = match old {
                Some(s) => s,
                None => tenant.snapshot().await?,
            };
            let graph = tenant.graph().await?;
            let scope = Scope::offline(tenant, record.user.clone(), record.groups.clone());
            let now = search(
                &snapshot,
                &graph,
                &scope,
                &req,
                cfg,
                &ReadState::default(),
                embedder,
            )?;
            anyhow::Ok(Some((
                now.trace_summary(),
                snapshot.version.clone(),
                graph.version.clone(),
            )))
        };
        let (now, version, graph_version) = match run.await {
            Ok(Some(done)) => done,
            Ok(None) => {
                report.skipped += 1;
                continue;
            }
            Err(e) => {
                report
                    .failed
                    .push((record.request_id.clone(), format!("{e:#}")));
                continue;
            }
        };

        report.searches += 1;
        if missing {
            report.wiki_missing += 1;
        }
        *report
            .recorded_services
            .entry(record.service.clone())
            .or_default() += 1;
        *report
            .recorded_search_configs
            .entry(record.search_config.clone())
            .or_default() += 1;
        let wiki_changed = record.snapshot.as_deref() != Some(version.as_str());
        if wiki_changed {
            report.wiki_changed += 1;
        }
        // Traces from before the graph have no version: they ran on no graph at all, the same
        // as an empty one.
        let graph_changed = record.graph.as_deref().unwrap_or(&empty_graph) != graph_version;
        if graph_changed {
            report.graph_changed += 1;
        }
        let mut slots = Vec::new();
        for slot in ["memories", "knowledge", "relations"] {
            let before = pages_of(&record.result, slot);
            let after = pages_of(&now, slot);
            let coverage_before = record.result[slot]["coverage"]
                .as_str()
                .unwrap_or("")
                .to_owned();
            let coverage_after = now[slot]["coverage"].as_str().unwrap_or("").to_owned();
            if before != after || coverage_before != coverage_after {
                slots.push(SlotChange {
                    slot: slot.to_owned(),
                    before,
                    after,
                    coverage_before,
                    coverage_after,
                });
            }
        }
        let unmatched_before = strings(&record.result["unmatched_keywords"]);
        let unmatched_after = strings(&now["unmatched_keywords"]);
        if slots.is_empty() && unmatched_before == unmatched_after {
            report.same += 1;
            continue;
        }
        report.changed += 1;
        report.diffs.push(ReplayDiff {
            request_id: record.request_id.clone(),
            at: record.at.clone(),
            tenant: record.tenant.clone(),
            query: req.query.clone(),
            wiki_changed,
            graph_changed,
            service_before: record.service.clone(),
            search_config_before: record.search_config.clone(),
            slots,
            unmatched_before,
            unmatched_after,
        });
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Relations edges are compared the same whether a trace holds them as objects or, as
    /// traces written before the format was settled do, as `[from, type, to]` arrays.
    #[test]
    fn edges_of_both_trace_forms() {
        let new = serde_json::json!({ "relations": { "edges": [
            { "from": "m:order/1", "type": "placed_by", "to": "m:customer/2" }
        ] } });
        let old = serde_json::json!({ "relations": { "edges": [
            ["m:order/1", "placed_by", "m:customer/2"]
        ] } });
        assert_eq!(
            pages_of(&new, "relations"),
            vec!["m:order/1 placed_by m:customer/2"]
        );
        assert_eq!(pages_of(&new, "relations"), pages_of(&old, "relations"));
    }
}

//! Search traces (design, "Search traces and replay"): one JSON line per `/search` and `/read`
//! in the tenant's `logs/search-<date>.jsonl`, written by a background thread so the read path
//! never waits on a disk. Next to each day's file, `logs/versions-<date>.txt` lists the wiki
//! versions that day's lines ran on. Old files are removed after the retention period, and so
//! are the wiki versions (`wiki::History`) no remaining day refers to.

pub mod replay;

use std::collections::{BTreeMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::wiki::History;

use serde::{Deserialize, Serialize};

/// Bumped when the line format changes incompatibly.
pub const TRACE_VERSION: u32 = 1;
/// Lines waiting for the writer; past this, new lines are dropped and counted.
const QUEUE: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Endpoint {
    Search,
    Read,
}

/// One line of a trace file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceRecord {
    pub v: u32,
    /// UTC, RFC 3339 with milliseconds.
    pub at: String,
    pub request_id: String,
    pub endpoint: Endpoint,
    /// Tenant, user and group folder names (hashes), never raw ids.
    pub tenant: String,
    pub user: Option<String>,
    pub groups: Vec<String>,
    /// Version of the search snapshot the request ran on, when it used one; the wiki history
    /// keeps that version for replays.
    pub snapshot: Option<String>,
    /// Version of the graph snapshot a search ran on. Graph data has no history, so replays
    /// always use today's graph and report when it changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<String>,
    /// The code that answered (`pinakes::SERVICE`).
    #[serde(default)]
    pub service: String,
    /// Fingerprint of the search settings (`SearchConfig::fingerprint`).
    #[serde(default)]
    pub search_config: String,
    pub request: serde_json::Value,
    pub result: serde_json::Value,
    /// Microseconds per step, plus `total`.
    pub timings_us: BTreeMap<String, u64>,
}

impl TraceRecord {
    /// A record stamped with the current time and a fresh request id.
    pub fn new(endpoint: Endpoint, tenant: &str) -> Self {
        Self {
            v: TRACE_VERSION,
            at: now_rfc3339(),
            request_id: hex::encode(rand::random::<[u8; 8]>()),
            endpoint,
            tenant: tenant.to_owned(),
            user: None,
            groups: Vec::new(),
            snapshot: None,
            graph: None,
            service: crate::SERVICE.to_owned(),
            search_config: String::new(),
            request: serde_json::Value::Null,
            result: serde_json::Value::Null,
            timings_us: BTreeMap::new(),
        }
    }
}

enum Msg {
    Record(Box<TraceRecord>),
    Flush(mpsc::Sender<()>),
}

/// The handle request handlers log through. Dropping the last handle stops the writer.
pub struct TraceLog {
    tx: Option<SyncSender<Msg>>,
    dropped: AtomicU64,
}

impl TraceLog {
    /// Starts the writer for tenants under `tenants_root`; `None` for `retention_days` keeps
    /// files forever. A disabled log accepts records and discards them.
    pub fn start(tenants_root: PathBuf, enabled: bool, retention_days: Option<u32>) -> Self {
        if !enabled {
            return Self {
                tx: None,
                dropped: AtomicU64::new(0),
            };
        }
        let (tx, rx) = mpsc::sync_channel(QUEUE);
        std::thread::Builder::new()
            .name("trace-writer".into())
            .spawn(move || writer(rx, &tenants_root, retention_days))
            .expect("trace writer thread starts");
        Self {
            tx: Some(tx),
            dropped: AtomicU64::new(0),
        }
    }

    /// Queues a record without waiting. When the writer is behind, the record is dropped.
    pub fn record(&self, record: TraceRecord) {
        let Some(tx) = &self.tx else {
            return;
        };
        match tx.try_send(Msg::Record(Box::new(record))) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                let n = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                if n.is_power_of_two() {
                    tracing::warn!(dropped = n, "trace writer is behind; dropping lines");
                }
            }
            Err(TrySendError::Disconnected(_)) => {
                tracing::error!("trace writer has stopped");
            }
        }
    }

    /// Waits until every record queued so far is on disk. Blocks; for tests and shutdown.
    pub fn flush(&self) {
        if let Some(tx) = &self.tx {
            let (done_tx, done_rx) = mpsc::channel();
            if tx.send(Msg::Flush(done_tx)).is_ok() {
                let _ = done_rx.recv();
            }
        }
    }

    /// Records dropped because the writer was behind.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// The trace file of one tenant for one day.
pub fn trace_file(tenants_root: &Path, tenant_dir: &str, date: &str) -> PathBuf {
    tenants_root
        .join(tenant_dir)
        .join("logs")
        .join(format!("search-{date}.jsonl"))
}

/// The list of wiki versions one tenant's traces used on one day.
fn versions_file(tenants_root: &Path, tenant_dir: &str, date: &str) -> PathBuf {
    tenants_root
        .join(tenant_dir)
        .join("logs")
        .join(format!("versions-{date}.txt"))
}

fn writer(rx: Receiver<Msg>, root: &Path, retention_days: Option<u32>) {
    let mut last_cleanup: Option<String> = None;
    // (tenant, date, version) already listed in a versions file by this writer.
    let mut listed: HashSet<(String, String, String)> = HashSet::new();
    for msg in rx {
        let record = match msg {
            Msg::Record(r) => r,
            Msg::Flush(done) => {
                let _ = done.send(());
                continue;
            }
        };
        let date = &record.at[..10];
        // Write and list first, so the day's cleanup below already sees this line's version
        // as in use and never collects it.
        match append(root, &record, date) {
            Err(e) => {
                tracing::warn!(error = %e, tenant = %record.tenant, "writing a trace line failed")
            }
            Ok(()) => {
                if let Some(version) = &record.snapshot {
                    let key = (record.tenant.clone(), date.to_owned(), version.clone());
                    if !listed.contains(&key) {
                        match list_version(root, &record.tenant, date, version) {
                            Ok(()) => {
                                listed.insert(key);
                            }
                            Err(e) => tracing::warn!(error = %e, "listing a wiki version failed"),
                        }
                    }
                }
            }
        }
        if let Some(days) = retention_days
            && last_cleanup.as_deref() != Some(date)
        {
            remove_expired(root, date, days);
            last_cleanup = Some(date.to_owned());
            listed.retain(|(_, d, _)| d == date);
        }
    }
}

fn list_version(root: &Path, tenant: &str, date: &str, version: &str) -> std::io::Result<()> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(versions_file(root, tenant, date))?
        .write_all(format!("{version}\n").as_bytes())
}

fn append(root: &Path, record: &TraceRecord, date: &str) -> std::io::Result<()> {
    // The tenant folder must already exist; traces never create tenants.
    let tenant_dir = root.join(&record.tenant);
    if !tenant_dir.is_dir() {
        return Ok(());
    }
    let file = trace_file(root, &record.tenant, date);
    std::fs::create_dir_all(file.parent().expect("has a parent"))?;
    let mut line = serde_json::to_vec(record).map_err(std::io::Error::other)?;
    line.push(b'\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)?
        .write_all(&line)
}

/// Removes trace and version-list files dated more than `days` days before `today`
/// (YYYY-MM-DD) in every tenant, then the wiki versions that no remaining list names and that
/// weren't used within the same period.
fn remove_expired(root: &Path, today: &str, days: u32) {
    let Some(today) = parse_date(today) else {
        return;
    };
    let Ok(tenants) = std::fs::read_dir(root) else {
        return;
    };
    // A retention longer than the clock can count back keeps every version.
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(u64::from(days) * 86_400))
        .unwrap_or(UNIX_EPOCH);
    for tenant in tenants.flatten() {
        // A tenant without logs was never searched: none of its versions is referenced, but
        // its history is still collected.
        let files = std::fs::read_dir(tenant.path().join("logs"))
            .into_iter()
            .flatten();
        let mut keep: HashSet<String> = HashSet::new();
        for file in files.flatten() {
            let name = file.file_name();
            let name = name.to_str().unwrap_or("");
            let Some(date) = file_day(name).or_else(|| versions_file_day(name)) else {
                continue;
            };
            if today - date > i64::from(days) {
                let _ = std::fs::remove_file(file.path());
            } else if versions_file_day(name).is_some()
                && let Ok(text) = std::fs::read_to_string(file.path())
            {
                keep.extend(text.lines().map(str::to_owned));
            }
        }
        if let Err(e) = History::new(&tenant.path()).collect_garbage(&keep, cutoff) {
            tracing::warn!(error = %e, "collecting old wiki versions failed");
        }
    }
}

// ---- dates without a date crate -------------------------------------------------------------

/// The day of a trace file from its name (`search-2026-09-30.jsonl`), as days since 1970.
pub(crate) fn file_day(name: &str) -> Option<i64> {
    name.strip_prefix("search-")
        .and_then(|n| n.strip_suffix(".jsonl"))
        .and_then(parse_date)
}

fn versions_file_day(name: &str) -> Option<i64> {
    name.strip_prefix("versions-")
        .and_then(|n| n.strip_suffix(".txt"))
        .and_then(parse_date)
}

/// Today (UTC) as days since 1970.
pub(crate) fn today() -> i64 {
    parse_date(&now_rfc3339()[..10]).expect("now is a valid date")
}

/// Now as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn now_rfc3339() -> String {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = since.as_secs() as i64;
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        since.subsec_millis()
    )
}

/// Days since 1970-01-01 of a `YYYY-MM-DD` date.
fn parse_date(s: &str) -> Option<i64> {
    let mut it = s.splitn(3, '-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    ((1..=12).contains(&m) && (1..=31).contains(&d)).then(|| days_from_civil(y, m, d))
}

// Howard Hinnant's civil calendar algorithms.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_roundtrip() {
        for (s, days) in [
            ("1970-01-01", 0),
            ("2000-03-01", 11_017),
            ("2026-09-30", 20_726),
        ] {
            assert_eq!(parse_date(s), Some(days), "{s}");
            let (y, m, d) = civil_from_days(days);
            assert_eq!(format!("{y:04}-{m:02}-{d:02}"), s);
        }
        assert_eq!(parse_date("2026-13-01"), None);
        let now = now_rfc3339();
        assert_eq!(now.len(), 24, "{now}");
        assert!(parse_date(&now[..10]).is_some());
    }

    /// A retention period longer than the clock can count back must not panic the writer.
    #[test]
    fn huge_retention_keeps_everything() {
        let dir = tempfile::tempdir().unwrap();
        let tenant = dir.path().join("t_c");
        let mut page = crate::wiki::Page::new(crate::wiki::PagePath::parse("expertise/a").unwrap());
        page.body = "a\n".into();
        History::new(&tenant)
            .save("00000000000000dd", &[page])
            .unwrap();
        remove_expired(dir.path(), &now_rfc3339()[..10], u32::MAX);
        assert!(
            tenant
                .join("history/versions/00000000000000dd.json")
                .exists()
        );
    }

    #[test]
    fn writes_lines_and_removes_expired_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("t_a")).unwrap();
        let old = trace_file(root, "t_a", "2020-01-01");
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, "{}\n").unwrap();

        let log = TraceLog::start(root.to_path_buf(), true, Some(30));
        let mut r = TraceRecord::new(Endpoint::Search, "t_a");
        r.request = serde_json::json!({ "query": "q" });
        log.record(r.clone());
        // A tenant folder that doesn't exist gets nothing.
        log.record(TraceRecord::new(Endpoint::Search, "t_missing"));
        log.flush();

        let file = trace_file(root, "t_a", &r.at[..10]);
        let text = std::fs::read_to_string(file).unwrap();
        let back: TraceRecord = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(back, r);
        assert!(!old.exists(), "expired file is removed");
        assert!(!root.join("t_missing").exists());
        assert_eq!(log.dropped(), 0);
    }

    #[test]
    fn versions_are_listed_and_unreferenced_ones_collected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let tenant = root.join("t_a");
        std::fs::create_dir_all(&tenant).unwrap();
        let history = History::new(&tenant);
        let mut page = crate::wiki::Page::new(crate::wiki::PagePath::parse("expertise/a").unwrap());
        page.body = "a\n".into();
        history
            .save("00000000000000aa", std::slice::from_ref(&page))
            .unwrap();
        history
            .save("00000000000000bb", std::slice::from_ref(&page))
            .unwrap();
        // Make both versions look old.
        let old = SystemTime::now() - Duration::from_secs(90 * 86_400);
        for v in ["00000000000000aa", "00000000000000bb"] {
            std::fs::File::options()
                .write(true)
                .open(tenant.join("history/versions").join(format!("{v}.json")))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }

        // A line on version aa lists it; the first line of the day also runs the collection.
        let log = TraceLog::start(root.to_path_buf(), true, Some(30));
        let mut r = TraceRecord::new(Endpoint::Search, "t_a");
        r.snapshot = Some("00000000000000aa".into());
        log.record(r.clone());
        log.record(r.clone());
        log.flush();
        let listed = std::fs::read_to_string(versions_file(root, "t_a", &r.at[..10])).unwrap();
        assert_eq!(listed, "00000000000000aa\n");

        // That first collection already kept aa, the version the line ran on, although it
        // looked old, and dropped bb (old and unlisted).
        assert!(history.load("00000000000000aa").unwrap().is_some());
        assert!(history.load("00000000000000bb").unwrap().is_none());
    }

    #[test]
    fn tenants_never_searched_are_collected_too() {
        let dir = tempfile::tempdir().unwrap();
        let tenant = dir.path().join("t_b");
        let history = History::new(&tenant);
        let mut page = crate::wiki::Page::new(crate::wiki::PagePath::parse("expertise/a").unwrap());
        page.body = "a\n".into();
        history.save("00000000000000cc", &[page]).unwrap();
        let manifest = tenant.join("history/versions/00000000000000cc.json");
        std::fs::File::options()
            .write(true)
            .open(&manifest)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(90 * 86_400))
            .unwrap();
        assert!(!tenant.join("logs").exists());
        remove_expired(dir.path(), &now_rfc3339()[..10], 30);
        assert!(!manifest.exists());
    }

    #[test]
    fn disabled_log_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("t_a")).unwrap();
        let log = TraceLog::start(dir.path().to_path_buf(), false, Some(30));
        log.record(TraceRecord::new(Endpoint::Search, "t_a"));
        log.flush();
        assert!(!dir.path().join("t_a").join("logs").exists());
    }
}

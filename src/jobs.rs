//! The per-tenant write queue and job status (design, "The librarian": every write is a
//! background job). Each tenant has one queue, worked by one task, so a tenant's jobs run one
//! at a time in the order they arrived, while other tenants' queues run alongside. Status is
//! held in memory and saved to the tenant's store as jobs are queued and finish, so it outlives
//! a restart: a job a restart cut off is shown failed, with what to do. Finished jobs are
//! dropped after `[jobs] keep_finished_minutes`.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::store::{Store, StoredJob};

/// Why a job saved before a restart, and not finished then, failed.
pub const INTERRUPTED: &str =
    "the service restarted before this job finished; nothing of it was kept: send it again";

/// What a job did.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct JobResult {
    pub pages_created: Vec<String>,
    pub pages_updated: Vec<String>,
    /// Edits refused by the checks code runs, with the reason.
    pub rejected: Vec<Rejected>,
    /// The proposal holding the edits the verifier didn't accept, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal: Option<String>,
    /// The raw record the job filed, as `/read` takes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// For a document (`/ingest/expertise`): its id, issued by the service the first time the
    /// document is filed. Send it as `document` with the next upload of the same document so
    /// that upload is filed as its new version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<String>,
    /// How the job knew the document: `given` (the caller sent the id), `matched` (the
    /// librarian recognized a document filed before, from its wiki pages) or `new` (a new id).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_match: Option<crate::librarian::DocumentMatch>,
    /// The document's version this upload became, from 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    /// For a lint run: what it found and proposed (the findings are at `GET /lint`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lint: Option<crate::librarian::lint::LintRun>,
    /// For an acquisition run: what it searched for and proposed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acquisition: Option<crate::acquisition::RunReport>,
    /// For an approved graph draft: what it applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph: Option<crate::api::draft::GraphApplied>,
    /// For a note (`/ingest/note`): the records the librarian split it into, each filed for its
    /// own audience (a decision for the user, a lesson for the team). The pages of all of them
    /// are in `pages_created` and `pages_updated`; `source` is the note as it arrived.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub split: Vec<NoteFiled>,
    /// Proposals of this job approved at once (`[librarian] auto_approve`): the verifier held
    /// their edits and they were written all the same; their pages are in the lists above.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved: Vec<String>,
}

/// One record a note was split into.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct NoteFiled {
    /// `decision` or `experience`.
    pub kind: String,
    /// Who it is for: `tenant`, or a group's or user's folder name (`g_…`, `u_…`).
    pub audience: String,
    /// The record, as `/read` takes it.
    pub source: String,
    /// The proposal holding what the verifier didn't accept of it, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Rejected {
    pub page: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
}

/// A job as `/jobs/{id}` shows it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct JobInfo {
    pub job: String,
    pub kind: String,
    pub status: JobState,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<JobResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// In the answer to `/ingest/expertise` only: the document id the caller sent. Without
    /// one, the id is in `result.document` once the job is done.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<String>,
    /// While a document is being read: how far the reading has come. Gone once the job ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<JobProgress>,
}

/// How far a running job has come in the document it reads, window by window.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct JobProgress {
    /// Lines of the document read so far, as the reading numbers them (its Markdown).
    pub lines_read: usize,
    pub lines_total: usize,
    /// Concepts read so far that are filed in the wiki.
    pub concepts_filed: usize,
}

tokio::task_local! {
    /// The job the current task runs (the queue and the job's id), so the work deep inside it
    /// can say how far it has come.
    static CURRENT: (Jobs, String);
}

/// Shows how far the running job has come, on `/jobs`. Does nothing outside a queued job.
pub fn report_progress(progress: JobProgress) {
    let _ = CURRENT.try_with(|(jobs, id)| {
        if let Some(e) = jobs.lock().jobs.get_mut(id)
            && e.info.status == JobState::Running
        {
            e.info.progress = Some(progress);
        }
    });
}

pub type Work = Pin<Box<dyn Future<Output = anyhow::Result<JobResult>> + Send>>;

struct Entry {
    tenant: String,
    /// The user folder of the caller who queued it; only that caller may look it up. Jobs
    /// queued without a user are visible to every caller of the tenant.
    owner: Option<String>,
    info: JobInfo,
    finished: Option<Instant>,
}

struct Queued {
    id: String,
    work: Work,
    done: Option<oneshot::Sender<JobInfo>>,
}

/// A write to a tenant's store, done in order by one task.
enum Save {
    Job(Arc<dyn Store>, Box<StoredJob>),
    /// Removes the jobs that finished before this time (Unix seconds).
    Prune(Arc<dyn Store>, i64),
    /// Answered once every save sent before it is done.
    Flush(oneshot::Sender<()>),
}

#[derive(Default)]
struct Inner {
    jobs: HashMap<String, Entry>,
    queues: HashMap<String, mpsc::UnboundedSender<Queued>>,
    /// The stores of the tenants opened, by folder name, which job status is saved to.
    stores: HashMap<String, Arc<dyn Store>>,
    saves: Option<mpsc::UnboundedSender<Save>>,
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[derive(Clone)]
pub struct Jobs {
    inner: Arc<Mutex<Inner>>,
    keep: Duration,
}

impl Jobs {
    pub fn new(keep_finished: Duration) -> Self {
        Self {
            inner: Arc::default(),
            keep: keep_finished,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queues work on a tenant's queue and returns the job as queued. Must be called inside a
    /// tokio runtime (the queue's task starts on first use).
    pub fn submit(&self, tenant: &str, owner: Option<&str>, kind: &str, work: Work) -> JobInfo {
        self.enqueue(tenant, owner, kind, work, None, false)
    }

    /// Like `submit`, unless a job of this kind for this tenant is queued or running: then
    /// that job is the answer and `work` is dropped.
    pub fn submit_once(&self, tenant: &str, kind: &str, work: Work) -> JobInfo {
        self.enqueue(tenant, None, kind, work, None, true)
    }

    /// Queues work and waits until it has run, for callers that answer with the result.
    pub async fn run(&self, tenant: &str, owner: Option<&str>, kind: &str, work: Work) -> JobInfo {
        let (tx, rx) = oneshot::channel();
        let queued = self.enqueue(tenant, owner, kind, work, Some(tx), false);
        rx.await.unwrap_or(JobInfo {
            status: JobState::Failed,
            error: Some("the job was dropped".into()),
            ..queued
        })
    }

    fn enqueue(
        &self,
        tenant: &str,
        owner: Option<&str>,
        kind: &str,
        work: Work,
        done: Option<oneshot::Sender<JobInfo>>,
        once: bool,
    ) -> JobInfo {
        let mut inner = self.lock();
        self.prune(&mut inner);
        if once
            && let Some(e) = inner.jobs.values().find(|e| {
                e.tenant == tenant
                    && e.info.kind == kind
                    && matches!(e.info.status, JobState::Queued | JobState::Running)
            })
        {
            return e.info.clone();
        }
        let id = format!("j_{}", hex::encode(rand::random::<[u8; 8]>()));
        let info = JobInfo {
            job: id.clone(),
            kind: kind.to_owned(),
            status: JobState::Queued,
            created_at: crate::trace::now_rfc3339(),
            finished_at: None,
            result: None,
            error: None,
            document: None,
            progress: None,
        };
        inner.jobs.insert(
            id.clone(),
            Entry {
                tenant: tenant.to_owned(),
                owner: owner.map(str::to_owned),
                info: info.clone(),
                finished: None,
            },
        );
        self.save(&mut inner, &id);
        let sender = inner
            .queues
            .entry(tenant.to_owned())
            .or_insert_with(|| self.start_queue())
            .clone();
        drop(inner);
        let queued = Queued { id, work, done };
        if let Err(mpsc::error::SendError(q)) = sender.send(queued) {
            // The queue's task is gone (it never stops on its own); fail the job visibly.
            self.finish(
                &q.id,
                Err(anyhow::anyhow!("the write queue stopped")),
                q.done,
            );
        }
        info
    }

    fn start_queue(&self) -> mpsc::UnboundedSender<Queued> {
        let (tx, mut rx) = mpsc::unbounded_channel::<Queued>();
        let jobs = self.clone();
        tokio::spawn(async move {
            while let Some(q) = rx.recv().await {
                jobs.set_running(&q.id);
                let work = CURRENT.scope((jobs.clone(), q.id.clone()), q.work);
                // A panicking job fails alone; the queue carries on.
                let outcome = match tokio::spawn(work).await {
                    Ok(r) => r,
                    Err(e) => Err(anyhow::anyhow!("the job panicked: {e}")),
                };
                jobs.finish(&q.id, outcome, q.done);
            }
        });
        tx
    }

    fn set_running(&self, id: &str) {
        if let Some(e) = self.lock().jobs.get_mut(id) {
            e.info.status = JobState::Running;
        }
    }

    fn finish(
        &self,
        id: &str,
        outcome: anyhow::Result<JobResult>,
        done: Option<oneshot::Sender<JobInfo>>,
    ) {
        let mut inner = self.lock();
        let Some(e) = inner.jobs.get_mut(id) else {
            return;
        };
        e.finished = Some(Instant::now());
        e.info.finished_at = Some(crate::trace::now_rfc3339());
        e.info.progress = None;
        match outcome {
            Ok(r) => {
                e.info.status = JobState::Done;
                e.info.result = Some(r);
            }
            Err(err) => {
                tracing::warn!(job = id, error = %format!("{err:#}"), "job failed");
                e.info.status = JobState::Failed;
                e.info.error = Some(format!("{err:#}"));
            }
        }
        let (info, tenant) = (e.info.clone(), e.tenant.clone());
        self.save(&mut inner, id);
        let keep = i64::try_from(self.keep.as_secs()).unwrap_or(i64::MAX);
        if let Some(store) = inner.stores.get(&tenant).cloned() {
            self.send(
                &mut inner,
                Save::Prune(store, unix_now().saturating_sub(keep)),
            );
        }
        drop(inner);
        if let Some(tx) = done {
            let _ = tx.send(info);
        }
    }

    /// Saves a job's status to its tenant's store, when the tenant was opened with one.
    fn save(&self, inner: &mut Inner, id: &str) {
        let Some(e) = inner.jobs.get(id) else {
            return;
        };
        let Some(store) = inner.stores.get(&e.tenant).cloned() else {
            return;
        };
        let job = StoredJob {
            owner: e.owner.clone(),
            info: e.info.clone(),
            finished: matches!(e.info.status, JobState::Done | JobState::Failed).then(unix_now),
        };
        self.send(inner, Save::Job(store, Box::new(job)));
    }

    fn send(&self, inner: &mut Inner, save: Save) {
        let saves = inner.saves.get_or_insert_with(|| {
            let (tx, mut rx) = mpsc::unbounded_channel::<Save>();
            tokio::spawn(async move {
                while let Some(save) = rx.recv().await {
                    let done = match save {
                        Save::Job(store, job) => store.put_job(&job).await,
                        Save::Prune(store, before) => store.prune_jobs(before).await,
                        Save::Flush(tx) => {
                            let _ = tx.send(());
                            Ok(())
                        }
                    };
                    if let Err(e) = done {
                        // Status still answers from memory; only a restart would lose it.
                        tracing::warn!(error = %format!("{e:#}"), "a job's status wasn't saved");
                    }
                }
            });
            tx
        });
        let _ = saves.send(save);
    }

    /// Waits until every status sent to be saved so far is saved.
    pub async fn flush(&self) {
        let (tx, rx) = oneshot::channel();
        self.send(&mut self.lock(), Save::Flush(tx));
        let _ = rx.await;
    }

    /// Saves the jobs of a tenant just opened to its store from now on, and brings back the
    /// ones saved before: finished ones still kept as they were, and ones that were queued or
    /// running when the service stopped as failed ([`INTERRUPTED`]).
    pub async fn attach(&self, tenant: &str, store: Arc<dyn Store>) {
        self.lock().stores.insert(tenant.to_owned(), store.clone());
        let saved = match store.jobs().await {
            Ok(saved) => saved,
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), tenant, "saved jobs couldn't be read");
                return;
            }
        };
        let (now, keep) = (unix_now(), self.keep.as_secs());
        let mut expired = false;
        let mut inner = self.lock();
        for mut job in saved {
            if inner.jobs.contains_key(&job.info.job) {
                continue;
            }
            let age = match job.finished {
                Some(at) => u64::try_from(now.saturating_sub(at)).unwrap_or(0),
                None => {
                    job.info.status = JobState::Failed;
                    job.info.error = Some(INTERRUPTED.into());
                    job.info.finished_at = Some(crate::trace::now_rfc3339());
                    job.finished = Some(now);
                    self.send(&mut inner, Save::Job(store.clone(), Box::new(job.clone())));
                    0
                }
            };
            if age >= keep {
                expired = true;
                continue;
            }
            let finished = Instant::now()
                .checked_sub(Duration::from_secs(age))
                .unwrap_or_else(Instant::now);
            inner.jobs.insert(
                job.info.job.clone(),
                Entry {
                    tenant: tenant.to_owned(),
                    owner: job.owner,
                    info: job.info,
                    finished: Some(finished),
                },
            );
        }
        if expired {
            let before = now.saturating_sub(i64::try_from(keep).unwrap_or(i64::MAX));
            self.send(&mut inner, Save::Prune(store, before));
        }
    }

    /// Forgets a deleted tenant's jobs and its store, once what was sent to it is saved.
    pub async fn forget_tenant(&self, tenant: &str) {
        {
            let mut inner = self.lock();
            inner.jobs.retain(|_, e| e.tenant != tenant);
            inner.stores.remove(tenant);
        }
        self.flush().await;
    }

    /// Forgets the jobs a deleted user or group folder queued, or whose results name it (its
    /// pages, or gaps an acquisition run found for it), once what was sent to be saved is
    /// saved; the store's own deletion then removes the saved ones.
    pub async fn forget_owner(&self, tenant: &str, owner: &str) {
        self.lock().jobs.retain(|_, e| {
            e.tenant != tenant
                || !(e.owner.as_deref() == Some(owner)
                    || serde_json::to_string(&e.info).is_ok_and(|j| j.contains(owner)))
        });
        self.flush().await;
    }

    /// A job of this tenant that this caller may see; `None` otherwise, as for an unknown id.
    pub fn get(&self, id: &str, tenant: &str, user: Option<&str>) -> Option<JobInfo> {
        self.found(id, tenant, |owner| owner.is_none() || owner == user)
    }

    /// A job of this tenant, whoever queued it: for the service's admin.
    pub fn get_any(&self, id: &str, tenant: &str) -> Option<JobInfo> {
        self.found(id, tenant, |_| true)
    }

    fn found(
        &self,
        id: &str,
        tenant: &str,
        shown: impl Fn(Option<&str>) -> bool,
    ) -> Option<JobInfo> {
        let mut inner = self.lock();
        self.prune(&mut inner);
        let e = inner.jobs.get(id)?;
        (e.tenant == tenant && shown(e.owner.as_deref())).then(|| e.info.clone())
    }

    /// The jobs of this tenant listed to this caller, the newest first: queued, running, and
    /// finished ones still kept. Only the caller's own (a job queued without a user is a
    /// caller's own only for callers without one), and the tenant's lint runs, whose results
    /// are counts. A job queued without a user may name a group's pages; listing it to every
    /// user would show those pages exist, so it stays reachable only by its id.
    pub fn list(&self, tenant: &str, user: Option<&str>) -> Vec<JobInfo> {
        self.listed(tenant, |owner, kind| {
            owner == user || (owner.is_none() && kind == "lint")
        })
    }

    /// Every job of this tenant, the newest first: for the service's admin, who may see all of
    /// it (approvals and filings queued without a user included).
    pub fn list_all(&self, tenant: &str) -> Vec<JobInfo> {
        self.listed(tenant, |_, _| true)
    }

    fn listed(&self, tenant: &str, shown: impl Fn(Option<&str>, &str) -> bool) -> Vec<JobInfo> {
        let mut inner = self.lock();
        self.prune(&mut inner);
        let mut out: Vec<JobInfo> = inner
            .jobs
            .values()
            .filter(|e| e.tenant == tenant && shown(e.owner.as_deref(), &e.info.kind))
            .map(|e| e.info.clone())
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.job.cmp(&a.job)));
        out
    }

    fn prune(&self, inner: &mut Inner) {
        let keep = self.keep;
        inner
            .jobs
            .retain(|_, e| e.finished.is_none_or(|t| t.elapsed() < keep));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_running_job_shows_how_far_it_has_come_until_it_ends() {
        let jobs = Jobs::new(Duration::from_secs(60));
        let (go_on, wait) = oneshot::channel::<()>();
        let (reported, seen) = oneshot::channel::<()>();
        let step = JobProgress {
            lines_read: 120,
            lines_total: 480,
            concepts_filed: 7,
        };
        let info = jobs.submit(
            "t_a",
            None,
            "expertise",
            Box::pin(async move {
                report_progress(step);
                let _ = reported.send(());
                let _ = wait.await;
                Ok(JobResult::default())
            }),
        );
        assert_eq!(info.progress, None);
        seen.await.unwrap();
        let running = jobs.get(&info.job, "t_a", None).unwrap();
        assert_eq!(running.status, JobState::Running);
        assert_eq!(running.progress, Some(step));
        let _ = go_on.send(());
        // Outside a job, reporting does nothing.
        report_progress(step);
        let done = jobs
            .run(
                "t_a",
                None,
                "test",
                Box::pin(async { Ok(JobResult::default()) }),
            )
            .await;
        assert_eq!(done.progress, None);
        let finished = jobs.get(&info.job, "t_a", None).unwrap();
        assert_eq!(finished.status, JobState::Done);
        assert_eq!(finished.progress, None, "gone once the job ends");
    }

    #[tokio::test]
    async fn jobs_of_one_tenant_run_in_order() {
        let jobs = Jobs::new(Duration::from_secs(60));
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut ids = Vec::new();
        for i in 0..5 {
            let log = log.clone();
            let info = jobs.submit(
                "t_a",
                None,
                "test",
                Box::pin(async move {
                    // Later jobs would finish first if they ran alongside.
                    tokio::time::sleep(Duration::from_millis(20 - 4 * i)).await;
                    log.lock().unwrap().push(i);
                    Ok(JobResult::default())
                }),
            );
            assert_eq!(info.status, JobState::Queued);
            ids.push(info.job);
        }
        let last = jobs
            .run(
                "t_a",
                None,
                "test",
                Box::pin(async { anyhow::bail!("boom") }),
            )
            .await;
        assert_eq!(last.status, JobState::Failed);
        assert_eq!(last.error.as_deref(), Some("boom"));
        assert_eq!(*log.lock().unwrap(), vec![0, 1, 2, 3, 4]);
        for id in &ids {
            assert_eq!(jobs.get(id, "t_a", None).unwrap().status, JobState::Done);
            assert!(
                jobs.get(id, "t_b", None).is_none(),
                "other tenants can't see it"
            );
        }
    }

    #[tokio::test]
    async fn owners_and_expiry() {
        let jobs = Jobs::new(Duration::ZERO);
        let info = jobs
            .run(
                "t_a",
                Some("u_1"),
                "test",
                Box::pin(async { Ok(JobResult::default()) }),
            )
            .await;
        assert_eq!(info.status, JobState::Done);
        // Finished jobs are kept for zero time here, so it is already gone.
        assert!(jobs.get(&info.job, "t_a", Some("u_1")).is_none());

        let jobs = Jobs::new(Duration::from_secs(60));
        let info = jobs
            .run(
                "t_a",
                Some("u_1"),
                "test",
                Box::pin(async { panic!("bad job") }),
            )
            .await;
        assert_eq!(info.status, JobState::Failed);
        assert!(jobs.get(&info.job, "t_a", Some("u_1")).is_some());
        assert!(jobs.get(&info.job, "t_a", Some("u_2")).is_none());
        assert!(jobs.get(&info.job, "t_a", None).is_none());
        // The admin sees every job of the tenant, and no other tenant's.
        assert!(jobs.get_any(&info.job, "t_a").is_some());
        assert!(jobs.get_any(&info.job, "t_b").is_none());
        assert!(jobs.list("t_a", Some("u_2")).is_empty());
        assert_eq!(jobs.list_all("t_a").len(), 1);
        assert!(jobs.list_all("t_b").is_empty());
        // The queue survives the panic.
        let next = jobs
            .run(
                "t_a",
                None,
                "test",
                Box::pin(async { Ok(JobResult::default()) }),
            )
            .await;
        assert_eq!(next.status, JobState::Done);
    }
}

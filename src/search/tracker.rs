//! The read tracker: which pages an agent's session has already read, so a second read returns
//! a short notice and search marks those hits `read: true`. Sessions live in memory, belong to
//! one tenant (each tenant has its own tracker) and one user, and expire when idle.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::text::normalize;

/// Session ids longer than this are rejected by the API.
pub const MAX_SESSION_ID_LEN: usize = 128;
/// Open sessions per tenant; past this the least recently used one is dropped.
const MAX_SESSIONS: usize = 10_000;
/// Idle sessions are swept at most this often.
const SWEEP_EVERY: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    user: Option<String>,
    session: String,
}

/// What one session has read.
#[derive(Debug, Clone, Default)]
pub struct ReadState {
    whole: HashSet<String>,
    headings: HashSet<(String, String)>,
    /// Each page read, whole or by a heading, in the order first read.
    order: Vec<String>,
}

impl ReadState {
    /// Whether this page, or this heading of it, has been read. Reading the whole page covers
    /// every heading.
    pub fn has_read(&self, page: &str, heading: Option<&str>) -> bool {
        if self.whole.contains(page) {
            return true;
        }
        heading.is_some_and(|h| {
            self.headings
                .contains(&(page.to_owned(), normalize(h.trim())))
        })
    }

    /// Every page read, whole or by one heading, the most recently first read first.
    pub fn pages(&self) -> Vec<String> {
        self.order.iter().rev().cloned().collect()
    }
}

struct Entry {
    last_used: Instant,
    state: ReadState,
}

struct Inner {
    sessions: HashMap<Key, Entry>,
    last_sweep: Instant,
}

pub struct ReadTracker {
    idle: Duration,
    inner: Mutex<Inner>,
}

impl ReadTracker {
    pub fn new(idle: Duration) -> Self {
        Self {
            idle,
            inner: Mutex::new(Inner {
                sessions: HashMap::new(),
                last_sweep: Instant::now(),
            }),
        }
    }

    fn key(user: Option<&str>, session: &str) -> Key {
        Key {
            user: user.map(str::to_owned),
            session: session.to_owned(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A panic while holding the lock leaves plain data behind; keep using it.
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if now.duration_since(inner.last_sweep) >= SWEEP_EVERY {
            let idle = self.idle;
            inner
                .sessions
                .retain(|_, e| now.duration_since(e.last_used) < idle);
            inner.last_sweep = now;
        }
        inner
    }

    /// Drops every session of a user (the user is being deleted).
    pub fn forget_user(&self, user: &str) {
        self.lock()
            .sessions
            .retain(|k, _| k.user.as_deref() != Some(user));
    }

    /// What a session has read so far; empty for an unknown or expired session.
    pub fn state(&self, user: Option<&str>, session: &str) -> ReadState {
        let mut inner = self.lock();
        let idle = self.idle;
        match inner.sessions.get_mut(&Self::key(user, session)) {
            Some(e) if e.last_used.elapsed() < idle => {
                e.last_used = Instant::now();
                e.state.clone()
            }
            _ => ReadState::default(),
        }
    }

    /// Records a read and says whether it had already happened in this session.
    pub fn record(
        &self,
        user: Option<&str>,
        session: &str,
        page: &str,
        heading: Option<&str>,
    ) -> bool {
        let mut inner = self.lock();
        let key = Self::key(user, session);
        let idle = self.idle;
        if inner
            .sessions
            .get(&key)
            .is_some_and(|e| e.last_used.elapsed() >= idle)
        {
            inner.sessions.remove(&key);
        }
        if !inner.sessions.contains_key(&key) && inner.sessions.len() >= MAX_SESSIONS {
            let oldest = inner
                .sessions
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                inner.sessions.remove(&k);
            }
        }
        let entry = inner.sessions.entry(key).or_insert_with(|| Entry {
            last_used: Instant::now(),
            state: ReadState::default(),
        });
        entry.last_used = Instant::now();
        let state = &mut entry.state;
        let already = state.has_read(page, heading);
        if !state.order.iter().any(|p| p == page) {
            state.order.push(page.to_owned());
        }
        match heading {
            None => {
                state.whole.insert(page.to_owned());
            }
            Some(h) => {
                state
                    .headings
                    .insert((page.to_owned(), normalize(h.trim())));
            }
        }
        already
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_come_most_recently_read_first() {
        let t = ReadTracker::new(Duration::from_secs(1800));
        t.record(None, "s", "expertise/b", None);
        t.record(None, "s", "expertise/a", Some("Steps"));
        t.record(None, "s", "expertise/c", None);
        t.record(None, "s", "expertise/b", Some("Again"));
        assert_eq!(
            t.state(None, "s").pages(),
            vec!["expertise/c", "expertise/a", "expertise/b"]
        );
    }

    #[test]
    fn remembers_reads_per_session_and_user() {
        let t = ReadTracker::new(Duration::from_secs(1800));
        assert!(!t.record(Some("u1"), "s", "expertise/a", Some("Steps")));
        assert!(t.record(Some("u1"), "s", "expertise/a", Some("steps")));
        // A heading doesn't cover the whole page; the whole page covers every heading.
        assert!(!t.record(Some("u1"), "s", "expertise/a", None));
        assert!(t.record(Some("u1"), "s", "expertise/a", Some("Other")));
        // Another user or session starts fresh.
        assert!(!t.record(Some("u2"), "s", "expertise/a", None));
        assert!(!t.record(Some("u1"), "s2", "expertise/a", None));
        assert!(t.state(Some("u1"), "s").has_read("expertise/a", None));
        assert!(!t.state(None, "s").has_read("expertise/a", None));
    }

    #[test]
    fn idle_sessions_expire() {
        let t = ReadTracker::new(Duration::ZERO);
        t.record(None, "s", "expertise/a", None);
        assert!(!t.state(None, "s").has_read("expertise/a", None));
        assert!(!t.record(None, "s", "expertise/a", None));
    }
}

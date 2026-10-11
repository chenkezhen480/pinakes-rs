//! Signing in to the web console (design, "M10": signing in): one account, the service's admin,
//! with a user name and a password. Signing in gives a session token the console keeps and sends
//! as its credential; whoever holds one is the admin (`identity::Admin`), and sees a tenant as a
//! user of it would (`identity::Scope`). The account is `root` / `1234`, or what
//! `PINAKES_ADMIN_USER` / `PINAKES_ADMIN_PASSWORD` say, until it is changed on the console; then
//! it is kept in `<data>/console-login.json` (the password only as an Argon2 hash). The sessions'
//! hashes are kept there too, from the first sign-in, and end when the account in force changes
//! (on the console, or in the environment between two starts). The console asks for `1234` to
//! be changed while it is in force. Failed sign-ins are counted per caller, and at most two
//! passwords are checked at once.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use argon2::Argon2;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub const DEFAULT_USER: &str = "root";
pub const DEFAULT_PASSWORD: &str = "1234";
pub const USER_ENV: &str = "PINAKES_ADMIN_USER";
pub const PASSWORD_ENV: &str = "PINAKES_ADMIN_PASSWORD";
pub const FILE: &str = "console-login.json";
/// Session tokens start with this, so the key header can tell them from API keys and JWTs.
pub const TOKEN_PREFIX: &str = "pnx_session_";
/// How long a session lasts.
const SESSION_DAYS: i64 = 30;
/// Most sessions kept; signing in past it drops the oldest.
const MAX_SESSIONS: usize = 50;
/// Failed sign-ins one caller may make in a minute before more are refused for the rest of it.
/// Counted per caller only: a limit for all callers together would let anyone lock every admin
/// out.
const MAX_FAILURES: usize = 10;
/// Passwords checked at once (each Argon2 check holds about 19 MiB).
const CHECKS_AT_ONCE: usize = 2;

#[derive(Debug, thiserror::Error)]
pub enum SignInError {
    #[error("wrong user name or password")]
    Wrong,
    #[error("too many failed sign-ins; try again in a minute")]
    TooMany,
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

/// Who is signing in, for counting failures: the connection's address.
#[derive(Debug, Clone)]
pub struct Caller {
    pub peer: String,
}

/// The bucket of a signed-in admin's own attempts (a change of password).
const SESSION_BUCKET: &str = "session";

impl Caller {
    /// The signed-in admin, checking the current password before changing it.
    fn session() -> Self {
        Self {
            peer: SESSION_BUCKET.into(),
        }
    }

    fn bucket(&self) -> String {
        self.peer.clone()
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Saved {
    /// The user name and the password's Argon2 hash (PHC string), when changed on the console.
    #[serde(default)]
    user: String,
    #[serde(default)]
    password: String,
    /// Whether that password is `1234`.
    #[serde(default)]
    default_password: bool,
    /// Whether the account was changed on the console; until then it is the environment's or
    /// the default, and only the sessions are kept here.
    #[serde(default)]
    changed: bool,
    /// While the account is the environment's or the default: an Argon2 hash of the user name
    /// and password the sessions were made under, so a change between two starts ends them.
    #[serde(default)]
    env_account: String,
    #[serde(default)]
    sessions: Vec<SavedSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedSession {
    /// SHA-256 of the token, hex.
    hash: String,
    /// Unix seconds.
    expires: i64,
}

/// The account as the Settings page shows it (never the password).
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Account {
    pub user: String,
    /// Whether the password is still `1234`.
    pub default_password: bool,
    /// Where the account comes from: `default`, `environment` or `console` (changed there).
    pub source: String,
}

pub struct Login {
    file: PathBuf,
    /// From the environment, used until the account is changed on the console.
    env_user: Option<String>,
    env_password: Option<String>,
    /// What the file holds, `None` until something is saved.
    saved: Mutex<Option<Saved>>,
    /// One writer of the file at a time (held while writing; `saved` is not).
    writing: Mutex<()>,
    /// Why the file couldn't be read at start, if so: it is never written over then.
    blocked: Option<String>,
    /// When recent sign-ins failed, by caller (`*` for all callers elsewhere).
    failures: Mutex<HashMap<String, VecDeque<Instant>>>,
    /// Turns to check a password.
    checks: Arc<Semaphore>,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

fn digest(s: &str) -> [u8; 32] {
    Sha256::digest(s.as_bytes()).into()
}

/// Constant-time equality of two strings, through their digests.
fn same(a: &str, b: &str) -> bool {
    crate::identity::same_digest(&digest(a), &digest(b))
}

fn hash_password(password: &str) -> anyhow::Result<String> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| anyhow::anyhow!("hashing the password: {e}"))
}

fn check_user_name(user: &str) -> Result<(), SignInError> {
    let ok = !user.is_empty()
        && user.chars().count() <= 64
        && !user.chars().any(|c| c.is_whitespace() || c.is_control());
    if ok {
        Ok(())
    } else {
        Err(SignInError::Invalid(
            "a user name is 1 to 64 characters, without spaces".into(),
        ))
    }
}

/// The account in force: the console's, when changed there, else the environment's or the
/// default.
enum Secret {
    Hash(String),
    Plain(String),
}

/// The file's text, `None` when it isn't there. Another error (a lock held by a scanner, say)
/// is tried three times, a little apart, then given up.
fn read_tried(file: &Path) -> std::io::Result<Option<String>> {
    let mut tries = 0;
    loop {
        match std::fs::read_to_string(file) {
            Ok(t) => return Ok(Some(t)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                tries += 1;
                if tries >= 3 {
                    return Err(e);
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

impl Login {
    /// The account of the service whose data is in `data_dir`, with the environment's user and
    /// password (`None` or empty: the defaults).
    pub fn new(data_dir: &Path, env_user: Option<String>, env_password: Option<String>) -> Self {
        let file = data_dir.join(FILE);
        let mut blocked = None;
        let saved = match read_tried(&file) {
            Ok(None) => None,
            Err(e) => {
                // Unreadable for another reason than not being there (permissions, a lock):
                // never written over, since it may hold the account set on the console.
                tracing::error!(file = %file.display(), error = %e, "the console login file can't be read: signing in can't save until it can (fix it, or remove it, and restart)");
                blocked = Some(e.to_string());
                None
            }
            Ok(Some(text)) => match serde_json::from_str::<Saved>(&text) {
                Ok(s) => Some(s),
                Err(e) => {
                    // Kept aside, never written over: it may hold the account set on the console.
                    let aside = file.with_extension(format!("json.unreadable-{}", now()));
                    let moved = std::fs::rename(&file, &aside);
                    tracing::error!(
                        file = %file.display(),
                        kept_as = %aside.display(),
                        moved = moved.is_ok(),
                        error = %e,
                        "the console login file can't be read: set aside, and the environment's (or the default) account is in force until it is restored"
                    );
                    None
                }
            },
        };
        let filled = |v: Option<String>| v.filter(|s| !s.trim().is_empty());
        let login = Self {
            file,
            env_user: filled(env_user).map(|s| s.trim().to_owned()),
            env_password: filled(env_password),
            saved: Mutex::new(saved),
            writing: Mutex::new(()),
            blocked,
            failures: Mutex::new(HashMap::new()),
            checks: Arc::new(Semaphore::new(CHECKS_AT_ONCE)),
        };
        login.end_stale_sessions();
        login
    }

    /// The environment's account as one string to hash: user name and password.
    fn env_account(&self) -> String {
        let (user, secret) = self.expected(&None);
        match secret {
            Secret::Plain(p) | Secret::Hash(p) => format!("{user}\n{p}"),
        }
    }

    /// Sessions made under another environment account (changed between two starts) end, and
    /// the note of that account goes with them, sessions or none.
    fn end_stale_sessions(&self) {
        let stale = {
            let saved = self.saved.lock().unwrap();
            let Some(s) = saved.as_ref() else { return };
            if s.changed {
                return;
            }
            if s.env_account.is_empty() {
                !s.sessions.is_empty()
            } else {
                Argon2::default()
                    .verify_password(self.env_account().as_bytes(), s.env_account.as_str())
                    .is_err()
            }
        };
        if !stale {
            return;
        }
        match self.update(|s| {
            s.sessions.clear();
            s.env_account.clear();
            Ok(())
        }) {
            Ok(()) => {
                tracing::info!("the console account changed in the environment: its sessions ended")
            }
            Err(e) => {
                tracing::error!(error = %e, "could not end the sessions of the earlier account")
            }
        }
    }

    /// Changes what the file holds: one writer at a time, the file written without holding the
    /// lock `check` takes, and the copy in memory replaced once the file is written.
    fn update<R>(
        &self,
        f: impl FnOnce(&mut Saved) -> Result<R, SignInError>,
    ) -> Result<R, SignInError> {
        let _one = self.writing.lock().unwrap();
        let mut next = self.saved.lock().unwrap().clone().unwrap_or_default();
        let r = f(&mut next)?;
        self.save(&next)?;
        *self.saved.lock().unwrap() = Some(next);
        Ok(r)
    }

    /// The account from the process's environment.
    pub fn from_env(data_dir: &Path) -> Self {
        Self::new(
            data_dir,
            std::env::var(USER_ENV).ok(),
            std::env::var(PASSWORD_ENV).ok(),
        )
    }

    /// A turn to check a password, or `None` when enough are being checked already (the caller
    /// is asked to try again; nothing queues).
    pub fn turn(&self) -> Option<OwnedSemaphorePermit> {
        self.checks.clone().try_acquire_owned().ok()
    }

    fn changed(saved: &Option<Saved>) -> Option<&Saved> {
        saved.as_ref().filter(|s| s.changed)
    }

    fn expected(&self, saved: &Option<Saved>) -> (String, Secret) {
        match Self::changed(saved) {
            Some(s) => (s.user.clone(), Secret::Hash(s.password.clone())),
            None => (
                self.env_user
                    .clone()
                    .unwrap_or_else(|| DEFAULT_USER.to_owned()),
                Secret::Plain(
                    self.env_password
                        .clone()
                        .unwrap_or_else(|| DEFAULT_PASSWORD.to_owned()),
                ),
            ),
        }
    }

    pub fn account(&self) -> Account {
        let saved = self.saved.lock().unwrap();
        let from_env = self.env_user.is_some() || self.env_password.is_some();
        let (default_password, source) = match Self::changed(&saved) {
            Some(s) => (s.default_password, "console"),
            None => (
                self.env_password
                    .as_deref()
                    .is_none_or(|p| p == DEFAULT_PASSWORD),
                if from_env { "environment" } else { "default" },
            ),
        };
        Account {
            user: self.expected(&saved).0,
            default_password,
            source: source.into(),
        }
    }

    /// Whether the password in force is still `1234`.
    pub fn default_password(&self) -> bool {
        self.account().default_password
    }

    fn too_many(&self, caller: &Caller) -> bool {
        let mut f = self.failures.lock().unwrap();
        let minute = Duration::from_secs(60);
        f.retain(|_, q| {
            while q.front().is_some_and(|t| t.elapsed() > minute) {
                q.pop_front();
            }
            !q.is_empty()
        });
        f.get(&caller.bucket()).map_or(0, VecDeque::len) >= MAX_FAILURES
    }

    fn failed(&self, caller: &Caller) {
        let mut f = self.failures.lock().unwrap();
        f.entry(caller.bucket())
            .or_default()
            .push_back(Instant::now());
    }

    /// Which account a check was made against: the console's (by its hash) or the
    /// environment's, so what follows a check can tell whether the account changed meanwhile.
    fn stamp(saved: &Saved) -> String {
        if saved.changed {
            format!("console:{}", saved.password)
        } else {
            "environment".into()
        }
    }

    /// Checks a user name and password against the account in force (slow: Argon2), counting
    /// failures against the caller. Answers which account it was (see [`Login::stamp`]).
    fn verify(&self, user: &str, password: &str, caller: &Caller) -> Result<String, SignInError> {
        if self.too_many(caller) {
            return Err(SignInError::TooMany);
        }
        let saved = self.saved.lock().unwrap().clone();
        let stamp = Self::stamp(&saved.clone().unwrap_or_default());
        let (expected_user, secret) = self.expected(&saved);
        let user_ok = same(user, &expected_user);
        let password_ok = match secret {
            Secret::Hash(h) => Argon2::default()
                .verify_password(password.as_bytes(), h.as_str())
                .is_ok(),
            Secret::Plain(p) => same(password, &p),
        };
        if user_ok && password_ok {
            Ok(stamp)
        } else {
            self.failed(caller);
            Err(SignInError::Wrong)
        }
    }

    /// Signs in. Returns a new session token. Slow (Argon2): call it off the async runtime,
    /// holding a [`Login::turn`].
    pub fn sign_in(
        &self,
        user: &str,
        password: &str,
        caller: &Caller,
    ) -> Result<String, SignInError> {
        let checked = self.verify(user, password, caller)?;
        // Only the session is saved; the account stays the environment's until changed here.
        // The account the session is made under is noted (once, hashed outside the lock).
        let unnoted = self
            .saved
            .lock()
            .unwrap()
            .as_ref()
            .is_none_or(|s| !s.changed && s.env_account.is_empty());
        let noted = if unnoted {
            Some(hash_password(&self.env_account())?)
        } else {
            None
        };
        self.update(|next| {
            // The account changed since the password was checked (changed on the console
            // meanwhile, which ended every session): no session for the old password.
            if Self::stamp(next) != checked {
                return Err(SignInError::Wrong);
            }
            if let Some(h) = noted
                && !next.changed
            {
                next.env_account = h;
            }
            Ok(Self::add_session(next))
        })
    }

    fn add_session(saved: &mut Saved) -> String {
        let token = format!("{TOKEN_PREFIX}{}", hex::encode(rand::random::<[u8; 32]>()));
        let t = now();
        saved.sessions.retain(|s| s.expires > t);
        if saved.sessions.len() >= MAX_SESSIONS {
            let drop = saved.sessions.len() + 1 - MAX_SESSIONS;
            saved.sessions.drain(..drop);
        }
        saved.sessions.push(SavedSession {
            hash: hex::encode(digest(&token)),
            expires: t + SESSION_DAYS * 86_400,
        });
        token
    }

    /// Whether `token` is a live session.
    pub fn check(&self, token: &str) -> bool {
        if !token.starts_with(TOKEN_PREFIX) {
            return false;
        }
        let hash = hex::encode(digest(token));
        let t = now();
        self.saved.lock().unwrap().as_ref().is_some_and(|s| {
            s.sessions
                .iter()
                .any(|x| x.expires > t && same(&x.hash, &hash))
        })
    }

    /// Ends a session; the file is written only when one ended. Blocking (file I/O).
    pub fn sign_out(&self, token: &str) -> anyhow::Result<()> {
        let hash = hex::encode(digest(token));
        let held = self
            .saved
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| s.sessions.iter().any(|x| x.hash == hash));
        if held {
            self.update(|s| {
                s.sessions.retain(|x| x.hash != hash);
                Ok(())
            })?;
        }
        Ok(())
    }

    /// Changes the user name and password, given the current password. Every session ends;
    /// the new one returned keeps the caller signed in. Slow (Argon2), as `sign_in`. Failures
    /// count against the signed-in admin's own bucket.
    pub fn change(&self, current: &str, user: &str, password: &str) -> Result<String, SignInError> {
        let user = user.trim();
        check_user_name(user)?;
        if password.is_empty() {
            return Err(SignInError::Invalid("the new password is empty".into()));
        }
        let admin = Caller::session();
        let current_user = self.expected(&self.saved.lock().unwrap()).0;
        let checked = self.verify(&current_user, current, &admin)?;
        let hashed = hash_password(password)?;
        self.update(|next| {
            // Changed by someone else since the check: their change stands.
            if Self::stamp(next) != checked {
                return Err(SignInError::Wrong);
            }
            *next = Saved {
                user: user.to_owned(),
                password: hashed,
                default_password: password == DEFAULT_PASSWORD,
                changed: true,
                env_account: String::new(),
                sessions: Vec::new(),
            };
            Ok(Self::add_session(next))
        })
    }

    /// Writes the file whole (a temporary file renamed over it); never over a file that
    /// couldn't be read.
    fn save(&self, saved: &Saved) -> anyhow::Result<()> {
        if let Some(why) = &self.blocked {
            anyhow::bail!(
                "the console login file {} couldn't be read at start ({why}): fix it, or remove it, and restart; nothing is written over it until then",
                self.file.display()
            );
        }
        let text = serde_json::to_string_pretty(saved)?;
        let tmp = self.file.with_extension("json.tmp");
        std::fs::write(&tmp, text)
            .and_then(|()| std::fs::rename(&tmp, &self.file))
            .map_err(|e| anyhow::anyhow!("writing {}: {e}", self.file.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrong<T>(r: Result<T, SignInError>) -> bool {
        matches!(r, Err(SignInError::Wrong))
    }

    fn here() -> Caller {
        from("127.0.0.1")
    }

    fn from(peer: &str) -> Caller {
        Caller { peer: peer.into() }
    }

    #[test]
    fn the_default_account_signs_in_from_anywhere_and_changes_on_the_console() {
        let dir = tempfile::tempdir().unwrap();
        let login = Login::new(dir.path(), None, None);
        assert_eq!(login.account().user, "root");
        assert!(login.default_password());
        assert!(wrong(login.sign_in("root", "nope", &here())));
        assert!(wrong(login.sign_in("admin", "1234", &here())));
        // The first sign-in, from wherever the browser is (in Docker, never this machine).
        assert!(login.sign_in("root", "1234", &from("172.17.0.1")).is_ok());
        let token = login.sign_in("root", "1234", &here()).unwrap();
        assert!(login.check(&token));
        assert!(!login.check("pnx_session_00"));
        assert!(!login.check("pnx_0000_1111"));

        // Sessions outlive a restart; the file holds no password or token in the clear.
        let text = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(!text.contains("1234") && !text.contains(&token), "{text}");
        let login = Login::new(dir.path(), None, None);
        assert!(login.check(&token));

        // Changing it needs the current password and ends every session.
        assert!(wrong(login.change("wrong", "ken", "s3cret")));
        let kept = login.change("1234", "ken", "s3cret").unwrap();
        assert!(!login.check(&token));
        assert!(login.check(&kept));
        let account = login.account();
        assert_eq!(
            (
                account.user.as_str(),
                account.default_password,
                account.source.as_str()
            ),
            ("ken", false, "console")
        );
        assert!(login.sign_in("ken", "s3cret", &from("192.0.2.1")).is_ok());
        assert!(wrong(login.sign_in("root", "1234", &here())));

        let other = login.sign_in("ken", "s3cret", &here()).unwrap();
        login.sign_out(&other).unwrap();
        assert!(!login.check(&other));
        assert!(login.check(&kept));
    }

    #[test]
    fn the_environment_sets_the_account_until_it_is_changed() {
        let dir = tempfile::tempdir().unwrap();
        let login = Login::new(dir.path(), Some("ops".into()), Some("from-env".into()));
        let account = login.account();
        assert_eq!(
            (
                account.user.as_str(),
                account.default_password,
                account.source.as_str()
            ),
            ("ops", false, "environment")
        );
        assert!(login.sign_in("ops", "from-env", &from("192.0.2.1")).is_ok());
        assert!(wrong(login.sign_in("root", "1234", &here())));
    }

    #[test]
    fn a_password_set_in_the_environment_after_signing_in_applies() {
        // Signed in once with the default, then PINAKES_ADMIN_PASSWORD set and restarted: the
        // new password is the one in force.
        let dir = tempfile::tempdir().unwrap();
        let token = Login::new(dir.path(), None, None)
            .sign_in("root", "1234", &here())
            .unwrap();
        let login = Login::new(dir.path(), None, Some("s3cret".into()));
        assert!(!login.check(&token), "the old account's session ended");
        let kept = login.sign_in("root", "s3cret", &from("192.0.2.1")).unwrap();
        // A restart with the same account keeps its sessions.
        let login = Login::new(dir.path(), None, Some("s3cret".into()));
        assert!(login.check(&kept));
        // Signed out, then the password changed again: the next sessions are noted under the
        // account they were made under, and survive the restart after.
        login.sign_out(&kept).unwrap();
        let login = Login::new(dir.path(), None, Some("third".into()));
        let third = login.sign_in("root", "third", &here()).unwrap();
        let login = Login::new(dir.path(), None, Some("third".into()));
        assert!(login.check(&third));
        assert!(wrong(login.sign_in("root", "1234", &here())));
        assert!(!login.default_password());
    }

    #[test]
    fn failures_are_limited_per_caller() {
        let dir = tempfile::tempdir().unwrap();
        let login = Login::new(dir.path(), None, Some("s3cret".into()));
        for _ in 0..MAX_FAILURES {
            assert!(wrong(login.sign_in("root", "x", &from("192.0.2.1"))));
        }
        assert!(matches!(
            login.sign_in("root", "s3cret", &from("192.0.2.1")),
            Err(SignInError::TooMany)
        ));
        // Another address, and this machine, still get in, however many addresses fail.
        for i in 0..200 {
            let _ = login.sign_in("root", "x", &from(&format!("198.51.100.{}", i % 250)));
        }
        assert!(login.sign_in("root", "s3cret", &from("192.0.2.2")).is_ok());
        assert!(login.sign_in("root", "s3cret", &here()).is_ok());
        // A signed-in admin's wrong current passwords count in their own bucket, not this
        // machine's.
        for _ in 0..MAX_FAILURES {
            assert!(wrong(login.change("x", "root", "n3w")));
        }
        assert!(login.sign_in("root", "s3cret", &here()).is_ok());
    }

    #[test]
    fn a_file_that_cant_be_read_is_never_written_over() {
        // A directory where the file should be: reading fails, and not for its absence.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(FILE)).unwrap();
        let login = Login::new(dir.path(), None, None);
        assert!(matches!(
            login.sign_in("root", "1234", &here()),
            Err(SignInError::Internal(_))
        ));
        assert!(dir.path().join(FILE).is_dir());
    }

    #[test]
    fn an_unreadable_file_is_set_aside_not_written_over() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), "{ not json").unwrap();
        let login = Login::new(dir.path(), None, None);
        assert!(!dir.path().join(FILE).exists());
        let aside: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("unreadable"))
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(
            std::fs::read_to_string(aside[0].path()).unwrap(),
            "{ not json"
        );
        assert!(login.sign_in("root", "1234", &here()).is_ok());
    }

    #[test]
    fn a_sign_in_checked_before_a_change_gets_no_session() {
        // The check passes against the old account; the account changes before the session is
        // added: the session isn't added (the same guard sign_in runs).
        let dir = tempfile::tempdir().unwrap();
        let login = Login::new(dir.path(), None, None);
        let checked = login.verify("root", "1234", &here()).unwrap();
        login.change("1234", "root", "n3w").unwrap();
        let late = login.update(|next| {
            if Login::stamp(next) != checked {
                return Err(SignInError::Wrong);
            }
            Ok(Login::add_session(next))
        });
        assert!(wrong(late));
        assert_eq!(
            login.saved.lock().unwrap().as_ref().unwrap().sessions.len(),
            1
        );
    }

    #[test]
    fn only_a_few_passwords_are_checked_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let login = Login::new(dir.path(), None, None);
        let turns: Vec<_> = (0..CHECKS_AT_ONCE).map(|_| login.turn().unwrap()).collect();
        assert!(login.turn().is_none());
        drop(turns);
        assert!(login.turn().is_some());
    }
}

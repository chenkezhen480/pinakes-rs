//! Raw records in a tenant's `sources/` folder: saved unchanged when content arrives, never
//! modified, and cited by name in the `sources` of the pages filed from them.

use std::io::{self, Write};
use std::path::Path;

use crate::kinds::Kind;

const MAX_NAME: usize = 128;

/// A new record name: the date, the kind and a random part, such as
/// `2026-10-01-experience-3fa91c0b.json`.
pub fn new_name(kind: Kind, today: &str) -> String {
    format!(
        "{today}-{}-{}.json",
        kind.as_str(),
        hex::encode(rand::random::<[u8; 4]>())
    )
}

/// A new name for a note an agent recorded (`/ingest/note`), such as
/// `2026-10-13-note-3fa91c0b.json`: the librarian splits it into records of their own.
pub fn new_note_name(today: &str) -> String {
    format!(
        "{today}-note-{}.json",
        hex::encode(rand::random::<[u8; 4]>())
    )
}

/// A new name for a page fetched from outside (`crate::acquisition`), such as
/// `2026-10-10-external-3fa91c0b.json`: pages citing it are marked as lower trust.
pub fn new_external_name(today: &str) -> String {
    format!(
        "{today}-{EXTERNAL}-{}.json",
        hex::encode(rand::random::<[u8; 4]>())
    )
}

const EXTERNAL: &str = "external";

/// Whether a cited record (a name, or a citation of its lines) is a page fetched from outside.
pub fn is_external(cited: &str) -> bool {
    let name = cited.split('#').next().unwrap_or(cited);
    name.get(11..)
        .is_some_and(|rest| rest.starts_with(&format!("{EXTERNAL}-")))
}

/// Whether a name can be a file in `sources/`: letters, digits, `-`, `_` and `.`, never starting
/// with a dot or holding `..`. Nothing else is ever joined onto the folder.
fn safe_name(name: &str) -> bool {
    name.len() <= MAX_NAME
        && !name.starts_with('.')
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Whether a name can be a record's file name: a safe name ending in `.json`.
pub fn valid_name(name: &str) -> bool {
    safe_name(name) && name.ends_with(".json")
}

/// Writes a new file in `sources/`; fails rather than overwrite one.
fn write_new(tenant_dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    if !safe_name(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "bad source name",
        ));
    }
    let dir = tenant_dir.join("sources");
    // Not `create_dir_all`: a tenant deleted meanwhile must not get its folder back.
    crate::trace::create_dir(&dir)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(name))?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Saves a record under a new name.
pub fn save(tenant_dir: &Path, name: &str, record: &serde_json::Value) -> io::Result<()> {
    if !valid_name(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "bad source name",
        ));
    }
    let mut text = serde_json::to_string_pretty(record).map_err(io::Error::other)?;
    text.push('\n');
    write_new(tenant_dir, name, text.as_bytes())
}

/// Saves an uploaded file next to its record (`<record>.<ext>`).
pub fn save_file(tenant_dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    write_new(tenant_dir, name, bytes)
}

/// A record's text; `None` when there is no such record.
pub fn read(tenant_dir: &Path, name: &str) -> io::Result<Option<String>> {
    if !valid_name(name) {
        return Ok(None);
    }
    match std::fs::read_to_string(tenant_dir.join("sources").join(name)) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Replaces a record whole, through a temporary file: only for a fetched page whose text may
/// not be kept once filed (`crate::acquisition`); records are otherwise never changed.
pub fn replace(tenant_dir: &Path, name: &str, record: &serde_json::Value) -> io::Result<()> {
    if !valid_name(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "bad source name",
        ));
    }
    let dir = tenant_dir.join("sources");
    let tmp = dir.join(format!("{name}.tmp"));
    {
        let mut file = std::fs::File::create(&tmp)?;
        let mut text = serde_json::to_string_pretty(record).map_err(io::Error::other)?;
        text.push('\n');
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, dir.join(name))
}

/// Deletes one file of `sources/` (an uploaded or fetched original); a missing one is fine.
pub fn remove_file(tenant_dir: &Path, name: &str) -> io::Result<()> {
    if !safe_name(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "bad source name",
        ));
    }
    match std::fs::remove_file(tenant_dir.join("sources").join(name)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Deletes a record and the uploaded file kept next to it (`file.original`), for user and group
/// deletion; answers how many files went.
pub fn remove(tenant_dir: &Path, name: &str) -> io::Result<usize> {
    let original = read(tenant_dir, name)?
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|r| r["file"]["original"].as_str().map(str::to_owned))
        .filter(|o| safe_name(o));
    let mut removed = 0;
    for file in std::iter::once(name).chain(original.as_deref()) {
        if !safe_name(file) {
            continue;
        }
        match std::fs::remove_file(tenant_dir.join("sources").join(file)) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_safe() {
        let name = new_name(Kind::Decision, "2026-10-01");
        assert!(
            name.starts_with("2026-10-01-decision-") && valid_name(&name),
            "{name}"
        );
        for bad in [
            "../x.json",
            ".hidden.json",
            "a/b.json",
            "a\\b.json",
            "x.txt",
            "C:x.json",
            "a..json",
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn saved_once_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let rec = serde_json::json!({ "topic": "退款" });
        save(dir.path(), "r.json", &rec).unwrap();
        assert!(
            save(dir.path(), "r.json", &rec).is_err(),
            "never overwritten"
        );
        let text = read(dir.path(), "r.json").unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            rec
        );
        assert_eq!(read(dir.path(), "missing.json").unwrap(), None);
        assert_eq!(read(dir.path(), "../r.json").unwrap(), None);
    }
}

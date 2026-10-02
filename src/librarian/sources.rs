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

/// Whether a name can be a record's file name: letters, digits, `-`, `_` and `.`, ending in
/// `.json`, never starting with a dot. Nothing else is ever joined onto the folder.
pub fn valid_name(name: &str) -> bool {
    name.len() <= MAX_NAME
        && name.ends_with(".json")
        && !name.starts_with('.')
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Saves a record under a new name; fails rather than overwrite one.
pub fn save(tenant_dir: &Path, name: &str, record: &serde_json::Value) -> io::Result<()> {
    if !valid_name(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "bad source name",
        ));
    }
    let dir = tenant_dir.join("sources");
    std::fs::create_dir_all(&dir)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(name))?;
    let text = serde_json::to_string_pretty(record).map_err(io::Error::other)?;
    file.write_all(text.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()
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

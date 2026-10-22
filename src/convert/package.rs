//! Office files are zip archives. Before anything is read from one, its directory is checked:
//! how many entries, how much they unpack to in all and how much each one is compressed, so a
//! zip bomb is turned away before it is opened. Every read is bounded again by the sizes the
//! directory declares, which a crafted file may understate.

use std::io::{Cursor, Read};

use zip::ZipArchive;

use super::xml::{self, Element};
use super::{Limits, Refused};

/// Entries smaller than this are not checked for their compression ratio: tiny parts of
/// repeated XML compress far better than the limit, harmlessly.
const RATIO_FLOOR: u64 = 1 << 20;

pub struct Package<'a> {
    zip: ZipArchive<Cursor<&'a [u8]>>,
    limits: &'a Limits,
    /// Bytes unpacked so far.
    unpacked: u64,
}

impl<'a> Package<'a> {
    /// Opens the archive after checking its directory against the limits.
    pub fn open(bytes: &'a [u8], limits: &'a Limits) -> Result<Self, Refused> {
        let mut zip = ZipArchive::new(Cursor::new(bytes))
            .map_err(|e| Refused::Unreadable(format!("not a valid Office file (zip): {e}")))?;
        if zip.len() > limits.zip_entries {
            return Err(Refused::TooLarge(format!(
                "the archive has {} entries, more than {}",
                zip.len(),
                limits.zip_entries
            )));
        }
        let mut total = 0u64;
        for i in 0..zip.len() {
            let entry = zip
                .by_index_raw(i)
                .map_err(|e| Refused::Unreadable(format!("a damaged archive entry: {e}")))?;
            if entry.encrypted() {
                return Err(Refused::Unreadable(
                    "the file is password-protected; save it without a password".into(),
                ));
            }
            let size = entry.size();
            total = total.saturating_add(size);
            if size > RATIO_FLOOR && size / entry.compressed_size().max(1) > limits.zip_ratio {
                return Err(Refused::TooLarge(format!(
                    "an archive entry unpacks to {}x its size (a zip bomb?)",
                    size / entry.compressed_size().max(1)
                )));
            }
        }
        if total > limits.unpacked {
            return Err(Refused::TooLarge(format!(
                "the archive unpacks to {} MB, more than {} MB",
                total >> 20,
                limits.unpacked >> 20
            )));
        }
        Ok(Self {
            zip,
            limits,
            unpacked: 0,
        })
    }

    /// An entry's bytes, or `None` when there is no such entry.
    pub fn read(&mut self, name: &str) -> Result<Option<Vec<u8>>, Refused> {
        let name = name.trim_start_matches('/');
        let mut entry = match self.zip.by_name(name) {
            Ok(e) => e,
            Err(zip::result::ZipError::FileNotFound) => return Ok(None),
            Err(e) => return Err(Refused::Unreadable(format!("reading {name}: {e}"))),
        };
        let declared = entry.size();
        let room = self.limits.unpacked.saturating_sub(self.unpacked);
        let mut out = Vec::with_capacity(declared.min(room).min(64 << 20) as usize);
        // One byte more than allowed tells a lying entry from one that fits.
        let cap = declared.min(room).saturating_add(1);
        (&mut entry)
            .take(cap)
            .read_to_end(&mut out)
            .map_err(|e| Refused::Unreadable(format!("reading {name}: {e}")))?;
        if out.len() as u64 > declared {
            return Err(Refused::TooLarge(format!(
                "{name} unpacks to more than its archive entry declares"
            )));
        }
        if out.len() as u64 > room {
            return Err(Refused::TooLarge(format!(
                "the archive unpacks to more than {} MB",
                self.limits.unpacked >> 20
            )));
        }
        self.unpacked += out.len() as u64;
        Ok(Some(out))
    }

    /// An entry parsed as XML, or `None` when there is no such entry.
    pub fn xml(&mut self, name: &str) -> Result<Option<Element>, Refused> {
        match self.read(name)? {
            Some(bytes) => xml::parse(&bytes, self.limits).map(Some),
            None => Ok(None),
        }
    }

    /// Unpacks every entry once, within the limits, and drops what it read: for archives
    /// handed whole to another reader (spreadsheets), so it never meets an entry that unpacks
    /// to more than declared.
    pub fn verify(mut self) -> Result<(), Refused> {
        let names = self
            .zip
            .file_names()
            .map(|n| n.map(|n| n.into_owned()))
            .collect::<Result<Vec<String>, _>>()
            .map_err(|e| Refused::Unreadable(format!("a damaged archive entry name: {e}")))?;
        for name in names {
            if !name.ends_with('/') {
                self.read(&name)?;
            }
        }
        Ok(())
    }
}

/// The targets of a relationships part (`_rels/*.rels`), by id: `(target, external)`.
pub fn relationships(rels: Option<Element>) -> std::collections::HashMap<String, (String, bool)> {
    let mut out = std::collections::HashMap::new();
    let Some(root) = rels else {
        return out;
    };
    let mut found = Vec::new();
    root.find_all("Relationship", &mut found);
    for r in found {
        if let (Some(id), Some(target)) = (r.attr("Id"), r.attr("Target")) {
            let external = r.attr("TargetMode") == Some("External");
            out.insert(id.to_owned(), (target.to_owned(), external));
        }
    }
    out
}

/// A relationship target as a part name: relative to the folder of the part it belongs to.
pub fn resolve(base_dir: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_owned();
    }
    let mut parts: Vec<&str> = base_dir.split('/').filter(|p| !p.is_empty()).collect();
    for seg in target.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    /// A zip archive of these entries, deflated.
    pub fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        let mut w = zip::ZipWriter::new(&mut out);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(bytes).unwrap();
        }
        w.finish().unwrap();
        out.into_inner()
    }

    #[test]
    fn a_zip_bomb_is_turned_away_before_it_is_unpacked() {
        let zeros = vec![0u8; 50 << 20];
        let bomb = zip_of(&[("word/document.xml", &zeros)]);
        assert!(bomb.len() < 1 << 20, "{}", bomb.len());
        let limits = Limits::default();
        assert!(matches!(
            Package::open(&bomb, &limits),
            Err(Refused::TooLarge(why)) if why.contains("zip bomb")
        ));

        // Many entries, or too much in all, are refused as well.
        let small = Limits {
            zip_entries: 3,
            ..Limits::default()
        };
        let many = zip_of(&[("a", b"1"), ("b", b"2"), ("c", b"3"), ("d", b"4")]);
        assert!(matches!(
            Package::open(&many, &small),
            Err(Refused::TooLarge(_))
        ));
        let tight = Limits {
            unpacked: 10,
            ..Limits::default()
        };
        let big = zip_of(&[("a", b"0123456789abcdef")]);
        assert!(matches!(
            Package::open(&big, &tight),
            Err(Refused::TooLarge(_))
        ));
    }

    #[test]
    fn relationship_targets_resolve_against_their_part() {
        assert_eq!(
            resolve("ppt/slides", "../notesSlides/notesSlide1.xml"),
            "ppt/notesSlides/notesSlide1.xml"
        );
        assert_eq!(resolve("ppt", "slides/slide1.xml"), "ppt/slides/slide1.xml");
        assert_eq!(resolve("word", "/word/media/a.png"), "word/media/a.png");
    }
}

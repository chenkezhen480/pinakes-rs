//! The edits the librarian plans, and the checks code runs on each before anything is written
//! (design, "The librarian", step 5): the page is of the job's kind and in its audience's
//! folder, required fields are there, anchors are allowed, and each kind's merge rule holds.
//! Code, not the model, writes `kind`, `sources` and `updated`.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::kinds::{AudienceLevel, Kind};
use crate::search::catalog::{Fence, heading_of, links};
use crate::search::text::normalize;
use crate::wiki::{Audience, FrontMatter, Page, PagePath, WikiError, WikiFs};

const MAX_TITLE: usize = 200;
const MAX_SUMMARY: usize = 600;
const MAX_TERM: usize = 100;
const MAX_TERMS: usize = 30;
const MAX_ANCHORS: usize = 30;
const MAX_BODY: usize = 100_000;

/// One edit. `create`, `update` and `supersede` come from the librarian's plan; `replace` is a
/// hand edit through `PUT /pages`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Edit {
    Create {
        page: String,
        title: String,
        summary: String,
        #[serde(default)]
        aliases: Vec<String>,
        #[serde(default)]
        keywords: Vec<String>,
        #[serde(default)]
        about: Vec<String>,
        body: String,
    },
    Update {
        page: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        heading: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        add_aliases: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        add_keywords: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        add_about: Vec<String>,
    },
    Supersede {
        page: String,
        claim: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replacement: Option<String>,
        reason: String,
    },
    Replace {
        page: String,
        /// The version (`Page::version`) the editor read, or none for a new page; the edit is
        /// refused when the page is no longer at it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base: Option<String>,
        front_matter: FrontMatter,
        body: String,
    },
}

impl Edit {
    /// The page the edit writes.
    pub fn page(&self) -> String {
        match self {
            Edit::Create { page, .. }
            | Edit::Update { page, .. }
            | Edit::Supersede { page, .. }
            | Edit::Replace { page, .. } => page.clone(),
        }
    }
}

/// Who a page is for, with group and user folders as names: `tenant`, `g_…` or `u_…`, the
/// same keys the search index uses.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PageAudience {
    Tenant,
    Group(String),
    User(String),
}

impl PageAudience {
    pub fn of(path: &PagePath) -> Self {
        match path.audience() {
            Audience::Tenant => PageAudience::Tenant,
            Audience::Group(g) => PageAudience::Group(g.to_owned()),
            Audience::User(u) => PageAudience::User(u.to_owned()),
        }
    }

    pub fn key(&self) -> &str {
        match self {
            PageAudience::Tenant => "tenant",
            PageAudience::Group(d) | PageAudience::User(d) => d,
        }
    }

    pub fn from_key(key: &str) -> Self {
        if key.starts_with("g_") {
            PageAudience::Group(key.to_owned())
        } else if key.starts_with("u_") {
            PageAudience::User(key.to_owned())
        } else {
            PageAudience::Tenant
        }
    }

    fn level(&self) -> AudienceLevel {
        match self {
            PageAudience::Tenant => AudienceLevel::Tenant,
            PageAudience::Group(_) => AudienceLevel::Group,
            PageAudience::User(_) => AudienceLevel::User,
        }
    }
}

/// Where one job may write: pages of one kind, for one audience.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub kind: Kind,
    pub audience: PageAudience,
}

impl Target {
    /// Fails when the kind can't have that audience (`Kind::audiences`).
    pub fn new(kind: Kind, audience: PageAudience) -> Result<Self, String> {
        if !kind.audiences().contains(&audience.level()) {
            return Err(format!(
                "{} pages can't be for that audience",
                kind.as_str()
            ));
        }
        Ok(Self { kind, audience })
    }

    /// The folder new pages go in; for preferences, the one profile page.
    pub fn folder(&self) -> String {
        let sub = match self.kind {
            Kind::Experience => "experience",
            Kind::Expertise => "expertise",
            Kind::Decision => "decisions",
            Kind::Graph => "graph",
            Kind::Preference => "preferences",
        };
        match &self.audience {
            PageAudience::Tenant => sub.to_owned(),
            PageAudience::Group(g) => format!("groups/{g}/{sub}"),
            PageAudience::User(u) => format!("users/{u}/{sub}"),
        }
    }

    pub fn allows(&self, path: &PagePath) -> bool {
        path.kind() == Some(self.kind) && PageAudience::of(path) == self.audience
    }
}

/// Says whether a page (second argument) may anchor to an anchor (first), given the pages the
/// plan creates (third).
pub type AnchorCheck<'a> = dyn Fn(&str, &PagePath, &HashSet<PagePath>) -> Result<(), String> + 'a;

/// What the checks need besides the edit: the target, today's date, the raw record to cite,
/// and the anchor check. `anchor(a, page, created)` says whether page `page` may anchor to `a`,
/// where `created` holds the pages this plan creates.
pub struct Rules<'a> {
    pub target: &'a Target,
    pub today: &'a str,
    pub source: Option<&'a str>,
    pub anchor: &'a AnchorCheck<'a>,
    /// Whether links an edit adds must lead to pages that exist (or that the plan creates): for
    /// the librarian's plans, which would otherwise link ahead under ids guessed for pages a
    /// later window writes. A person's hand edit isn't held to it.
    pub links: bool,
}

/// Pages as a plan changes them, over the wiki as it stands. Each edit applies whole or not
/// at all.
pub struct Draft<'a> {
    wiki: &'a WikiFs,
    pages: BTreeMap<PagePath, Option<Page>>,
    created: HashSet<PagePath>,
    order: Vec<PagePath>,
}

/// A page an edit wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub page: PagePath,
    pub created: bool,
}

impl<'a> Draft<'a> {
    pub fn new(wiki: &'a WikiFs) -> Self {
        Self {
            wiki,
            pages: BTreeMap::new(),
            created: HashSet::new(),
            order: Vec::new(),
        }
    }

    fn current(&mut self, path: &PagePath) -> anyhow::Result<Result<Option<Page>, String>> {
        if let Some(p) = self.pages.get(path) {
            return Ok(Ok(p.clone()));
        }
        let page = match self.wiki.read(path) {
            Ok(p) => Some(p),
            Err(WikiError::NotFound) => None,
            Err(WikiError::Malformed { reason, .. }) => {
                return Ok(Err(format!("page {path} can't be read: {reason}")));
            }
            Err(e) => return Err(e.into()),
        };
        self.pages.insert(path.clone(), page.clone());
        Ok(Ok(page))
    }

    /// Checks one edit and applies it to the draft. The outer error is a failure of the
    /// service (disk); the inner one says why the edit was refused.
    pub fn apply(&mut self, edit: &Edit, r: &Rules) -> anyhow::Result<Result<Applied, String>> {
        let page_id = edit.page();
        let path = match PagePath::parse(&page_id) {
            Ok(p) => p,
            Err(_) => return Ok(Err(format!("{page_id:?} is not a valid page id"))),
        };
        if !r.target.allows(&path) {
            return Ok(Err(format!(
                "{path} is outside {}/, where this job writes",
                r.target.folder()
            )));
        }
        let current = match self.current(&path)? {
            Ok(c) => c,
            Err(e) => return Ok(Err(e)),
        };
        let mut created = self.created.clone();
        if current.is_none() && matches!(edit, Edit::Create { .. } | Edit::Replace { .. }) {
            created.insert(path.clone());
        }
        let linked_before: HashSet<String> = current
            .as_ref()
            .map(|c| links(&c.body).into_iter().collect())
            .unwrap_or_default();
        let result = match edit {
            Edit::Create {
                title,
                summary,
                aliases,
                keywords,
                about,
                body,
                ..
            } => {
                if current.is_some() {
                    return Ok(Err(format!("{path} already exists; update it instead")));
                }
                create(
                    &path, title, summary, aliases, keywords, about, body, r, &created,
                )
            }
            Edit::Update {
                heading,
                text,
                summary,
                add_aliases,
                add_keywords,
                add_about,
                ..
            } => match current {
                None => Err(format!("{path} does not exist")),
                Some(page) => update(
                    page,
                    heading.as_deref(),
                    text.as_deref(),
                    summary.as_deref(),
                    add_aliases,
                    add_keywords,
                    add_about,
                    r,
                    &created,
                ),
            },
            Edit::Supersede {
                claim,
                replacement,
                reason,
                ..
            } => match current {
                None => Err(format!("{path} does not exist")),
                Some(page) => supersede(page, claim, replacement.as_deref(), reason, r),
            },
            Edit::Replace {
                base,
                front_matter,
                body,
                ..
            } => replace(
                &path,
                current,
                base.as_deref(),
                front_matter,
                body,
                r,
                &created,
            ),
        };
        let page = match result {
            Ok(p) => p,
            Err(e) => return Ok(Err(e)),
        };
        // Links the edit adds must lead somewhere every reader of the page can go: a link to
        // a page a later step might write (under a name guessed now) would dangle.
        for link in links(&page.body).into_iter().filter(|_| r.links) {
            if link == path.as_str() || linked_before.contains(&link) {
                continue;
            }
            // A link names what an anchor may: a page id, `schema:type` or
            // `space:type/key`, by the same rule.
            if let Err(e) = (r.anchor)(&link, &path, &created) {
                return Ok(Err(format!(
                    "link [[{link}]]: {e}; link only to pages that exist (the related pages, \
                     the ledger's pages, or one this plan creates)"
                )));
            }
        }
        let was_new = created.contains(&path);
        self.created = created;
        if !self.order.contains(&path) {
            self.order.push(path.clone());
        }
        self.pages.insert(path.clone(), Some(page));
        Ok(Ok(Applied {
            page: path,
            created: was_new,
        }))
    }

    /// The pages the plan wrote, in the order first written, each with whether it is new.
    pub fn into_pages(mut self) -> Vec<(Page, bool)> {
        let order = std::mem::take(&mut self.order);
        order
            .into_iter()
            .filter_map(|p| {
                let created = self.created.contains(&p);
                self.pages.remove(&p).flatten().map(|page| (page, created))
            })
            .collect()
    }
}

fn check_text(what: &str, s: &str, max: usize) -> Result<String, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err(format!("{what} is empty"));
    }
    if s.chars().count() > max {
        return Err(format!("{what} is longer than {max} characters"));
    }
    Ok(s.to_owned())
}

/// Adds terms without duplicates (compared normalized), checking each.
fn add_terms(list: &mut Vec<String>, add: &[String], what: &str) -> Result<(), String> {
    for t in add {
        let t = check_text(what, t, MAX_TERM)?;
        if !list.iter().any(|x| normalize(x) == normalize(&t)) {
            list.push(t);
        }
    }
    if list.len() > MAX_TERMS {
        return Err(format!("more than {MAX_TERMS} {what}s"));
    }
    Ok(())
}

fn add_anchors(
    list: &mut Vec<String>,
    add: &[String],
    page: &PagePath,
    r: &Rules,
    created: &HashSet<PagePath>,
) -> Result<(), String> {
    for a in add {
        let a = a.trim();
        // A page about itself says nothing; models add it now and then, so it is dropped.
        if list.iter().any(|x| x == a) || a == page.as_str() {
            continue;
        }
        (r.anchor)(a, page, created).map_err(|e| format!("anchor {a:?}: {e}"))?;
        list.push(a.to_owned());
    }
    if list.len() > MAX_ANCHORS {
        return Err(format!("more than {MAX_ANCHORS} anchors"));
    }
    Ok(())
}

/// Code-owned fields, and the anchor every experience and decision page needs (a preference page
/// is its user's, by its folder: its anchors are optional).
fn finish(page: &mut Page, r: &Rules) -> Result<(), String> {
    page.front.kind = Some(r.target.kind);
    page.front.updated = Some(r.today.to_owned());
    if let Some(source) = r.source
        && !page.front.sources.iter().any(|s| s == source)
    {
        page.front.sources.push(source.to_owned());
    }
    let anchored = matches!(r.target.kind, Kind::Experience | Kind::Decision);
    if anchored && page.front.about.is_empty() {
        return Err("an experience or decision page needs at least one anchor in about".into());
    }
    if page.body.chars().count() > MAX_BODY {
        return Err(format!(
            "the page body is longer than {MAX_BODY} characters"
        ));
    }
    if !page.body.ends_with('\n') {
        page.body.push('\n');
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn create(
    path: &PagePath,
    title: &str,
    summary: &str,
    aliases: &[String],
    keywords: &[String],
    about: &[String],
    body: &str,
    r: &Rules,
    created: &HashSet<PagePath>,
) -> Result<Page, String> {
    let mut page = Page::new(path.clone());
    let f = &mut page.front;
    f.title = Some(check_text("title", title, MAX_TITLE)?);
    f.summary = Some(check_text("summary", summary, MAX_SUMMARY)?);
    add_terms(&mut f.aliases, aliases, "alias")?;
    add_terms(&mut f.keywords, keywords, "keyword")?;
    add_anchors(&mut page.front.about, about, path, r, created)?;
    page.body = check_text("body", body, MAX_BODY)?;
    finish(&mut page, r)?;
    Ok(page)
}

#[allow(clippy::too_many_arguments)]
fn update(
    mut page: Page,
    heading: Option<&str>,
    text: Option<&str>,
    summary: Option<&str>,
    add_aliases: &[String],
    add_keywords: &[String],
    add_about: &[String],
    r: &Rules,
    created: &HashSet<PagePath>,
) -> Result<Page, String> {
    match (heading, text) {
        (Some(h), Some(t)) => {
            let h = h.trim().trim_start_matches('#').trim();
            if h.is_empty() || h.contains('\n') {
                return Err("heading must be one line of text".into());
            }
            let existed = find_section(&page.body, h).is_some();
            if existed && r.target.kind == Kind::Decision {
                return Err(format!(
                    "decision pages never overwrite past entries; add the decision as a new \
                     section instead of changing {h:?}"
                ));
            }
            page.body = put_section(&page.body, h, t.trim_end());
        }
        (None, None) => {}
        _ => return Err("heading and text go together".into()),
    }
    if let Some(s) = summary {
        page.front.summary = Some(check_text("summary", s, MAX_SUMMARY)?);
    }
    add_terms(&mut page.front.aliases, add_aliases, "alias")?;
    add_terms(&mut page.front.keywords, add_keywords, "keyword")?;
    let path = page.path.clone();
    add_anchors(&mut page.front.about, add_about, &path, r, created)?;
    finish(&mut page, r)?;
    Ok(page)
}

fn supersede(
    mut page: Page,
    claim: &str,
    replacement: Option<&str>,
    reason: &str,
    r: &Rules,
) -> Result<Page, String> {
    if r.target.kind == Kind::Decision {
        return Err("decision pages never overwrite past entries; add a new section".into());
    }
    let claim = claim.trim();
    let reason = check_text("reason", reason, MAX_SUMMARY)?;
    if claim.is_empty() || claim.contains('\n') {
        return Err("claim must be one line copied from the page".into());
    }
    // Models often copy a list item with its marker; the claim is the text after it, and only
    // that is struck, so a list item stays a list item.
    let bare = strip_marker(claim);
    let Some((at, claim)) = find_claim(&page.body, bare)
        .map(|at| (at, bare))
        .or_else(|| find_claim(&page.body, claim).map(|at| (at, claim)))
    else {
        return Err(format!("claim {claim:?} is not on the page"));
    };
    let mut marked = format!("~~{claim}~~ (superseded {}: {reason})", r.today);
    if let Some(rep) = replacement.map(str::trim).filter(|s| !s.is_empty()) {
        marked.push_str(&format!(" {rep}"));
    }
    page.body.replace_range(at..at + claim.len(), &marked);
    finish(&mut page, r)?;
    Ok(page)
}

/// Where a claim is in a body: a whole line (after its list marker) if one says exactly that,
/// else the first place in a line that says it outside struck text. Headings and code blocks
/// are never claims.
fn find_claim(body: &str, claim: &str) -> Option<usize> {
    if claim.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    let mut fence = Fence::default();
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let t = line.trim();
        if fence.step(t) || heading_of(t).is_some() {
            continue;
        }
        lines.push((start, line.trim_end_matches(['\n', '\r'])));
    }
    let whole = lines.iter().find_map(|(start, line)| {
        let text = strip_marker(line).trim_end();
        (text == claim).then(|| start + (text.as_ptr() as usize - line.as_ptr() as usize))
    });
    whole.or_else(|| {
        lines.iter().find_map(|(start, line)| {
            let struck = struck_spans(line);
            line.match_indices(claim)
                .map(|(i, _)| i)
                .find(|&i| !struck.iter().any(|&(a, b)| i < b && i + claim.len() > a))
                .map(|i| start + i)
        })
    })
}

/// The byte ranges of `~~struck~~` text in a line.
fn struck_spans(line: &str) -> Vec<(usize, usize)> {
    let marks: Vec<usize> = line.match_indices("~~").map(|(i, _)| i).collect();
    marks
        .chunks(2)
        .filter(|c| c.len() == 2)
        .map(|c| (c[0], c[1] + 2))
        .collect()
}

/// A line without a leading list marker (`- `, `* `, `+ `, `1. `).
fn strip_marker(line: &str) -> &str {
    let t = line.trim_start();
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(m) {
            return rest.trim_start();
        }
    }
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0
        && let Some(rest) = t[digits..].strip_prefix(". ")
    {
        return rest.trim_start();
    }
    t
}

fn replace(
    path: &PagePath,
    current: Option<Page>,
    base: Option<&str>,
    front: &FrontMatter,
    body: &str,
    r: &Rules,
    created: &HashSet<PagePath>,
) -> Result<Page, String> {
    match (&current, base) {
        (Some(c), Some(b)) if c.version() == b => {}
        (None, None) => {}
        (Some(c), _) => {
            return Err(format!(
                "{path} changed since it was read (now at version {}); read it again and \
                 redo the edit",
                c.version()
            ));
        }
        (None, Some(_)) => return Err(format!("{path} no longer exists")),
    }
    // A hand edit is held to the decision rule too: past entries stay as they are.
    if r.target.kind == Kind::Decision
        && let Some(c) = &current
        && !body.trim_end().starts_with(c.body.trim_end())
    {
        return Err(
            "decision pages never overwrite past entries: keep the page as it is and add new \
             sections after it"
                .into(),
        );
    }
    let mut page = Page::new(path.clone());
    page.front = front.clone();
    page.body = body.to_owned();
    let f = &mut page.front;
    f.title = Some(check_text(
        "title",
        f.title.as_deref().unwrap_or(""),
        MAX_TITLE,
    )?);
    f.summary = Some(check_text(
        "summary",
        f.summary.as_deref().unwrap_or(""),
        MAX_SUMMARY,
    )?);
    let (aliases, keywords) = (
        std::mem::take(&mut f.aliases),
        std::mem::take(&mut f.keywords),
    );
    add_terms(&mut f.aliases, &aliases, "alias")?;
    add_terms(&mut f.keywords, &keywords, "keyword")?;
    // Sources are what the service recorded; a hand edit can't change them.
    f.sources = current
        .as_ref()
        .map(|c| c.front.sources.clone())
        .unwrap_or_default();
    // Anchors the page already had stay; new ones are checked.
    let old: Vec<String> = current.map(|c| c.front.about).unwrap_or_default();
    let wanted = std::mem::take(&mut f.about);
    let mut about: Vec<String> = wanted.iter().filter(|a| old.contains(a)).cloned().collect();
    let new: Vec<String> = wanted.into_iter().filter(|a| !old.contains(a)).collect();
    add_anchors(&mut about, &new, path, r, created)?;
    page.front.about = about;
    finish(&mut page, r)?;
    Ok(page)
}

/// The line range of the section under `heading` (compared normalized, at any level): from the
/// heading line to the next heading of the same or a higher level. Headings in code blocks
/// don't count.
pub fn find_section(body: &str, heading: &str) -> Option<(usize, usize)> {
    let wanted = normalize(heading);
    let lines: Vec<&str> = body.lines().collect();
    let mut fence = Fence::default();
    let mut start: Option<(usize, usize)> = None;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if fence.step(t) {
            continue;
        }
        let Some((level, text)) = heading_of(t) else {
            continue;
        };
        match start {
            None if normalize(text) == wanted => start = Some((i, level)),
            Some((s, l)) if level <= l => return Some((s, i)),
            _ => {}
        }
    }
    start.map(|(s, _)| (s, lines.len()))
}

/// The body with the section under `heading` replaced by `text`, keeping its heading line; or,
/// when there is no such section, with a new `## heading` section added at the end.
pub fn put_section(body: &str, heading: &str, text: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    match find_section(body, heading) {
        Some((start, end)) => {
            let mut out: Vec<String> = lines[..=start].iter().map(|s| (*s).to_owned()).collect();
            out.push(String::new());
            out.push(text.to_owned());
            if end < lines.len() {
                out.push(String::new());
                out.extend(lines[end..].iter().map(|s| (*s).to_owned()));
            }
            let mut s = out.join("\n");
            s.push('\n');
            s
        }
        None => {
            let mut s = body.trim_end().to_owned();
            if !s.is_empty() {
                s.push_str("\n\n");
            }
            s.push_str(&format!("## {heading}\n\n{text}\n"));
            s
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const U: &str = "u_0123456789abcdef";

    fn wiki() -> (tempfile::TempDir, WikiFs) {
        let dir = tempfile::tempdir().unwrap();
        let wiki = WikiFs::new(dir.path());
        (dir, wiki)
    }

    fn anchors_ok(a: &str, _: &PagePath, _: &HashSet<PagePath>) -> Result<(), String> {
        if a.starts_with("bad") {
            Err("unknown".into())
        } else {
            Ok(())
        }
    }

    fn rules(target: &Target) -> Rules<'_> {
        Rules {
            target,
            today: "2026-10-01",
            source: Some("2026-10-01-experience-0a1b2c3d.json"),
            anchor: &anchors_ok,
            links: true,
        }
    }

    fn create_edit(page: &str, about: &[&str]) -> Edit {
        Edit::Create {
            page: page.into(),
            title: "Refunds".into(),
            summary: "How to refund.".into(),
            aliases: vec!["refund".into(), "Refund".into()],
            keywords: vec![],
            about: about.iter().map(|s| (*s).to_owned()).collect(),
            body: "# Refunds\n\n## Steps\n\n1. Check.".into(),
        }
    }

    #[test]
    fn sections_are_replaced_or_added() {
        let body = "# T\n\nintro\n\n## Steps\n\n1. a\n\n### Sub\n\nx\n\n## Pitfalls\n\n- p\n";
        let out = put_section(body, "steps", "1. b");
        assert_eq!(
            out,
            "# T\n\nintro\n\n## Steps\n\n1. b\n\n## Pitfalls\n\n- p\n"
        );
        let out = put_section(body, "Pitfalls", "- q");
        assert!(out.ends_with("## Pitfalls\n\n- q\n"), "{out}");
        let out = put_section(body, "Notes", "n");
        assert!(out.ends_with("- p\n\n## Notes\n\nn\n"), "{out}");
        let code = "## A\n\n```\n## B\n```\n";
        assert_eq!(find_section(code, "B"), None);
    }

    #[test]
    fn a_page_never_anchors_to_itself() {
        let (_d, wiki) = wiki();
        let target = Target::new(Kind::Experience, PageAudience::Tenant).unwrap();
        let r = rules(&target);
        let mut draft = Draft::new(&wiki);
        draft
            .apply(
                &create_edit(
                    "experience/refunds",
                    &["experience/refunds", "commerce:order"],
                ),
                &r,
            )
            .unwrap()
            .unwrap();
        let (page, _) = draft.into_pages().pop().unwrap();
        assert_eq!(page.front.about, vec!["commerce:order"]);
    }

    #[test]
    fn create_checks_folder_fields_and_anchors() {
        let (_d, wiki) = wiki();
        let target = Target::new(Kind::Experience, PageAudience::Tenant).unwrap();
        let r = rules(&target);
        let mut draft = Draft::new(&wiki);
        let bad = [
            create_edit("expertise/refunds", &["commerce:order"]),
            create_edit(&format!("users/{U}/decisions/x"), &["commerce:order"]),
            create_edit("experience/refunds", &[]),
            create_edit("experience/refunds", &["bad:anchor"]),
            create_edit("experience/../x", &["commerce:order"]),
        ];
        for e in &bad {
            assert!(draft.apply(e, &r).unwrap().is_err(), "{e:?}");
        }
        let ok = draft
            .apply(&create_edit("experience/refunds", &["commerce:order"]), &r)
            .unwrap()
            .unwrap();
        assert!(ok.created);
        // A second create of the same page is refused; an update of it works.
        assert!(
            draft
                .apply(&create_edit("experience/refunds", &["commerce:order"]), &r)
                .unwrap()
                .is_err()
        );
        let upd = Edit::Update {
            page: "experience/refunds".into(),
            heading: Some("Pitfalls".into()),
            text: Some("- Seen once.".into()),
            summary: None,
            add_aliases: vec![],
            add_keywords: vec![],
            add_about: vec![],
        };
        draft.apply(&upd, &r).unwrap().unwrap();
        let pages = draft.into_pages();
        assert_eq!(pages.len(), 1);
        let (page, created) = &pages[0];
        assert!(created);
        assert_eq!(page.front.kind, Some(Kind::Experience));
        assert_eq!(page.front.aliases, vec!["refund"]);
        assert_eq!(page.front.updated.as_deref(), Some("2026-10-01"));
        assert_eq!(
            page.front.sources,
            vec!["2026-10-01-experience-0a1b2c3d.json"]
        );
        assert!(page.body.contains("## Pitfalls\n\n- Seen once.\n"));
    }

    #[test]
    fn decisions_are_append_only() {
        let (_d, wiki) = wiki();
        let path = PagePath::parse(&format!("users/{U}/decisions/refunds")).unwrap();
        let mut page = Page::new(path.clone());
        page.front.title = Some("Refunds".into());
        page.front.about = vec!["commerce:order".into()];
        page.body = "# Refunds\n\n## 2026-09-01: refund shipping\n\nYes.\n".into();
        wiki.write(&page).unwrap();
        let target = Target::new(Kind::Decision, PageAudience::User(U.to_owned())).unwrap();
        let r = rules(&target);
        let mut draft = Draft::new(&wiki);
        let rewrite = Edit::Update {
            page: path.to_string(),
            heading: Some("2026-09-01: refund shipping".into()),
            text: Some("No.".into()),
            summary: None,
            add_aliases: vec![],
            add_keywords: vec![],
            add_about: vec![],
        };
        assert!(draft.apply(&rewrite, &r).unwrap().is_err());
        let strike = Edit::Supersede {
            page: path.to_string(),
            claim: "Yes.".into(),
            replacement: None,
            reason: "changed".into(),
        };
        assert!(draft.apply(&strike, &r).unwrap().is_err());
        let append = Edit::Update {
            page: path.to_string(),
            heading: Some("2026-10-01: stop refunding shipping".into()),
            text: Some("Reverses the decision of 2026-09-01.".into()),
            summary: Some("Shipping is no longer refunded.".into()),
            add_aliases: vec![],
            add_keywords: vec![],
            add_about: vec![],
        };
        let applied = draft.apply(&append, &r).unwrap().unwrap();
        assert!(!applied.created);
        // A hand edit too: it may add after the page, never change what is there.
        let mut front = page.front.clone();
        front.summary = Some("Refund decisions.".into());
        let hand = |body: &str| Edit::Replace {
            page: path.to_string(),
            base: Some(page.version()),
            front_matter: front.clone(),
            body: body.into(),
        };
        let mut fresh = Draft::new(&wiki);
        let rewrite = hand("# Refunds\n\n## 2026-09-01: refund shipping\n\nNo.\n");
        assert!(fresh.apply(&rewrite, &r).unwrap().is_err());
        let added = format!("{}\n## 2026-10-02: note\n\nAdded.\n", page.body);
        fresh.apply(&hand(&added), &r).unwrap().unwrap();
        let (page, _) = draft.into_pages().pop().unwrap();
        assert!(
            page.body
                .contains("## 2026-09-01: refund shipping\n\nYes.\n")
        );
        assert!(page.body.contains("Reverses the decision"));
    }

    #[test]
    fn supersede_marks_the_claim() {
        let (_d, wiki) = wiki();
        let path = PagePath::parse("expertise/vat").unwrap();
        let mut page = Page::new(path.clone());
        page.front.title = Some("VAT".into());
        page.body = "# VAT\n\n- The rate is 13%.\n".into();
        wiki.write(&page).unwrap();
        let target = Target::new(Kind::Expertise, PageAudience::Tenant).unwrap();
        let r = rules(&target);
        let mut draft = Draft::new(&wiki);
        let missing = Edit::Supersede {
            page: "expertise/vat".into(),
            claim: "The rate is 9%.".into(),
            replacement: None,
            reason: "x".into(),
        };
        assert!(draft.apply(&missing, &r).unwrap().is_err());
        let ok = Edit::Supersede {
            page: "expertise/vat".into(),
            // Copied with its list marker.
            claim: "- The rate is 13%.".into(),
            replacement: Some("The rate is 9% from 2026-10-01.".into()),
            reason: "new law".into(),
        };
        draft.apply(&ok, &r).unwrap().unwrap();
        let (page, _) = draft.into_pages().pop().unwrap();
        assert!(page.body.contains(
            "~~The rate is 13%.~~ (superseded 2026-10-01: new law) The rate is 9% from 2026-10-01."
        ));
    }

    #[test]
    fn links_must_lead_to_pages_that_exist() {
        assert_eq!(
            links(
                "See [[expertise/a]] and [[expertise/b|B]], [[expertise/a#x]].\n```\n[[no]]\n```\n"
            ),
            vec!["expertise/a", "expertise/b"]
        );
        let (_d, wiki) = wiki();
        let target = Target::new(Kind::Expertise, PageAudience::Tenant).unwrap();
        let exists = |a: &str, _: &PagePath, created: &HashSet<PagePath>| {
            if a == "expertise/known" || created.iter().any(|p| p.as_str() == a) {
                Ok(())
            } else {
                Err("no such page".to_owned())
            }
        };
        let r = Rules {
            target: &target,
            today: "2026-10-01",
            source: None,
            anchor: &exists,
            links: true,
        };
        let page = |name: &str, body: &str| Edit::Create {
            page: format!("expertise/{name}"),
            title: "T".into(),
            summary: "S.".into(),
            aliases: vec![],
            keywords: vec![],
            about: vec![],
            body: body.into(),
        };
        let mut draft = Draft::new(&wiki);
        let guessed = draft
            .apply(&page("a", "See [[expertise/later]]."), &r)
            .unwrap()
            .unwrap_err();
        assert!(guessed.contains("[[expertise/later]]"), "{guessed}");
        assert!(
            draft
                .apply(&page("a", "See [[a title]]."), &r)
                .unwrap()
                .is_err()
        );
        draft
            .apply(
                &page("a", "See [[expertise/known]] and [[expertise/a]]."),
                &r,
            )
            .unwrap()
            .unwrap();
        // A page this plan creates can be linked.
        draft
            .apply(&page("b", "After [[expertise/a]]."), &r)
            .unwrap()
            .unwrap();
        // A person's edit isn't held to it.
        let hand = Rules { links: false, ..r };
        draft
            .apply(&page("c", "See [[a title]]."), &hand)
            .unwrap()
            .unwrap();
    }

    #[test]
    fn supersede_finds_the_live_claim() {
        let body = "# The rate is 13%.\n\n\
                    - ~~The rate is 13%.~~ (superseded 2026-09-01: x) The rate is 9%.\n\
                    - Shops say the rate is 13%. for food\n\
                    - The rate is 13%.\n";
        // The whole line, not the heading, the struck text or a longer line.
        let at = find_claim(body, "The rate is 13%.").unwrap();
        assert_eq!(&body[at - 2..at], "- ");
        assert!(body[at..].starts_with("The rate is 13%.\n"));
        // Part of a line, outside struck text.
        let at = find_claim(body, "rate is 13%. for").unwrap();
        assert!(body[..at].ends_with("Shops say the "));
        assert_eq!(find_claim("~~a b~~ (superseded)\n", "a b"), None);
    }

    #[test]
    fn targets_follow_kind_audiences() {
        assert!(Target::new(Kind::Preference, PageAudience::Tenant).is_err());
        assert!(Target::new(Kind::Decision, PageAudience::Tenant).is_err());
        let t = Target::new(Kind::Preference, PageAudience::User(U.into())).unwrap();
        assert_eq!(t.folder(), format!("users/{U}/preferences"));
        assert!(t.allows(&PagePath::parse(&format!("users/{U}/preferences")).unwrap()));
        let g = Target::new(
            Kind::Experience,
            PageAudience::Group("g_0123456789abcdef".into()),
        )
        .unwrap();
        assert_eq!(g.folder(), "groups/g_0123456789abcdef/experience");
        assert!(!g.allows(&PagePath::parse("experience/x").unwrap()));
        assert_eq!(PageAudience::from_key(U), PageAudience::User(U.into()));
        assert_eq!(PageAudience::from_key("tenant"), PageAudience::Tenant);
    }
}

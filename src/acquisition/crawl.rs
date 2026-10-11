//! Reading the tenant's trusted sites into a local index of candidate pages (design, "Filling
//! gaps from outside"). Each site is read the way a well-behaved crawler reads it: its
//! `robots.txt` says what may be read and where its sitemaps are; the sitemaps, the addresses
//! the allowlist names, and the links of pages read lead to more pages of the same site, up to
//! a number of pages per site, one request at a time with a pause between. No search engine is
//! asked: what a tenant is missing never leaves the service. A page is turned into Markdown by
//! the converter and kept only as what matching needs (title, headings, terms and a vector),
//! never its text; a page chosen for a gap is fetched again when it is proposed.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use reqwest::Url;
use serde::{Deserialize, Serialize};

use super::Settings;
use super::web::{self, Fetcher};
use crate::AppState;
use crate::search::text::{RunKind, is_stopword, normalize, runs, word_parts};
use crate::tenants::Tenant;

/// The longest pause between two requests a site's `Crawl-delay` is followed up to.
const MAX_CRAWL_DELAY: Duration = Duration::from_secs(60);
/// Most sitemaps read per site (a sitemap index can name many).
const MAX_SITEMAPS: usize = 20;
/// Most terms kept per page.
const MAX_TERMS: usize = 3000;
const MAX_HEADINGS: usize = 40;
/// How much of a page its vector is made from.
const VECTOR_CHARS: usize = 2000;

/// A page of a trusted site, as the crawl last saw it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrawledPage {
    pub url: String,
    /// The allowlist domain it was read under.
    pub site: String,
    pub title: String,
    pub headings: Vec<String>,
    /// Normalized words, and pairs of Chinese characters.
    pub terms: Vec<String>,
    /// From its title, headings and opening, by the tenant's embedding model, when one is
    /// loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector: Option<Vec<f32>>,
    /// A hash of its Markdown, to tell when it changed.
    pub hash: String,
    pub fetched_at: String,
}

/// What a crawl did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct CrawlReport {
    pub at: String,
    /// The allowlist it read, so a changed one is read again at once.
    pub sites: Vec<String>,
    /// Pages read and kept.
    pub pages: usize,
    /// Pages whose reading failed (the site, the converter, a type that can't be read).
    pub failed: usize,
    /// Addresses `robots.txt` asked not to read.
    pub disallowed: usize,
    /// Pages no longer found, taken out of the index.
    pub removed: usize,
}

/// A `robots.txt` group: the agents it names, its (allow, pattern) rules and its
/// `Crawl-delay`.
type Group = (Vec<String>, Vec<(bool, String)>, Option<Duration>);

/// The rules of one `robots.txt` for this service, and the sitemaps it names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Robots {
    /// (allow, pattern) of the group for this service, or for every agent.
    rules: Vec<(bool, String)>,
    pub sitemaps: Vec<String>,
    /// The pause the group asks for between two requests, if it asks (at most
    /// [`MAX_CRAWL_DELAY`]).
    pub delay: Option<Duration>,
}

impl Robots {
    /// Reads a `robots.txt`: the group naming `agent` (by the start of its name, any case),
    /// else the group for `*`; `Sitemap` lines wherever they are.
    pub fn parse(text: &str, agent: &str) -> Self {
        let agent = agent.to_ascii_lowercase();
        let mut groups: Vec<Group> = Vec::new();
        let mut sitemaps = Vec::new();
        let mut taking_agents = false;
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let (key, value) = (key.trim().to_ascii_lowercase(), value.trim().to_owned());
            match key.as_str() {
                "user-agent" => {
                    if !taking_agents {
                        groups.push((Vec::new(), Vec::new(), None));
                    }
                    taking_agents = true;
                    if let Some(g) = groups.last_mut() {
                        g.0.push(value.to_ascii_lowercase());
                    }
                }
                "allow" | "disallow" => {
                    taking_agents = false;
                    if let Some(g) = groups.last_mut()
                        && !value.is_empty()
                    {
                        g.1.push((key == "allow", value));
                    }
                }
                "sitemap" => sitemaps.push(value),
                "crawl-delay" => {
                    taking_agents = false;
                    if let (Some(g), Ok(secs)) = (groups.last_mut(), value.parse::<f64>())
                        && secs.is_finite()
                        && secs > 0.0
                    {
                        g.2 = Some(Duration::from_secs_f64(secs.min(60.0)));
                    }
                }
                _ => taking_agents = false,
            }
        }
        let ours = |a: &String| a != "*" && agent.starts_with(a.as_str());
        let group = groups
            .iter()
            .find(|(agents, _, _)| agents.iter().any(ours))
            .or_else(|| {
                groups
                    .iter()
                    .find(|(agents, _, _)| agents.iter().any(|a| a == "*"))
            });
        Self {
            rules: group.map(|g| g.1.clone()).unwrap_or_default(),
            sitemaps,
            delay: group.and_then(|g| g.2).map(|d| d.min(MAX_CRAWL_DELAY)),
        }
    }

    /// Whether a path (with its query) may be read: the longest matching rule decides, and an
    /// `Allow` wins a tie.
    pub fn allows(&self, path: &str) -> bool {
        let mut best: Option<(usize, bool)> = None;
        for (allow, pattern) in &self.rules {
            if pattern_matches(pattern, path) {
                let len = pattern.len();
                let better = match best {
                    None => true,
                    Some((l, a)) => len > l || (len == l && *allow && !a),
                };
                if better {
                    best = Some((len, *allow));
                }
            }
        }
        best.is_none_or(|(_, allow)| allow)
    }
}

/// A `robots.txt` pattern: a path prefix where `*` stands for anything and a final `$` ends it.
fn pattern_matches(pattern: &str, path: &str) -> bool {
    let (pattern, anchored) = match pattern.strip_suffix('$') {
        Some(p) => (p, true),
        None => (pattern, false),
    };
    let parts: Vec<&str> = pattern.split('*').collect();
    let last = parts.len() - 1;
    let mut at = 0;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            if !path.starts_with(part) {
                return false;
            }
            at = part.len();
            if last == 0 {
                return !anchored || at == path.len();
            }
            continue;
        }
        if i == last && anchored {
            // The last part ends the path, wherever the parts before it ended.
            return path.len() >= at + part.len() && path.ends_with(part);
        }
        match path[at..].find(part) {
            Some(j) => at += j + part.len(),
            None => return false,
        }
    }
    true
}

/// The `<loc>` addresses of a sitemap, and whether it is an index of sitemaps.
pub fn sitemap_locs(xml: &str) -> (bool, Vec<String>) {
    let index = xml.contains("<sitemapindex");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<loc>") {
        rest = &rest[i + 5..];
        let Some(j) = rest.find("</loc>") else {
            break;
        };
        let loc = rest[..j]
            .trim()
            .trim_start_matches("<![CDATA[")
            .trim_end_matches("]]>")
            .replace("&amp;", "&")
            .replace("&apos;", "'")
            .replace("&quot;", "\"")
            .replace("&lt;", "<")
            .replace("&gt;", ">");
        out.push(loc);
        rest = &rest[j + 6..];
    }
    (index, out)
}

/// The addresses a page's Markdown links to, with those `found` outside it (an HTML page's
/// navigation, which its Markdown leaves out), made absolute against the page's own, without
/// fragments; files that aren't pages (images, styles, scripts, archives) left out.
pub fn links_with(markdown: &str, mut found: Vec<String>, base: &Url) -> Vec<Url> {
    let mut rest = markdown;
    while let Some(i) = rest.find("](") {
        rest = &rest[i + 2..];
        let end = rest
            .find(|c: char| c == ')' || c.is_whitespace())
            .unwrap_or(rest.len());
        found.push(rest[..end].trim_matches(['<', '>']).to_owned());
        rest = &rest[end..];
    }
    let mut rest = markdown;
    while let Some(i) = rest.find("<http") {
        rest = &rest[i + 1..];
        let end = rest.find('>').unwrap_or(rest.len());
        found.push(rest[..end].to_owned());
        rest = &rest[end..];
    }
    const NOT_PAGES: [&str; 14] = [
        ".png", ".jpg", ".jpeg", ".gif", ".svg", ".webp", ".ico", ".css", ".js", ".zip", ".gz",
        ".mp4", ".mp3", ".woff2",
    ];
    let mut out: Vec<Url> = Vec::new();
    for link in found {
        let Ok(mut url) = base.join(&link) else {
            continue;
        };
        if !matches!(url.scheme(), "http" | "https") {
            continue;
        }
        url.set_fragment(None);
        let path = url.path().to_ascii_lowercase();
        if NOT_PAGES.iter().any(|e| path.ends_with(e)) {
            continue;
        }
        if !out.contains(&url) {
            out.push(url);
        }
    }
    out
}

/// The terms a text is matched by: normalized words of two letters or more (stop words left
/// out), and every pair of adjacent Chinese characters.
pub fn terms(text: &str) -> Vec<String> {
    let text = normalize(text);
    let mut out: BTreeSet<String> = BTreeSet::new();
    for run in runs(&text) {
        match run.kind {
            RunKind::Word => {
                for w in word_parts(run.text) {
                    if w.chars().count() >= 2 && !is_stopword(w) {
                        out.insert(w.to_owned());
                    }
                }
            }
            RunKind::Cjk => {
                let chars: Vec<char> = run.text.chars().collect();
                for pair in chars.windows(2) {
                    out.insert(pair.iter().collect());
                }
            }
        }
    }
    out.into_iter().take(MAX_TERMS).collect()
}

/// A page's title (its first top heading, else its first line) and its other headings.
fn outline(markdown: &str, url: &str) -> (String, Vec<String>) {
    let cut = |s: &str, n: usize| s.trim().chars().take(n).collect::<String>();
    // Top headings that are only a link are a site's banner or logo, the same on every page.
    let mut top: Vec<String> = Vec::new();
    let mut banner = None;
    let mut headings = Vec::new();
    for line in markdown.lines().map(str::trim) {
        let level = line.chars().take_while(|c| *c == '#').count();
        if level == 0 || !line[level..].starts_with(' ') {
            continue;
        }
        let raw = line[level..].trim();
        let text = cut(&plain(raw), 200);
        if text.is_empty() {
            continue;
        }
        if level == 1 {
            if only_a_link(raw) {
                banner.get_or_insert(text);
            } else {
                top.push(text);
            }
        } else if headings.len() < MAX_HEADINGS {
            headings.push(cut(&text, 120));
        }
    }
    let mut top = top.into_iter();
    let title = top
        .next()
        .or_else(|| headings.first().cloned())
        .or(banner)
        .or_else(|| {
            markdown
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(|l| cut(&plain(l), 200))
        })
        .unwrap_or_else(|| url.to_owned());
    // Other top headings are headings too, first.
    let mut all: Vec<String> = top.collect();
    all.extend(headings);
    all.truncate(MAX_HEADINGS);
    (title, all)
}

/// Markdown text without its link and image syntax: `[text](url)` is `text`, an image nothing.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('[') {
        let image = rest[..i].ends_with('!');
        out.push_str(&rest[..if image { i - 1 } else { i }]);
        let after = &rest[i + 1..];
        match after.find("](") {
            Some(j) => {
                let label = &after[..j];
                let tail = &after[j + 2..];
                let end = tail.find(')').map_or(tail.len(), |k| k + 1);
                if !image {
                    out.push_str(label);
                }
                rest = &tail[end..];
            }
            None => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_owned()
}

/// Whether a heading is nothing but one link.
fn only_a_link(heading: &str) -> bool {
    let h = heading.trim();
    h.starts_with('[') && h.ends_with(')') && h.find("](").is_some_and(|i| !h[..i].contains(']'))
}

/// An origin's rules: those of its `robots.txt`, nothing closed when it has none, everything
/// closed when it can't be read (the site is down or failing: read it another time).
async fn robots_of(fetcher: &Fetcher, origin: &str) -> Robots {
    match fetcher.robots_txt(origin).await {
        Some(text) => Robots::parse(&text, web::USER_AGENT),
        None => Robots {
            rules: vec![(false, "/".to_owned())],
            sitemaps: Vec::new(),
            delay: None,
        },
    }
}

/// `scheme://host[:port]` of an address.
fn origin(url: &Url) -> String {
    url.origin().ascii_serialization()
}

fn hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(&Sha256::digest(text.as_bytes())[..8])
}

/// Reads every trusted site into the index (see the module's description), replacing what an
/// earlier crawl kept of it; pages of sites no longer on the allowlist leave the index.
#[tracing::instrument(name = "acquisition_crawl", skip_all, fields(tenant = %tenant.dir_name))]
pub async fn crawl(
    app: &AppState,
    tenant: &Arc<Tenant>,
    settings: &Settings,
) -> anyhow::Result<CrawlReport> {
    let cfg = &app.config.acquisition;
    let domains = settings.domains();
    let fetcher = Fetcher::new(&domains, cfg.allow_private_hosts).map_err(anyhow::Error::msg)?;
    let delay = Duration::from_millis(cfg.crawl_delay_ms);
    let embedder = app.embedder.clone().filter(|e| e.ready());
    let mut report = CrawlReport {
        at: crate::trace::now_rfc3339(),
        sites: domains.clone(),
        ..CrawlReport::default()
    };
    let known: HashMap<String, CrawledPage> = tenant
        .store
        .crawled_pages()
        .await?
        .into_iter()
        .map(|p| (p.url.clone(), p))
        .collect();
    let mut gone: Vec<String> = known
        .values()
        .filter(|p| !domains.contains(&p.site))
        .map(|p| p.url.clone())
        .collect();
    let mut first_request = true;
    let mut pause = || {
        let wait = !std::mem::replace(&mut first_request, false);
        async move {
            if wait && !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
        }
    };

    for source in &settings.sources {
        let site = std::slice::from_ref(&source.domain);
        let mine = |u: &Url| u.host_str().is_some_and(|h| web::allowed(h, site));
        // Where the site is read from: the start addresses it lists, else its home over HTTPS.
        let mut origins: BTreeSet<String> = BTreeSet::new();
        let mut frontier: VecDeque<Url> = VecDeque::new();
        for u in &source.urls {
            if let Ok(url) = Url::parse(u)
                && mine(&url)
            {
                origins.insert(origin(&url));
                frontier.push_back(url);
            }
        }
        if origins.is_empty() {
            origins.insert(format!("https://{}", source.domain));
        }
        // Each origin's rules, and its sitemaps' pages.
        let mut robots: HashMap<String, Robots> = HashMap::new();
        for o in &origins {
            pause().await;
            let r = robots_of(&fetcher, o).await;
            // A site that is closed (or down) isn't read further, its sitemaps included.
            let mut maps: VecDeque<String> = if !r.allows("/") {
                VecDeque::new()
            } else if r.sitemaps.is_empty() {
                VecDeque::from([format!("{o}/sitemap.xml")])
            } else {
                r.sitemaps.iter().cloned().collect()
            };
            let mut read = 0;
            while let Some(map) = maps.pop_front() {
                if read >= MAX_SITEMAPS {
                    break;
                }
                read += 1;
                pause().await;
                let Ok(raw) = fetcher.get(&map).await else {
                    continue;
                };
                let (index, locs) = sitemap_locs(&String::from_utf8_lossy(&raw.bytes));
                for loc in locs {
                    if index {
                        maps.push_back(loc);
                    } else if let Ok(url) = Url::parse(&loc)
                        && mine(&url)
                    {
                        frontier.push_back(url);
                    }
                }
            }
            robots.insert(o.clone(), r);
        }
        if frontier.is_empty() {
            for o in &origins {
                if let Ok(url) = Url::parse(&format!("{o}/")) {
                    frontier.push_back(url);
                }
            }
        }

        let mut seen: HashSet<String> = HashSet::new();
        let mut queued: HashSet<String> = frontier.iter().map(Url::to_string).collect();
        let mut kept: Vec<CrawledPage> = Vec::new();
        let mut capped = false;
        // Pages that fail count too: a site of broken links isn't read without end.
        let mut tries = 0;
        while let Some(mut url) = frontier.pop_front() {
            if kept.len() >= cfg.crawl_pages_per_site || tries >= 2 * cfg.crawl_pages_per_site {
                capped = true;
                break;
            }
            url.set_fragment(None);
            if !seen.insert(url.to_string()) || !mine(&url) || fetcher.refuse(&url).is_some() {
                continue;
            }
            let o = origin(&url);
            if !robots.contains_key(&o) {
                pause().await;
                let r = robots_of(&fetcher, &o).await;
                robots.insert(o.clone(), r);
            }
            let path = match url.query() {
                Some(q) => format!("{}?{q}", url.path()),
                None => url.path().to_owned(),
            };
            if !robots[&o].allows(&path) {
                report.disallowed += 1;
                continue;
            }
            pause().await;
            // A site asking for a longer pause than ours gets it.
            if let Some(extra) = robots[&o].delay.and_then(|d| d.checked_sub(delay)) {
                tokio::time::sleep(extra).await;
            }
            tries += 1;
            let fetched = match fetcher.page(url.as_str()).await {
                Ok(f) => f,
                Err(why) => {
                    tracing::debug!(url = %url, why, "a page of a trusted site wasn't read");
                    report.failed += 1;
                    continue;
                }
            };
            // A page is kept under the address it lands on; one reached again through a
            // redirect (`www.` to the bare domain) is read once.
            let mut landed = Url::parse(&fetched.url).unwrap_or_else(|_| url.clone());
            landed.set_fragment(None);
            if landed != url && !seen.insert(landed.to_string()) {
                continue;
            }
            let (markdown, found) = match app.converter.convert(&fetched.ext, fetched.bytes).await {
                Ok(c) => (c.markdown, c.links),
                Err(e) => {
                    tracing::debug!(url = %url, error = %e, "a page of a trusted site wasn't converted");
                    report.failed += 1;
                    continue;
                }
            };
            for link in links_with(&markdown, found, &landed) {
                if mine(&link) && !seen.contains(link.as_str()) && queued.insert(link.to_string()) {
                    frontier.push_back(link);
                }
            }
            let address = landed.to_string();
            let h = hash(&markdown);
            let page = match known.get(&address).filter(|p| p.hash == h) {
                Some(p) => CrawledPage {
                    fetched_at: crate::trace::now_rfc3339(),
                    site: source.domain.clone(),
                    ..p.clone()
                },
                None => {
                    let (title, headings) = outline(&markdown, &address);
                    let mut lead = format!("{title}\n{}\n", headings.join("\n"));
                    lead.extend(markdown.chars().take(VECTOR_CHARS));
                    let vector = match &embedder {
                        Some(e) => {
                            let e = e.clone();
                            tokio::task::spawn_blocking(move || e.embed(&[lead]))
                                .await
                                .ok()
                                .and_then(Result::ok)
                                .and_then(|mut v| v.pop())
                        }
                        None => None,
                    };
                    CrawledPage {
                        url: address.clone(),
                        site: source.domain.clone(),
                        title,
                        headings,
                        terms: terms(&markdown),
                        vector,
                        hash: h,
                        fetched_at: crate::trace::now_rfc3339(),
                    }
                }
            };
            kept.push(page);
        }
        report.pages += kept.len();
        tenant.store.put_crawled_pages(&kept).await?;
        // A site read whole: what wasn't found any more leaves the index. Only when something
        // was read: a site down for now keeps its pages until it is back.
        if !capped && !kept.is_empty() {
            let now: HashSet<&str> = kept.iter().map(|p| p.url.as_str()).collect();
            gone.extend(
                known
                    .values()
                    .filter(|p| p.site == source.domain && !now.contains(p.url.as_str()))
                    .map(|p| p.url.clone()),
            );
        }
    }
    report.removed = gone.len();
    tenant.store.delete_crawled_pages(&gone).await?;
    tracing::info!(
        pages = report.pages,
        failed = report.failed,
        disallowed = report.disallowed,
        "trusted sites read"
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn robots_rules_for_this_service() {
        let text = "User-agent: Googlebot\nDisallow: /\n\nUser-agent: *\nDisallow: /private/\nAllow: /private/open\nDisallow: /*.pdf$\n# a comment\nSitemap: https://a.com/sitemap.xml\n";
        let r = Robots::parse(text, "pinakes/0.1.0");
        assert!(r.allows("/docs/a"));
        assert!(!r.allows("/private/x"));
        assert!(r.allows("/private/open/page"));
        assert!(!r.allows("/files/manual.pdf"));
        assert!(r.allows("/files/manual.pdf?x=1"));
        assert!(!r.allows("/a.pdf.d/b.pdf"), "the last .pdf ends it");
        assert!(pattern_matches("/x$", "/x") && !pattern_matches("/x$", "/xy"));
        assert!(pattern_matches("/a*$", "/abc"));
        assert_eq!(r.sitemaps, vec!["https://a.com/sitemap.xml"]);
        assert_eq!(r.delay, None);
        let slow = Robots::parse(
            "User-agent: *\nCrawl-delay: 2.5\nDisallow: /x\nUser-agent: other\nCrawl-delay: 9\n",
            "pinakes/0.1.0",
        );
        assert_eq!(slow.delay, Some(Duration::from_millis(2500)));
        assert!(!slow.allows("/x"));
        assert_eq!(
            Robots::parse("User-agent: *\nCrawl-delay: 3600\n", "pinakes").delay,
            Some(MAX_CRAWL_DELAY)
        );
        let ours = Robots::parse(
            "User-agent: pinakes\nDisallow: /\nUser-agent: *\nAllow: /\n",
            "pinakes/0.1.0",
        );
        assert!(!ours.allows("/anything"));
        assert!(Robots::default().allows("/anything"));
    }

    #[test]
    fn sitemaps_and_their_indexes() {
        let (index, locs) = sitemap_locs(
            "<urlset><url><loc> https://a.com/x?a=1&amp;b=2 </loc></url><url><loc>https://a.com/y</loc></url></urlset>",
        );
        assert!(!index);
        assert_eq!(locs, vec!["https://a.com/x?a=1&b=2", "https://a.com/y"]);
        let (index, locs) = sitemap_locs(
            "<sitemapindex><sitemap><loc>https://a.com/s1.xml</loc></sitemap></sitemapindex>",
        );
        assert!(index);
        assert_eq!(locs, vec!["https://a.com/s1.xml"]);
    }

    #[test]
    fn links_are_made_absolute_and_pages_only() {
        let base = Url::parse("https://a.com/docs/one").unwrap();
        let md = "See [two](two#part) and [home](/), ![logo](/logo.png), <https://b.org/x> and [mail](mailto:x@a.com).";
        let got: Vec<String> = links_with(md, vec!["/nav".into()], &base)
            .iter()
            .map(Url::to_string)
            .collect();
        assert_eq!(
            got,
            vec![
                "https://a.com/nav",
                "https://a.com/docs/two",
                "https://a.com/",
                "https://b.org/x"
            ]
        );
    }

    #[test]
    fn terms_are_words_and_chinese_pairs() {
        let t = terms("跨境退货 Returns policy of the shop");
        for want in ["跨境", "境退", "退货", "returns", "policy", "shop"] {
            assert!(t.contains(&want.to_string()), "{want} in {t:?}");
        }
        assert!(!t.contains(&"the".to_string()));
        let (title, headings) = outline("intro\n# Title\n## A\n### B\n", "u");
        assert_eq!(
            (title.as_str(), headings),
            ("Title", vec!["A".into(), "B".into()])
        );
        // A banner heading that is only a link isn't the page's title.
        let page = "# [菜鸟教程 -- 学的不仅是技术](/)\n![logo](/l.png)\n## [HTML 基础](/html)\n# HTML 基础- 4个实例\n## HTML 标题\n";
        let (title, headings) = outline(page, "u");
        assert_eq!(title, "HTML 基础- 4个实例");
        assert_eq!(headings, vec!["HTML 基础".to_string(), "HTML 标题".into()]);
        assert_eq!(
            plain("see ![x](/a.png)[the rules](/r) now"),
            "see the rules now"
        );
    }
}

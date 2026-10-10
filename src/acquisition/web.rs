//! Fetching pages from the tenant's trusted sites, and nowhere else (design, "Filling gaps
//! from outside"). A page is fetched only from a host on the allowlist, redirects included,
//! and never from this machine or a private network: names are resolved by a resolver that
//! drops such addresses, so an allowlist can't reach the service's own network. No search
//! engine or other third party is asked anything. The service never parses what it fetches;
//! the converter turns pages into Markdown, as it does uploads.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::Url;

/// The largest page fetched.
pub const MAX_FETCH: usize = 5 * 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REDIRECTS: usize = 5;
/// How the service names itself to the sites it reads (and in `robots.txt` groups).
pub const USER_AGENT: &str = concat!("pinakes/", env!("CARGO_PKG_VERSION"));

/// Whether a host is on an allowlist: the domain itself or one of its subdomains.
/// A `www.` entry also takes the bare domain itself, the same site (many redirect one to the
/// other), but not the bare domain's other subdomains.
pub fn allowed(host: &str, domains: &[String]) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    domains.iter().any(|d| {
        let d = d.trim_end_matches('.').to_ascii_lowercase();
        host == d
            || host.ends_with(&format!(".{d}"))
            || d.strip_prefix("www.").is_some_and(|bare| host == bare)
    })
}

/// Whether an address is on the public internet: not this machine, a private or shared
/// network, link-local, multicast or reserved.
pub fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (64..128).contains(&o[1]))
                || o[0] >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return public(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80)
        }
    }
}

/// Resolves names to public addresses only.
struct PublicOnly;

impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| public(a.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(format!("{host} has no public address").into());
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// A fetched page.
#[derive(Debug, Clone)]
pub struct Fetched {
    /// Where it was found, after redirects.
    pub url: String,
    /// The extension the converter takes it as (`html`, `pdf`, …), or `md`/`txt` for text
    /// taken as it is.
    pub ext: String,
    pub bytes: axum::body::Bytes,
}

/// Why a URL may not be fetched, if it may not: its scheme, its host off the allowlist, or an
/// address that isn't public.
pub fn refuse(url: &Url, domains: &[String], allow_private: bool) -> Option<String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Some(format!("{} is not a web address", url.scheme()));
    }
    let Some(host) = url.host_str() else {
        return Some("the address has no host".into());
    };
    if !allowed(host, domains) {
        return Some(format!("{host} is not on the allowlist"));
    }
    let literal = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>();
    if !allow_private && literal.is_ok_and(|ip| !public(ip)) {
        return Some(format!("{host} is not a public address"));
    }
    None
}

/// What a fetch brought back, before it is known to be a page.
#[derive(Debug, Clone)]
pub struct Raw {
    /// Where it was found, after redirects.
    pub url: String,
    /// The content type, without parameters, lowercase.
    pub ctype: String,
    pub bytes: axum::body::Bytes,
}

/// Fetches from one allowlist, with one HTTP client for all its requests.
pub struct Fetcher {
    http: reqwest::Client,
    domains: Arc<Vec<String>>,
    allow_private: bool,
}

impl Fetcher {
    pub fn new(domains: &[String], allow_private: bool) -> Result<Self, String> {
        let domains: Arc<Vec<String>> = Arc::new(domains.to_vec());
        let allow = domains.clone();
        let policy = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error("too many redirects");
            }
            match refuse(attempt.url(), &allow, allow_private) {
                Some(why) => attempt.error(format!("redirected: {why}")),
                None => attempt.follow(),
            }
        });
        let mut http = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .redirect(policy)
            .user_agent(USER_AGENT);
        http = if allow_private {
            http.no_proxy()
        } else {
            http.dns_resolver(Arc::new(PublicOnly))
        };
        Ok(Self {
            http: http.build().map_err(|e| e.to_string())?,
            domains,
            allow_private,
        })
    }

    /// Whether an address may be fetched; why not, if not.
    pub fn refuse(&self, url: &Url) -> Option<String> {
        refuse(url, &self.domains, self.allow_private)
    }

    /// Fetches whatever is at an allowed address (`robots.txt`, a sitemap, a page), up to
    /// [`MAX_FETCH`]. Errors say why, for the run's report.
    pub async fn get(&self, url: &str) -> Result<Raw, String> {
        let parsed = Url::parse(url).map_err(|e| format!("not a valid address: {e}"))?;
        if let Some(why) = self.refuse(&parsed) {
            return Err(why);
        }
        let mut resp = self
            .http
            .get(parsed)
            .send()
            .await
            .map_err(|e| format!("fetching failed: {}", root(&e)))?;
        if !resp.status().is_success() {
            return Err(format!("the site answered {}", resp.status()));
        }
        let final_url = resp.url().to_string();
        let ctype = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        Ok(Raw {
            url: final_url,
            ctype,
            bytes: body(&mut resp).await?,
        })
    }

    /// An origin's `robots.txt` as RFC 9309 reads an answer: its text when there is one, an
    /// empty one (nothing is closed) when the site says there is none (4xx), and `None` (every
    /// page closed for now) when the site can't be reached, fails (5xx) or asks to be left
    /// alone for now (429).
    pub async fn robots_txt(&self, origin: &str) -> Option<String> {
        let url = Url::parse(&format!("{origin}/robots.txt")).ok()?;
        if self.refuse(&url).is_some() {
            return None;
        }
        let mut resp = self.http.get(url).send().await.ok()?;
        let status = resp.status();
        if status.is_client_error() && status != reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Some(String::new());
        }
        if !status.is_success() {
            return None;
        }
        let bytes = body(&mut resp).await.ok()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Fetches a page the converter can read.
    pub async fn page(&self, url: &str) -> Result<Fetched, String> {
        let raw = self.get(url).await?;
        let parsed = Url::parse(&raw.url).map_err(|e| e.to_string())?;
        let ext =
            ext_of(&raw.ctype, &parsed).ok_or_else(|| format!("{:?} can't be read", raw.ctype))?;
        Ok(Fetched {
            url: raw.url,
            ext,
            bytes: raw.bytes,
        })
    }
}

/// A response's body, up to [`MAX_FETCH`].
async fn body(resp: &mut reqwest::Response) -> Result<axum::body::Bytes, String> {
    let mut body = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("reading failed: {}", root(&e)))?
    {
        if body.len() + chunk.len() > MAX_FETCH {
            return Err(format!("larger than {} MB", MAX_FETCH / 1024 / 1024));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body.into())
}

fn root(e: &dyn std::error::Error) -> String {
    let mut msg = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        msg = format!("{msg}: {s}");
        src = s.source();
    }
    msg
}

/// The extension a page's content type (or, for a generic type, its path) stands for.
fn ext_of(ctype: &str, url: &Url) -> Option<String> {
    let by_type = match ctype {
        "text/html" | "application/xhtml+xml" => Some("html"),
        "application/pdf" => Some("pdf"),
        "text/markdown" | "text/x-markdown" => Some("md"),
        "text/plain" => Some("txt"),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => Some("docx"),
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => Some("pptx"),
        _ => None,
    };
    if let Some(e) = by_type {
        return Some(e.to_owned());
    }
    if ctype.is_empty() || ctype == "application/octet-stream" {
        let path = url.path().to_ascii_lowercase();
        let ext = path.rsplit_once('.')?.1;
        return ["html", "htm", "pdf", "md", "txt", "docx", "pptx"]
            .contains(&ext)
            .then(|| ext.to_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_must_be_allowed_and_public() {
        let domains = vec!["docs.example.com".to_string(), "rust-lang.org".to_string()];
        assert!(allowed("docs.example.com", &domains));
        assert!(allowed("blog.rust-lang.org", &domains));
        assert!(!allowed("example.com", &domains));
        assert!(!allowed("evilrust-lang.org", &domains));
        let www = vec!["www.example.org".to_string()];
        assert!(allowed("example.org", &www), "the same site");
        assert!(!allowed("blog.example.org", &www));
        let url = |s: &str| Url::parse(s).unwrap();
        assert!(refuse(&url("https://docs.example.com/a"), &domains, false).is_none());
        assert!(refuse(&url("file:///etc/passwd"), &domains, false).is_some());
        assert!(refuse(&url("https://other.com/"), &domains, false).is_some());
        let local = vec!["127.0.0.1".to_string(), "10.0.0.5".to_string()];
        assert!(refuse(&url("http://127.0.0.1:8080/"), &local, false).is_some());
        assert!(refuse(&url("http://10.0.0.5/"), &local, false).is_some());
        assert!(refuse(&url("http://127.0.0.1:8080/"), &local, true).is_none());
        for ip in [
            "169.254.169.254",
            "100.64.0.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(!public(ip.parse().unwrap()), "{ip}");
        }
        assert!(public("93.184.216.34".parse().unwrap()));
        assert!(public("2606:2800:220:1::".parse().unwrap()));
    }

    #[test]
    fn content_types_name_the_converter_format() {
        let u = Url::parse("https://a.com/x/guide.PDF").unwrap();
        assert_eq!(ext_of("text/html", &u).as_deref(), Some("html"));
        assert_eq!(
            ext_of("application/octet-stream", &u).as_deref(),
            Some("pdf")
        );
        assert_eq!(ext_of("image/png", &u), None);
    }
}

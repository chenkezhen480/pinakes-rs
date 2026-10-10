//! Reading a real trusted site, as acquisition does: its `robots.txt`, sitemaps and pages over
//! the internet, each page through the real converter. Runs only when asked, with the site and
//! the dev converter:
//!
//! ```text
//! PINAKES_TEST_CRAWL=www.example.com PINAKES_TEST_CONVERTER=http://127.0.0.1:8090 \
//!   cargo test --test integration crawl_live:: -- --nocapture
//! ```
//!
//! `PINAKES_TEST_CRAWL` is a domain, or a domain and start address separated by a space.
//! `PINAKES_TEST_CRAWL_PAGES` caps the pages read (15 by default); requests are a second apart.

use crate::common::app::TestApp;
use pinakes::acquisition::crawl;
use pinakes::acquisition::{AllowedSource, Settings};

#[tokio::test]
async fn a_real_site_is_read_politely() {
    let (Ok(site), Ok(converter)) = (
        std::env::var("PINAKES_TEST_CRAWL"),
        std::env::var("PINAKES_TEST_CONVERTER"),
    ) else {
        eprintln!("PINAKES_TEST_CRAWL and PINAKES_TEST_CONVERTER are not both set; skipping");
        return;
    };
    let pages: usize = std::env::var("PINAKES_TEST_CRAWL_PAGES")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(15);
    let app = TestApp::with_config(&format!(
        "[converter]\nurl = \"{converter}\"\n[acquisition]\nenabled = true\ncrawl_pages_per_site = {pages}\n"
    ));
    app.create_tenant("acme").await;
    let tenant = app.state.tenants.by_id("acme").await.unwrap().unwrap();
    let mut parts = site.split_whitespace();
    let settings = Settings {
        enabled: true,
        sources: vec![AllowedSource {
            domain: parts.next().unwrap().to_owned(),
            license: None,
            copy: false,
            urls: parts.map(str::to_owned).collect(),
        }],
    }
    .checked()
    .expect("a valid allowlist entry");

    let started = std::time::Instant::now();
    let report = crawl::crawl(&app.state, &tenant, &settings).await.unwrap();
    println!("{report:?} in {:.0} s", started.elapsed().as_secs_f64());
    let index = tenant.store.crawled_pages().await.unwrap();
    for p in &index {
        println!(
            "{}\n  title: {}\n  headings: {}\n  terms: {}",
            p.url,
            p.title,
            p.headings
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(" | "),
            p.terms.len()
        );
    }
    assert!(!index.is_empty(), "nothing was read: {report:?}");
    assert!(index.len() <= pages);
    assert!(
        index.iter().all(|p| p.terms.len() > 5),
        "pages without text"
    );
}

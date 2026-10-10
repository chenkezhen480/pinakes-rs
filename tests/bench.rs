//! M6 benchmark (design, "How it stays fast"): one tenant with 1k, 10k and 100k pages and a
//! graph of a matching size, timed per search step in keyword and semantic mode, plus the cost
//! of opening the tenant and of writes.
//!
//! The tenant is the `shopline` scenario (its pages, schema, spaces and graph data, and its
//! questions as the queries), filled up to the size with pages made from the lines, titles and
//! aliases of all six scenarios, and with orders and customers in its `mainland` space (a tenth
//! of them on one hub customer). Page vectors come from a fast stand-in model with the real
//! model's 768 dimensions, so the vector search costs what it would; embedding the query with
//! the real model is timed on its own when `PINAKES_TEST_EMBEDDING` is set.
//!
//! ```text
//! PINAKES_BENCH_PAGES=1000,10000,100000 cargo test --release --test integration bench:: -- --ignored --nocapture
//! ```
//!
//! Without the variable it runs 1,000 pages. `PINAKES_BENCH_THREADS=<n>` sets the service's
//! compute threads (`[server] threads`; 4 by default). `PINAKES_BENCH_SETTLE=<seconds>` waits after the
//! tenant opens, so the history save it starts in the background is done before anything else
//! is timed.

use crate::common;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::common::app::{Caller, TestApp};
use crate::common::testset;
use pinakes::embed::Embedder;
use pinakes::tenants::{group_dir_name, tenant_dir_name, user_dir_name};
use pinakes::wiki::{Page, PagePath, WikiFs};
use serde_json::{Value, json};

const TENANT: &str = "shopline";
/// Each query runs this often per mode.
const RUNS: usize = 5;
const DIM: usize = 768;
/// The customer every tenth order belongs to.
const HUB: &str = "王大客户";

/// Bags of character bigrams in 768 dimensions: as fast as hashing, as large as the real
/// model's vectors.
struct Stand;

impl Embedder for Stand {
    fn model(&self) -> &str {
        "bench-stand-in"
    }
    fn dim(&self) -> usize {
        DIM
    }
    fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                let chars: Vec<char> = t.to_lowercase().chars().collect();
                let mut v = vec![0f32; DIM];
                for w in chars.windows(2) {
                    let h = (w[0] as usize).wrapping_mul(31).wrapping_add(w[1] as usize);
                    v[h % DIM] += 1.0;
                }
                let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
                v.iter().map(|x| x / n).collect()
            })
            .collect())
    }
}

/// xorshift64*, so every run makes the same pages.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a, T>(&mut self, list: &'a [T]) -> &'a T {
        &list[self.below(list.len())]
    }
}

/// What filler pages are made from: every scenario's titles, aliases, keywords, summaries,
/// headings and body lines.
#[derive(Default)]
struct Pool {
    titles: Vec<String>,
    cjk_aliases: Vec<String>,
    latin_aliases: Vec<String>,
    keywords: Vec<String>,
    summaries: Vec<String>,
    headings: Vec<String>,
    lines: Vec<String>,
}

fn pool() -> Pool {
    let mut pool = Pool::default();
    let root = testset::testset_dir().join("tenants");
    let mut files = Vec::new();
    collect_md(&root, &mut files);
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        let page = Page::parse(PagePath::parse("expertise/x").unwrap(), &text).unwrap();
        let f = &page.front;
        if f.extra.contains_key("schema") {
            continue;
        }
        pool.titles.extend(f.title.clone());
        for a in &f.aliases {
            if a.chars().all(|c| c as u32 > 0x2e80) {
                pool.cjk_aliases.push(a.clone());
            } else {
                pool.latin_aliases.push(a.clone());
            }
        }
        pool.keywords.extend(f.keywords.iter().cloned());
        pool.summaries.extend(f.summary.clone());
        for line in page.body.lines().map(str::trim).filter(|l| !l.is_empty()) {
            match line.strip_prefix("## ") {
                Some(h) => pool.headings.push(h.to_owned()),
                None if !line.starts_with('#') => pool.lines.push(line.to_owned()),
                None => {}
            }
        }
    }
    pool
}

fn collect_md(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_md(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

/// Filler page `i`: mostly tenant experience and expertise, some user decisions and group
/// notes, each with its own aliases (so the tenant's term list grows with it).
fn filler(pool: &Pool, rng: &mut Rng, i: usize) -> Page {
    let users = [user_dir_name("lily"), user_dir_name("wang")];
    let group = group_dir_name("ops");
    let roll = rng.below(100);
    let path = match roll {
        0..40 => format!("experience/bench-{i}"),
        40..85 => format!("expertise/bench-{i}"),
        85..95 => format!("users/{}/decisions/bench-{i}", rng.pick(&users)),
        _ => format!("groups/{group}/expertise/bench-{i}"),
    };
    let mut page = Page::new(PagePath::parse(&path).unwrap());
    let title = format!("{} {i}", rng.pick(&pool.titles));
    let f = &mut page.front;
    f.title = Some(title.clone());
    f.aliases = vec![
        format!(
            "{}{}",
            rng.pick(&pool.cjk_aliases),
            rng.pick(&pool.cjk_aliases)
        ),
        format!(
            "{} {}",
            rng.pick(&pool.latin_aliases),
            rng.pick(&pool.latin_aliases)
        ),
    ];
    f.keywords = (0..3).map(|_| rng.pick(&pool.keywords).clone()).collect();
    f.summary = Some(rng.pick(&pool.summaries).clone());
    if roll < 40 || (85..95).contains(&roll) {
        f.about = vec!["commerce:order".into()];
    }
    f.updated = Some("2026-10-01".into());
    let mut body = format!("# {title}\n");
    for _ in 0..2 + rng.below(3) {
        body.push_str(&format!("\n## {}\n\n", rng.pick(&pool.headings)));
        for _ in 0..3 + rng.below(4) {
            body.push_str(rng.pick(&pool.lines));
            body.push('\n');
        }
    }
    page.body = body;
    page
}

/// Orders and customers for the `mainland` space: `n` orders, `n / 5` customers, every tenth
/// order placed by one hub customer.
fn graph_batches(n: usize, rng: &mut Rng) -> Vec<Value> {
    const SURNAMES: &[&str] = &["王", "李", "张", "刘", "陈", "杨", "黄", "赵", "周", "吴"];
    const GIVEN: &[&str] = &[
        "伟", "芳", "娜", "敏", "静", "磊", "洋", "艳", "勇", "军", "杰", "涛",
    ];
    let customers = (n / 5).max(1);
    let mut nodes = Vec::new();
    for c in 0..customers {
        let name = match c {
            0 => HUB.to_owned(),
            _ => format!(
                "{}{}{}",
                rng.pick(SURNAMES),
                rng.pick(GIVEN),
                rng.pick(GIVEN)
            ),
        };
        nodes.push(
            json!({ "type": "customer", "key": format!("bc_{c}"), "name": name,
            "aliases": [format!("Customer {c}")] }),
        );
    }
    let mut edges = Vec::new();
    for o in 0..n {
        nodes.push(
            json!({ "type": "order", "key": format!("b{o}"), "name": format!("Order b{o}"),
            "aliases": [format!("订单b{o}")] }),
        );
        let c = if o % 10 == 0 { 0 } else { rng.below(customers) };
        edges.push(json!({ "from": format!("order/b{o}"), "type": "placed_by",
            "to": format!("customer/bc_{c}") }));
    }
    // Customers first, then orders with their edges, 10,000 of each per call (the limit).
    let mut out = Vec::new();
    let (cust, orders) = nodes.split_at(customers);
    for chunk in cust.chunks(10_000) {
        out.push(json!({ "space": "mainland", "schema": "commerce", "nodes": chunk, "edges": [] }));
    }
    for (chunk, e) in orders.chunks(10_000).zip(edges.chunks(10_000)) {
        out.push(json!({ "space": "mainland", "schema": "commerce", "nodes": chunk, "edges": e }));
    }
    out
}

fn ms(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1000.0)
}

/// Median and 95th percentile of microsecond samples, in milliseconds.
fn stats(samples: &mut [u64]) -> (f64, f64) {
    samples.sort_unstable();
    let at = |q: f64| samples[((samples.len() as f64 - 1.0) * q).round() as usize] as f64 / 1000.0;
    (at(0.5), at(0.95))
}

fn trace_timings(app: &TestApp) -> Vec<BTreeMap<String, u64>> {
    app.state.traces.flush();
    let logs = app
        .state
        .config
        .tenants_dir()
        .join(tenant_dir_name(TENANT))
        .join("logs");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&logs).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.starts_with("search-") {
            continue;
        }
        for line in std::fs::read_to_string(&path).unwrap().lines() {
            let v: Value = serde_json::from_str(line).unwrap();
            if v["endpoint"] != "search" {
                continue;
            }
            let t: BTreeMap<String, u64> =
                serde_json::from_value(v["timings_us"].clone()).unwrap_or_default();
            out.push(t);
        }
        std::fs::remove_file(&path).unwrap();
    }
    out
}

/// The time of one search, the first after a (re)start: it opens the tenant.
async fn first_search(app: &TestApp, key: &str) -> Duration {
    let started = Instant::now();
    let (status, resp) = app
        .post(Caller::new(key), "/search", json!({ "query": "退款" }))
        .await;
    assert!(status.is_success(), "{resp}");
    started.elapsed()
}

async fn run(pages: usize) {
    println!("\n==== {pages} pages ====");
    // `PINAKES_BENCH_GRAPH_ROWS` sets `[search] vector_graph_rows` (0: never a graph index).
    let extra = match std::env::var("PINAKES_BENCH_GRAPH_ROWS") {
        Ok(n) => format!(
            "[search]
vector_graph_rows = {}
",
            n.trim()
        ),
        Err(_) => String::new(),
    };
    let mut app = TestApp::with_embedder(&extra, Arc::new(Stand));
    let key = app.create_tenant(TENANT).await;
    let wiki_dir = app.wiki_dir(TENANT);
    testset::install_wiki(TENANT, &wiki_dir);
    let pool = pool();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let wiki = WikiFs::new(&wiki_dir);
    let real = WikiFs::new(&wiki_dir).all_pages().unwrap().len();
    let started = Instant::now();
    for i in 0..pages.saturating_sub(real) {
        wiki.write(&filler(&pool, &mut rng, i)).unwrap();
    }
    println!("pages written in {}", ms(started.elapsed()));

    // Opening the tenant: the first search builds the snapshot (and embeds every page); after a
    // restart the vectors come from SQLite.
    let t = testset::tenants()
        .into_iter()
        .find(|t| t.id == TENANT)
        .unwrap();
    app.restart();
    println!(
        "open, embedding every page: {}",
        ms(first_search(&app, &key).await)
    );
    app.restart();
    println!(
        "open, vectors from SQLite: {}",
        ms(first_search(&app, &key).await)
    );

    // The first open saves the wiki version in the history in the background, writing every
    // page once: on Windows that takes minutes for 100,000 pages and slows everything else.
    if let Some(secs) = std::env::var("PINAKES_BENCH_SETTLE")
        .ok()
        .and_then(|s| s.parse().ok())
    {
        tokio::time::sleep(Duration::from_secs(secs)).await;
    }

    // The scenario's graph, then orders and customers. Every write call is timed.
    for call in testset::graph_calls(&t) {
        let caller = Caller::new(&key)
            .user(call.user.as_deref())
            .groups(&call.groups);
        let (status, body) = app.post(caller, call.uri, call.body).await;
        assert!(status.is_success(), "{} {status} {body}", call.uri);
    }
    let mut writes = Vec::new();
    for batch in graph_batches(pages, &mut rng) {
        let n = batch["nodes"].as_array().unwrap().len();
        let started = Instant::now();
        let (status, body) = app
            .post(Caller::new(&key), "/ingest/graph/data", batch)
            .await;
        assert!(status.is_success(), "{status} {body}");
        writes.push((n, started.elapsed()));
    }
    let total: Duration = writes.iter().map(|w| w.1).sum();
    println!(
        "graph data: {} calls, {} in all, last call ({} nodes) {}",
        writes.len(),
        ms(total),
        writes.last().unwrap().0,
        ms(writes.last().unwrap().1)
    );
    let one = json!({ "space": "mainland", "schema": "commerce",
        "nodes": [{ "type": "customer", "key": "bc_new", "name": "新客户" }], "edges": [] });
    let started = Instant::now();
    let (status, _) = app.post(Caller::new(&key), "/ingest/graph/data", one).await;
    assert!(status.is_success());
    println!("graph data: one node added: {}", ms(started.elapsed()));

    // One page changed: the write path re-indexes the tenant.
    let tenant = app.state.tenants.authenticate(&key).await.unwrap().unwrap();
    let mut page = filler(&pool, &mut rng, pages + 1);
    page.path = PagePath::parse("expertise/bench-changed").unwrap();
    wiki.write(&page).unwrap();
    let started = Instant::now();
    tenant.reindex().await.unwrap();
    println!("one page changed, re-index: {}", ms(started.elapsed()));

    // The questions, as their callers, in each mode.
    trace_timings(&app);
    // The scenario's questions as their callers, and a few on the graph's size: the hub
    // customer's orders, a keyword inside node names, a keyword that names nothing.
    let mut queries: Vec<(Option<String>, Value)> = testset::questions(TENANT)
        .into_iter()
        .map(|q| {
            let mut body = json!({ "query": q.query });
            if !q.keywords.is_empty() {
                body["keywords"] = json!(q.keywords);
            }
            (q.user, body)
        })
        .collect();
    for (query, keywords) in [
        (format!("{HUB}下了哪些订单？"), vec![HUB, "订单"]),
        ("大客户的订单最近有退款吗".into(), vec!["大客户", "退款"]),
        ("这个词什么都找不到".into(), vec!["无此词条", "不存在"]),
    ] {
        queries.push((None, json!({ "query": query, "keywords": keywords })));
    }
    for mode in ["keyword", "semantic"] {
        let mut wall = Vec::new();
        for _ in 0..RUNS {
            for (user, body) in &queries {
                let groups = user.as_deref().map(|u| t.groups_of(u)).unwrap_or_default();
                let caller = Caller::new(&key).user(user.as_deref()).groups(&groups);
                let mut body = body.clone();
                body["mode"] = json!(mode);
                let started = Instant::now();
                let (status, resp) = app.post(caller, "/search", body).await;
                wall.push(started.elapsed().as_micros() as u64);
                assert!(status.is_success(), "{resp}");
            }
        }
        let mut steps: BTreeMap<String, Vec<u64>> = BTreeMap::new();
        for t in trace_timings(&app) {
            for (k, v) in t {
                steps.entry(k).or_default().push(v);
            }
        }
        println!("{mode}: {} searches", wall.len());
        println!("  {:<18} {:>9} {:>9}", "step", "median", "p95");
        for (step, samples) in &mut steps {
            let (p50, p95) = stats(samples);
            println!("  {step:<18} {p50:>6.2} ms {p95:>6.2} ms");
        }
        let (p50, p95) = stats(&mut wall);
        println!("  {:<18} {p50:>6.2} ms {p95:>6.2} ms", "request");
    }

    // The graph index against the exact scan, on the same snapshot: every question's top ten
    // pages per slot as the tenant sees them.
    let snap = tenant.snapshot().await.unwrap();
    let vectors = snap.vectors.as_ref().unwrap();
    println!(
        "vectors: {} rows, graph index: {}",
        vectors.len(),
        vectors.has_graph()
    );
    if vectors.has_graph() {
        let (mut hits, mut wanted) = (0, 0);
        let (mut graph_us, mut exact_us) = (Vec::new(), Vec::new());
        for (_, body) in &queries {
            let q = Stand
                .embed(&[body["query"].as_str().unwrap().to_owned()])
                .unwrap()
                .remove(0);
            for slot in [
                pinakes::search::catalog::Slot::Memories,
                pinakes::search::catalog::Slot::Knowledge,
            ] {
                let keep = |ord: usize| {
                    let p = &snap.catalog.pages[ord];
                    p.audience == "tenant" && p.slot == slot
                };
                let started = Instant::now();
                let (graph, method) = vectors.nearest(&q, slot, keep, 50);
                graph_us.push(started.elapsed().as_micros() as u64);
                assert_eq!(method, pinakes::search::vectors::Method::Graph);
                let started = Instant::now();
                let exact = vectors.best(&q, keep);
                exact_us.push(started.elapsed().as_micros() as u64);
                // A page as similar as the exact scan's tenth counts: among equal scores
                // either is right.
                let Some(&(_, tenth)) = exact.get(9).or(exact.last()) else {
                    continue;
                };
                wanted += exact.len().min(10);
                hits += graph
                    .iter()
                    .take(10)
                    .filter(|(_, s)| *s >= tenth - 1e-6)
                    .count();
            }
        }
        let (g50, g95) = stats(&mut graph_us);
        let (e50, e95) = stats(&mut exact_us);
        println!(
            "graph vs exact, per slot: graph {g50:.2} ms (p95 {g95:.2}), exact {e50:.2} ms (p95 {e95:.2}), recall@10 {:.3} ({hits}/{wanted})",
            hits as f64 / wanted.max(1) as f64
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark: PINAKES_BENCH_PAGES=1000,10000,100000 cargo test --release --test integration bench:: -- --ignored --nocapture"]
async fn bench() {
    // The service's own log lines say where opening a tenant spends its time.
    let _ = tracing_subscriber::fmt()
        .with_env_filter("pinakes=info")
        .with_test_writer()
        .try_init();
    // `PINAKES_BENCH_THREADS` sets the service's compute budget (`[server] threads`).
    let threads: usize = std::env::var("PINAKES_BENCH_THREADS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    pinakes::par::set_threads(threads);
    println!("compute threads: {}", pinakes::par::threads());
    let sizes: Vec<usize> = std::env::var("PINAKES_BENCH_PAGES")
        .unwrap_or_else(|_| "1000".into())
        .split(',')
        .map(|s| s.trim().parse().expect("page counts"))
        .collect();
    if let Some(model) = common::embed::real() {
        let q = vec!["客户付款后想改收货地址，已经发货了怎么办？".to_string()];
        model.embed(&q).unwrap();
        let mut samples: Vec<u64> = (0..20)
            .map(|_| {
                let started = Instant::now();
                model.embed(&q).unwrap();
                started.elapsed().as_micros() as u64
            })
            .collect();
        let (p50, p95) = stats(&mut samples);
        println!("embed_query (real model): median {p50:.2} ms, p95 {p95:.2} ms");
    }
    for pages in sizes {
        run(pages).await;
    }
}

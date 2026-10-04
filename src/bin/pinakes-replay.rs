//! `pinakes-replay`: runs recorded searches again against the current code and wiki, and reports
//! what changed (design, "Search traces and replay").
//!
//! ```text
//! pinakes-replay [--config config.toml] [--days N] [--wiki recorded|current] [--json report.json]
//!           [TRACE_FILE_OR_FOLDER ...]
//! ```
//!
//! `--wiki recorded` (the default) runs each search on the wiki version it ran on, rebuilt from
//! the tenant's wiki history, so differences come from the code or the config; `current` runs
//! on today's wiki.
//!
//! Without paths it replays every tenant's `logs/` under the configured data folder. Inside
//! folders only the last `--days` days are read (7 by default, 0 for all); files named directly
//! are always read. Records are streamed, so memory stays flat however many days are replayed.
//! Replays never write traces and never mark pages as read.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use pinakes::config::Config;
use pinakes::tenants::Tenants;
use pinakes::trace::replay::{TraceReader, WikiSource, replay, trace_files};

/// Days of traces replayed by default.
const DEFAULT_DAYS: u32 = 7;

fn usage() -> ! {
    eprintln!(
        "usage: pinakes-replay [--config config.toml] [--days N] [--wiki recorded|current] [--json report.json] [TRACE_FILE_OR_FOLDER ...]"
    );
    std::process::exit(2)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut config_path = std::env::var_os("PINAKES_CONFIG").map(PathBuf::from);
    let mut json_out: Option<PathBuf> = None;
    let mut days = DEFAULT_DAYS;
    let mut wiki = WikiSource::Recorded;
    let mut paths = Vec::new();
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--config") => {
                config_path = Some(args.next().map(PathBuf::from).unwrap_or_else(|| usage()))
            }
            Some("--json") => {
                json_out = Some(args.next().map(PathBuf::from).unwrap_or_else(|| usage()))
            }
            Some("--days") => {
                days = args
                    .next()
                    .and_then(|d| d.to_str().and_then(|d| d.parse().ok()))
                    .unwrap_or_else(|| usage())
            }
            Some("--wiki") => {
                wiki = match args.next().as_deref().and_then(|w| w.to_str()) {
                    Some("recorded") => WikiSource::Recorded,
                    Some("current") => WikiSource::Current,
                    _ => usage(),
                }
            }
            Some("-h" | "--help") => usage(),
            _ => paths.push(PathBuf::from(arg)),
        }
    }
    // A config named on the command line or in PINAKES_CONFIG must exist; otherwise
    // ./config.toml is used when present, and the defaults when not.
    let config = match config_path {
        Some(path) => Config::load(&path)?,
        None if std::path::Path::new("config.toml").exists() => {
            Config::load(std::path::Path::new("config.toml"))?
        }
        None => Config::default(),
    };
    if paths.is_empty() {
        paths.push(config.tenants_dir());
    }

    let files = trace_files(&paths, (days > 0).then_some(days)).context("finding trace files")?;
    if days > 0 {
        println!(
            "reading {} trace files from the last {days} days",
            files.len()
        );
    } else {
        println!("reading {} trace files", files.len());
    }
    let mut records = TraceReader::new(files);
    // Replays read today's graph: graph data has no history (design, "Search traces and
    // replay").
    // Searches in the meaning-based modes need the embedding model the service uses.
    let embedder = pinakes::load_embedder(&config);
    let tenants = Tenants::new(
        config.tenants_dir(),
        config.store.file_name.clone(),
        Duration::from_secs(60),
    )
    .graph_backend(pinakes::graph::GraphBackend::from_config(&config))
    .embedder(embedder.clone())
    // The service may be running on the same folder: a replay never writes its stores.
    .without_storing_vectors();
    let report = replay(
        &tenants,
        &config.search,
        embedder.as_deref(),
        wiki,
        records.by_ref(),
    )
    .await?;
    for e in &records.errors {
        eprintln!("could not read {e}");
    }

    println!(
        "replaying with {} (search config {}) on the {} wiki",
        report.service,
        report.search_config,
        match report.wiki {
            WikiSource::Recorded => "recorded",
            WikiSource::Current => "current",
        }
    );
    for (service, n) in &report.recorded_services {
        println!("  recorded by {service}: {n}");
    }
    for (config, n) in &report.recorded_search_configs {
        println!("  recorded with search config {config}: {n}");
    }
    for d in &report.diffs {
        println!(
            "{} {} [{}]{}{} {:?}",
            d.at,
            d.request_id,
            d.tenant,
            if d.wiki_changed {
                " (wiki changed)"
            } else {
                ""
            },
            if d.graph_changed {
                " (graph changed)"
            } else {
                ""
            },
            d.query
        );
        for s in &d.slots {
            println!(
                "    {}: {:?} ({}) -> {:?} ({})",
                s.slot, s.before, s.coverage_before, s.after, s.coverage_after
            );
        }
        if d.unmatched_before != d.unmatched_after {
            println!(
                "    unmatched: {:?} -> {:?}",
                d.unmatched_before, d.unmatched_after
            );
        }
    }
    for (id, why) in &report.failed {
        println!("failed {id}: {why}");
    }
    println!(
        "replayed {} searches: {} same, {} changed; {} on another wiki than recorded ({} recorded versions no longer kept), {} on another graph; skipped {}, failed {}, unreadable lines {}",
        report.searches,
        report.same,
        report.changed,
        report.wiki_changed,
        report.wiki_missing,
        report.graph_changed,
        report.skipped,
        report.failed.len(),
        records.bad_lines
    );
    if let Some(out) = json_out {
        std::fs::write(&out, serde_json::to_string_pretty(&report)?)
            .with_context(|| format!("writing {}", out.display()))?;
    }
    Ok(())
}

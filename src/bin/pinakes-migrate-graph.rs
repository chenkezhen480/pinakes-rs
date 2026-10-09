//! Moves every tenant's graph data from one store to the other (design, "Graph data in SQLite
//! by default"):
//!
//! ```text
//! pinakes-migrate-graph --to neo4j [--replace] [--config config.toml]
//! pinakes-migrate-graph --to sqlite [--replace] [--config config.toml]
//! ```
//!
//! Stop the service first. The data folder and the Neo4j connection come from the config
//! (`PINAKES_CONFIG`, `--config`, or `./config.toml`); its `[graph] backend` is not used, the
//! direction is `--to`. Each tenant is copied, counted back, and only then switched; then its
//! old copy is removed. Afterwards set `[graph] backend` to the new store and start the
//! service.

use std::path::PathBuf;

use anyhow::Context;
use pinakes::config::Config;
use pinakes::graph::GraphBackend;
use pinakes::graph::neo4j::Neo4jClient;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut to = None;
    let mut replace = false;
    let mut config_path = std::env::var_os("PINAKES_CONFIG").map(PathBuf::from);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--to" => to = args.next(),
            "--replace" => replace = true,
            "--config" => config_path = args.next().map(PathBuf::from),
            other => anyhow::bail!(
                "unknown argument {other:?}; usage: pinakes-migrate-graph --to sqlite|neo4j [--replace] [--config <file>]"
            ),
        }
    }
    let config = match config_path {
        Some(path) => Config::load(&path)?,
        None if std::path::Path::new("config.toml").exists() => {
            Config::load(std::path::Path::new("config.toml"))?
        }
        None => Config::default(),
    };
    let neo4j = || -> anyhow::Result<GraphBackend> {
        Ok(GraphBackend::Neo4j(std::sync::Arc::new(Neo4jClient::new(
            config.neo4j.clone(),
        ))))
    };
    let (from, to) = match to.as_deref() {
        Some("neo4j") => (GraphBackend::Sqlite, neo4j()?),
        Some("sqlite") => (neo4j()?, GraphBackend::Sqlite),
        _ => anyhow::bail!("say where to move the graph data: --to sqlite or --to neo4j"),
    };
    for backend in [&from, &to] {
        if let GraphBackend::Neo4j(client) = backend {
            client.connect().await?;
        }
    }
    let root = config.tenants_dir();
    let moved =
        pinakes::graph::migrate::migrate(&root, &config.store.file_name, &from, &to, replace)
            .await
            .context("moving the graph data")?;
    for m in &moved {
        let name = m
            .tenant_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        if m.already {
            println!("{name}: already in {} (nothing to copy)", to.name());
        } else {
            println!(
                "{name}: {} nodes and {} edges copied to {}",
                m.nodes,
                m.edges,
                to.name()
            );
        }
    }
    println!(
        "Done: {} tenants, their copies in {} removed. Set [graph] backend = \"{}\" in the config before starting the service.",
        moved.len(),
        from.name(),
        to.name()
    );
    Ok(())
}

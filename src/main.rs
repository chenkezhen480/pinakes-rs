use std::path::PathBuf;

use anyhow::Context;
use pinakes::AppState;
use pinakes::config::Config;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::FmtSpan;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Span close events carry `time.busy`/`time.idle`, which is the per-step timing.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,pinakes=debug")),
        )
        .with_span_events(FmtSpan::CLOSE)
        .init();

    let config_path = std::env::var_os("PINAKES_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.toml"));
    let config = if config_path.exists() {
        Config::load(&config_path)?
    } else {
        tracing::warn!(path = %config_path.display(), "config file not found, using defaults");
        Config::default()
    };

    std::fs::create_dir_all(config.tenants_dir())
        .with_context(|| format!("creating {}", config.tenants_dir().display()))?;

    let admin_key = std::env::var(&config.identity.admin_key_env).ok();
    if admin_key.as_deref().is_none_or(|k| k.trim().is_empty()) {
        tracing::warn!(
            env = %config.identity.admin_key_env,
            "no admin key set; /admin routes are disabled"
        );
    }

    let addr = format!("{}:{}", config.server.host, config.server.port);
    let state = AppState::new(config, admin_key.as_deref());
    if let Some(key) = state.prepare().await? {
        // Shown once; it is also kept in the key file.
        println!(
            "Created the default tenant. Its API key (also in {}):\n{key}",
            state
                .config
                .data
                .dir
                .join(pinakes::PERSONAL_KEY_FILE)
                .display()
        );
    }
    let app = pinakes::router(state.clone());

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    // Lines still queued when the server stops are written before exiting.
    tokio::task::spawn_blocking(move || state.traces.flush()).await?;
    Ok(())
}

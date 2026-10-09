use std::path::PathBuf;

use std::io::IsTerminal;

use anyhow::Context;
use pinakes::AppState;
use pinakes::config::Config;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::FmtSpan;

fn main() -> anyhow::Result<()> {
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
        let mut config = Config::default();
        config.load_model_file(std::path::Path::new("."))?;
        config
    };

    // The service computes within `[server] threads`; the async runtime too.
    pinakes::par::set_threads(config.server.threads);
    tracing::info!(threads = pinakes::par::threads(), "compute threads");
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(pinakes::par::threads())
        .enable_all()
        .build()
        .context("starting the async runtime")?
        .block_on(serve(config))
}

async fn serve(config: Config) -> anyhow::Result<()> {
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
    state.start_cache_sweeper();
    ask_for_key(&state);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, "listening");
    if pinakes::dev::enabled() {
        tracing::info!("test chat at http://{addr}/dev");
    }
    let shutdown = state.shutdown.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            stop_signal().await;
            shutdown.cancel();
        })
        .await?;
    // Lines still queued when the server stops are written before exiting.
    tokio::task::spawn_blocking(move || state.traces.flush()).await?;
    Ok(())
}

/// Ctrl-C, or SIGTERM, which `docker stop` sends: either stops the service cleanly, so queued
/// trace lines are written. In a container the service is process 1, which the system doesn't
/// stop for an unhandled SIGTERM.
async fn stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
                return;
            }
            Err(e) => tracing::warn!(error = %e, "SIGTERM can't be watched; only Ctrl-C stops"),
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// The librarian is the core of every write, so when only its key is missing the service asks
/// for it in the terminal once it is up (reads work meanwhile). A key typed here lasts until
/// the service stops; to keep one, put it in the model file (`api_key`). Without a terminal (a
/// container, a background run) it can't ask, and says where to put the key instead.
fn ask_for_key(state: &AppState) {
    let Some(why) = state.missing_key() else {
        return;
    };
    if !std::io::stdin().is_terminal() {
        tracing::warn!("the librarian is off: {why}; add the key and restart");
        return;
    }
    let state = state.clone();
    // A plain thread, not the runtime's: a pending read must not hold up shutdown.
    std::thread::spawn(move || {
        loop {
            let prompt = format!(
                "\nThe librarian (every write) is off: {why}.\nEnter the key for this run: "
            );
            let key = match rpassword::prompt_password(prompt) {
                Ok(k) if !k.trim().is_empty() => k.trim().to_owned(),
                Ok(_) => continue,
                Err(e) => {
                    tracing::warn!(error = %e, "could not read the key");
                    return;
                }
            };
            match pinakes::llm::client(&state.config.librarian, Some(key)) {
                Ok(llm) => {
                    state.set_llm(llm);
                    println!("Librarian on.");
                    return;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "the librarian stays off");
                    return;
                }
            }
        }
    });
}

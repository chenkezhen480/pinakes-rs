//! A self-hosted knowledge service: an LLM-maintained wiki that agents search in milliseconds.
//! See `docs/design.md`.

pub mod api;
pub mod config;
pub mod error;
pub mod identity;
pub mod kinds;
pub mod store;
pub mod tenants;
pub mod wiki;

use std::ops::Deref;
use std::sync::Arc;

use config::Config;
use identity::AdminKey;
use tenants::Tenants;

/// State shared by every request.
#[derive(Clone)]
pub struct AppState(Arc<AppInner>);

pub struct AppInner {
    pub config: Config,
    pub tenants: Tenants,
    pub admin_key: Option<AdminKey>,
}

impl AppState {
    /// `admin_key` is the service admin key; `None` or empty disables the `/admin` routes.
    pub fn new(config: Config, admin_key: Option<&str>) -> Self {
        let tenants = Tenants::new(config.tenants_dir(), config.store.file_name.clone());
        Self(Arc::new(AppInner {
            admin_key: admin_key.and_then(AdminKey::new),
            config,
            tenants,
        }))
    }
}

impl Deref for AppState {
    type Target = AppInner;

    fn deref(&self) -> &AppInner {
        &self.0
    }
}

pub use api::router;

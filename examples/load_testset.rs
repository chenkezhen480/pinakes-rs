//! Loads the six `testset/` scenarios into a data folder so the service can be tried by hand:
//!
//! ```text
//! cargo run --example load_testset -- ./data
//! cargo run
//! ```
//!
//! Each scenario becomes a tenant with the scenario's name; its API key and users are printed.
//! Scenarios that already exist in the folder are skipped.

#[path = "../tests/common/testset.rs"]
mod testset;

use std::path::PathBuf;
use std::time::Duration;

use kb::tenants::{CreateTenantError, Tenants};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let data = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./data"));
    let root = data.join("tenants");
    std::fs::create_dir_all(&root)?;
    let tenants = Tenants::new(&root, "kb.sqlite", Duration::from_secs(60));
    for t in testset::tenants() {
        match tenants.create(&t.id, "testset").await {
            Ok(created) => {
                testset::install_wiki(&t.id, &created.tenant.dir.join("wiki"));
                println!("{:<11} key {}", t.id, created.api_key);
                for u in &t.users {
                    let groups = if u.groups.is_empty() {
                        String::new()
                    } else {
                        format!("  X-User-Groups: {}", u.groups.join(","))
                    };
                    println!("            X-User-Id: {}{groups}", u.id);
                }
            }
            Err(CreateTenantError::AlreadyExists) => {
                println!("{:<11} already exists, skipped", t.id)
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

//! The console's API types, `ui/src/api/types.ts`, generated from the Rust structs the handlers
//! take and answer with (design, "M10": the console and the API can't drift apart unnoticed).
//! A test writes them with ts-rs, every type and what it uses into one file, and fails when the
//! committed file differs: `PINAKES_WRITE_TYPES=1 cargo test --lib api::types`.

use std::collections::BTreeMap;
use std::path::Path;

use ts_rs::{Config, TS};

/// Exports `T` and everything it uses into the config's folder.
fn export<T: TS + 'static>(cfg: &Config) {
    T::export_all(cfg).unwrap_or_else(|e| panic!("exporting {}: {e}", T::name(cfg)));
}

/// Every `.ts` file under `dir`, by type name, without its header and imports.
fn collect(dir: &Path, out: &mut BTreeMap<String, String>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
            continue;
        }
        if path.extension().is_none_or(|e| e != "ts") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let body: Vec<&str> = text
            .lines()
            .filter(|l| !l.starts_with("import ") && !l.starts_with("// This file was generated"))
            .collect();
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        out.insert(name, body.join("\n").trim().to_owned());
    }
}

fn generate() -> String {
    use crate::api;
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let cfg = Config::new().with_large_int("number").with_out_dir(d);
    export::<crate::console::ConsoleSession>(&cfg);
    export::<crate::console::SignIn>(&cfg);
    export::<crate::console::SignedIn>(&cfg);
    export::<crate::console::AccountChange>(&cfg);
    export::<crate::login::Account>(&cfg);
    export::<api::status::Status>(&cfg);
    export::<api::embedding::EmbeddingStatus>(&cfg);
    export::<api::embedding::SetEmbedding>(&cfg);
    export::<crate::settings::SettingsView>(&cfg);
    export::<api::settings::SettingsChange>(&cfg);
    export::<api::settings::Environment>(&cfg);
    export::<api::settings::AuditLog>(&cfg);
    export::<crate::admin::ModelView>(&cfg);
    export::<crate::admin::ModelChange>(&cfg);
    export::<crate::admin::ModelTest>(&cfg);
    export::<api::admin::TenantList>(&cfg);
    export::<api::admin::TenantAudit>(&cfg);
    export::<api::admin::CreateTenantRequest>(&cfg);
    export::<api::admin::CreateTenantResponse>(&cfg);
    export::<api::pages::ListResponse>(&cfg);
    export::<api::pages::PageResponse>(&cfg);
    export::<api::pages::PutPage>(&cfg);
    export::<api::pages::HistoryResponse>(&cfg);
    export::<api::jobs::JobList>(&cfg);
    export::<crate::jobs::JobInfo>(&cfg);
    export::<api::jobs::ProposalList>(&cfg);
    export::<api::jobs::Approved>(&cfg);
    export::<api::jobs::Rejected>(&cfg);
    export::<crate::search::SearchRequest>(&cfg);
    export::<crate::search::SearchResponse>(&cfg);
    export::<api::graph::GraphRequest>(&cfg);
    export::<api::graph::GraphResponse>(&cfg);
    export::<api::graph::PathRequest>(&cfg);
    export::<api::graph::PathResponse>(&cfg);
    export::<api::browse::SchemaList>(&cfg);
    export::<api::browse::SpaceList>(&cfg);
    export::<api::browse::NodeList>(&cfg);
    export::<api::insert::ExperienceIn>(&cfg);
    export::<api::insert::DecisionIn>(&cfg);
    export::<api::insert::ExpertiseIn>(&cfg);
    export::<api::insert::PreferenceIn>(&cfg);
    export::<api::read::ReadRequest>(&cfg);
    export::<api::read::ReadResponse>(&cfg);
    export::<api::lint::LintReport>(&cfg);
    export::<api::acquisition::Status>(&cfg);
    export::<crate::acquisition::Settings>(&cfg);
    export::<api::ingest::SchemaResponse>(&cfg);
    export::<api::ingest::SpaceResponse>(&cfg);
    export::<api::ingest::DataResponse>(&cfg);
    export::<crate::graph::def::SchemaDef>(&cfg);
    export::<crate::graph::def::SpaceRequest>(&cfg);
    export::<crate::graph::def::DataRequest>(&cfg);
    export::<api::draft::DraftRequest>(&cfg);
    export::<api::draft::DraftView>(&cfg);
    let mut types = BTreeMap::new();
    collect(d, &mut types);
    let mut out = String::from(
        "// The API's types, generated from the Rust structs by `PINAKES_WRITE_TYPES=1 cargo test --lib api::types`.\n// Never edit by hand.\n",
    );
    for body in types.values() {
        out.push('\n');
        out.push_str(body);
        out.push('\n');
    }
    out
}

#[test]
fn the_console_types_are_up_to_date() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui/src/api/types.ts");
    let now = generate();
    if std::env::var_os("PINAKES_WRITE_TYPES").is_some() {
        std::fs::write(&path, &now).unwrap();
    }
    let written = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        written.replace("\r\n", "\n") == now,
        "ui/src/api/types.ts is out of date: PINAKES_WRITE_TYPES=1 cargo test --lib api::types"
    );
}

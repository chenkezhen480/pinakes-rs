//! Two things for the binary: which code it was built from, for search traces and replay
//! reports (the git commit, with `-dirty` when the working tree had uncommitted changes; a
//! build given `PINAKES_GIT_COMMIT`, such as the Docker image, takes that; outside a git
//! checkout without it, `unknown`), and the console's built files (`ui/dist/`), embedded so
//! the service serves them at `/ui` with nothing beside it.

use std::path::Path;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn main() {
    console();
    commit();
}

/// `$OUT_DIR/console.rs`: every file of `ui/dist/` as `(path, bytes)`, its path relative to
/// `dist` with forward slashes. Without a build of the console the list is empty.
fn console() {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let path = e.path();
            if path.is_dir() {
                walk(&path, base, out);
            } else if let Ok(rel) = path.strip_prefix(base) {
                let rel = rel.to_string_lossy().replace('\\', "/");
                let abs = path.canonicalize().unwrap_or(path.clone());
                out.push((rel, abs.to_string_lossy().into_owned()));
            }
        }
    }
    println!("cargo:rerun-if-changed=ui/dist");
    let dist = Path::new("ui/dist");
    let mut files = Vec::new();
    walk(dist, dist, &mut files);
    let mut code = String::from("pub static FILES: &[(&str, &[u8])] = &[\n");
    for (rel, abs) in &files {
        code.push_str(&format!("    ({rel:?}, include_bytes!({abs:?})),\n"));
    }
    code.push_str("];\n");
    let out = Path::new(&std::env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("console.rs");
    std::fs::write(out, code).expect("writing console.rs");
}

fn commit() {
    println!("cargo:rerun-if-env-changed=PINAKES_GIT_COMMIT");
    // A build without the repository (the Docker image) is told its commit.
    if let Ok(commit) = std::env::var("PINAKES_GIT_COMMIT")
        && !commit.trim().is_empty()
    {
        println!("cargo:rustc-env=PINAKES_GIT_COMMIT={}", commit.trim());
        return;
    }
    let commit = match git(&["rev-parse", "--short=12", "HEAD"]) {
        Some(hash) if !hash.is_empty() => {
            // New, not yet committed source files count too; data folders are gitignored.
            let dirty = git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
            if dirty { format!("{hash}-dirty") } else { hash }
        }
        _ => "unknown".to_owned(),
    };
    println!("cargo:rustc-env=PINAKES_GIT_COMMIT={commit}");
    // Rebuild the stamp when the checked-out branch, its commit or the tracked sources change.
    // `.git/HEAD` only names the branch; a commit moves the branch's ref.
    for path in [
        ".git/HEAD",
        ".git/refs/heads",
        ".git/index",
        "src",
        "Cargo.toml",
        "build.rs",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
}

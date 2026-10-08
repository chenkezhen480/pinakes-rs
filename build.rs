//! Records which code a binary was built from, for search traces and replay reports: the git
//! commit, with `-dirty` when the working tree had uncommitted changes. A build given
//! `PINAKES_GIT_COMMIT` (the Docker image) takes that; outside a git checkout without it, the
//! commit is `unknown`.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn main() {
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

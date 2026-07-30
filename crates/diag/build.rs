//! Capture the git revision and build time so a crash report can say exactly
//! which build produced it.
//!
//! A report that only carries a semver is useless during a show: the version
//! has not changed since the last release, but the binary on the machine may
//! be three commits ahead of it.

use std::process::Command;

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into());

    // A dirty tree is worth knowing about: it means the sha alone does not
    // identify the source that was built.
    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    let rev = if dirty { format!("{sha}-dirty") } else { sha };
    println!("cargo:rustc-env=DIAG_GIT_REV={rev}");

    let built_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=DIAG_BUILT_AT_UNIX={built_at}");

    // Without this the rev goes stale after the first build of a session.
    // The path is found by walking up rather than hardcoded, because this
    // crate is vendored into repos that put it at different depths.
    if let Some(git) = find_git_dir() {
        println!("cargo:rerun-if-changed={}/HEAD", git.display());
        println!("cargo:rerun-if-changed={}/index", git.display());
    }
}

fn find_git_dir() -> Option<std::path::PathBuf> {
    let mut dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").ok()?);
    loop {
        let candidate = dir.join(".git");
        if candidate.exists() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

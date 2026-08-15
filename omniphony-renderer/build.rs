use anyhow::Result;
use chrono::TimeZone;
use std::env;
use std::path::PathBuf;
use std::process::Command;
use vergen_gitcl::{Emitter, GitclBuilder};

// Resolve a path inside the git directory, if it exists.
//
// `git rev-parse --git-path` handles the layouts we actually build in: a plain
// checkout, and a linked worktree (where `.git` is a file and per-worktree refs
// live under `.git/worktrees/<name>/`). Returns None outside a repository — a
// source tarball still builds, it just falls back to Cargo's default staleness
// rule.
fn git_path(arg: &str) -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--git-path", arg])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
    // Only declare paths that exist: cargo treats a missing `rerun-if-changed`
    // target as perpetually dirty, which would rebuild on every invocation.
    path.exists().then_some(path)
}

// Declare the git HEAD as an input of this build script.
//
// Without this, a build script that emits no `rerun-if-changed` is re-run only
// when a file in *its own package* changes. The version stamp does not depend
// on this package's sources at all — it depends on HEAD — so it used to freeze
// at whatever commit was checked out the last time some unrelated edit landed
// in the package. Observed in practice: `liborender` reporting a commit three
// days stale, and the `orender` binary one nearly three months stale, both on a
// clean tree. A wrong commit in `--version` is worse than no commit at all,
// since it silently misidentifies which build is running.
//
// Kept in sync with runtime_control/build.rs, which stamps the same pair of
// values for the embedded host.
fn emit_git_rerun_triggers() {
    // Covers a detached HEAD, where the file holds the commit id directly, and
    // any branch switch, which rewrites it.
    if let Some(path) = git_path("HEAD") {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    // On a branch, HEAD only names the ref; the commit id lives in the ref
    // file, and that is what a new commit rewrites.
    if let Ok(out) = Command::new("git")
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .output()
    {
        if out.status.success() {
            if let Ok(reference) = String::from_utf8(out.stdout) {
                if let Some(path) = git_path(reference.trim()) {
                    println!("cargo:rerun-if-changed={}", path.display());
                }
            }
        }
    }

    // After `git gc` the loose ref above is gone and the id lives here instead.
    if let Some(path) = git_path("packed-refs") {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn main() -> Result<()> {
    emit_git_rerun_triggers();

    // Generate git information
    let gitcl = GitclBuilder::default()
        .describe(true, true, Some("[0-9]*"))
        .build()?;

    let gitcl_res = Emitter::default()
        .idempotent()
        .fail_on_error()
        .add_instructions(&gitcl)
        .and_then(|emitter| emitter.emit());

    if let Err(e) = gitcl_res {
        eprintln!("Warning: Failed to generate git information: {e:?}");
        eprintln!("Using fallback version information");
        println!("cargo:rustc-env=VERGEN_GIT_DESCRIBE=unknown");
    }

    // Add build timestamp
    let now = match env::var("SOURCE_DATE_EPOCH") {
        Ok(val) => chrono::Utc.timestamp_opt(val.parse::<i64>()?, 0).unwrap(),
        Err(_) => chrono::Utc::now(),
    };

    println!(
        "cargo:rustc-env=BUILD_TIMESTAMP={}",
        now.format("%Y-%m-%d %H:%M:%S UTC")
    );

    Ok(())
}

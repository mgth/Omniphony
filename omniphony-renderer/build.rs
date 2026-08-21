use anyhow::Result;
use chrono::TimeZone;
use std::env;
use std::path::PathBuf;
use std::process::Command;
use vergen_gitcl::{Emitter, GitclBuilder};

// The repository root, resolved relative to this crate instead of by git's
// upward discovery: the workspace sits one level below it in a checkout. Git
// discovery walks past the source root when there is no `.git` — so a release
// tarball extracted inside an unrelated repository (an AUR clone's src/ tree,
// say) would get stamped with that repository's HEAD. Anything above this
// directory is therefore never consulted.
//
// Kept in sync with runtime_control/build.rs (which sits one level deeper).
fn repo_root() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").ok()?);
    Some(manifest_dir.parent()?.to_path_buf())
}

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
    // Only trust git when `.git` exists at the expected repo root; vergen and
    // `git rev-parse` both search upward from the crate dir, so the nearest
    // repository they find is then guaranteed to be ours and not an enclosing
    // one. Release tags are `v<semver>`, hence the `v[0-9]*` match — without it
    // describe never matches a tag and degrades to a bare commit hash.
    let in_repo = repo_root().is_some_and(|root| root.join(".git").exists());

    let git_ok = in_repo && {
        emit_git_rerun_triggers();

        let gitcl_res = GitclBuilder::default()
            .describe(true, true, Some("v[0-9]*"))
            .build()
            .map_err(anyhow::Error::from)
            .and_then(|gitcl| {
                Emitter::default()
                    .idempotent()
                    .fail_on_error()
                    .add_instructions(&gitcl)?
                    .emit()
            });

        if let Err(e) = &gitcl_res {
            eprintln!("Warning: Failed to generate git information: {e:?}");
            eprintln!("Using fallback version information");
        }
        gitcl_res.is_ok()
    };

    if !git_ok {
        // Tarball build (or broken repo): stamp the package version rather
        // than letting the binary claim a commit it was not built from.
        println!(
            "cargo:rustc-env=VERGEN_GIT_DESCRIBE=v{}",
            env::var("CARGO_PKG_VERSION")?
        );
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

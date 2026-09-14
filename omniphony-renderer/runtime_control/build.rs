use anyhow::Result;
use chrono::TimeZone;
use std::env;
use std::path::PathBuf;
use std::process::Command;
use vergen_gitcl::{Emitter, GitclBuilder};

// The repository root, resolved relative to this crate instead of by git's
// upward discovery — this crate sits two levels below it. See the twin in the
// root build.rs for why upward discovery must not be trusted (tarball builds
// nested inside an unrelated repository); both files are kept in sync.
fn repo_root() -> Option<PathBuf> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").ok()?);
    Some(manifest_dir.parent()?.parent()?.to_path_buf())
}

// Resolve a path inside the git directory, if it exists. See the twin in the
// root build.rs for why this exists; both are kept in sync.
fn git_path(arg: &str) -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--git-path", arg])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
    // A missing `rerun-if-changed` target is perpetually dirty to cargo.
    path.exists().then_some(path)
}

// Declare the git HEAD as an input of this build script.
//
// A build script emitting no `rerun-if-changed` is re-run only when a file in
// its own package changes. This stamp depends on HEAD, not on this package's
// sources, so it used to freeze at whatever commit was checked out the last
// time `runtime_control/` happened to be edited — which is exactly how
// liborender came to report a commit three days older than the tree it was
// built from, while claiming to be a fresh build.
fn emit_git_rerun_triggers() {
    // Detached HEAD holds the commit id directly; a branch switch rewrites it.
    if let Some(path) = git_path("HEAD") {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    // On a branch the id lives in the ref file, which is what a commit updates.
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

    // After `git gc` the loose ref is packed away and the id lives here.
    if let Some(path) = git_path("packed-refs") {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

// Stamp this crate with a git-describe + build timestamp, exactly like the
// `orender` binary crate's root build.rs. `runtime_control` is linked by BOTH
// hosts (the standalone `orender` binary and the `liborender` cdylib embedded
// in mpv), so broadcasting this fingerprint over OSC lets Studio show the
// connected renderer's real build in About — making a liborender-vs-orender
// version skew immediately visible.
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

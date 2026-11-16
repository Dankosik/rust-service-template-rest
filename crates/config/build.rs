//! Stamp the full git revision into this crate as `VERGEN_GIT_SHA`.
//!
//! Binaries read it through [`service_config::BuildInfo::from_package_version`].
//! The image build has no `.git`; it passes the revision in `VERGEN_GIT_SHA`,
//! which wins when nonblank. The name predates this script and stays, because
//! the Dockerfile, CI and derived services' pipelines set it. Without either
//! source the stamp is `unknown`, never a placeholder that telemetry would
//! publish as if it were a revision.

use std::process::Command;

const STAMP: &str = "VERGEN_GIT_SHA";

fn main() {
    println!("cargo:rerun-if-env-changed={STAMP}");
    let revision = std::env::var(STAMP)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(checked_out_revision)
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env={STAMP}={revision}");
}

/// `HEAD`'s full SHA, rebuilding when `HEAD` moves to another branch or its
/// branch gains a commit. `--git-path` resolves both inside a linked worktree.
fn checked_out_revision() -> Option<String> {
    let revision = git(&["rev-parse", "HEAD"])?;
    let mut watched = vec!["HEAD".to_owned(), "packed-refs".to_owned()];
    watched.extend(git(&["symbolic-ref", "-q", "HEAD"]));
    for name in watched {
        // Cargo reruns on every build for a path that does not exist.
        if let Some(path) = git(&["rev-parse", "--git-path", &name])
            && std::path::Path::new(&path).exists()
        {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    Some(revision)
}

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

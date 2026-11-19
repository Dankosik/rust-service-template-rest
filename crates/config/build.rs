#[expect(
    clippy::disallowed_types,
    reason = "a build script runs before any async runtime exists"
)]
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

#[expect(
    clippy::disallowed_methods,
    reason = "a build script runs before any async runtime exists"
)]
fn checked_out_revision() -> Option<String> {
    let revision = git(&["rev-parse", "HEAD"])?;
    let symbolic_ref = git(&["symbolic-ref", "-q", "HEAD"]);
    for name in ["HEAD", "packed-refs"]
        .into_iter()
        .chain(symbolic_ref.as_deref())
    {
        if let Some(path) = git(&["rev-parse", "--git-path", name])
            && std::path::Path::new(&path).exists()
        {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    Some(revision)
}

#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "a build script runs before any async runtime exists"
)]
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

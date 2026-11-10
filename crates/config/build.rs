//! Stamp the git revision into this crate as `VERGEN_GIT_SHA`.
//!
//! Binaries read it through [`service_config::BuildInfo::from_package_version`].
//! The image build has no `.git`; it sets `VERGEN_GIT_SHA` from its `VCS_REF`
//! argument and vergen emits that value verbatim. Without either, vergen
//! stamps its `VERGEN_IDEMPOTENT_OUTPUT` placeholder, which telemetry would
//! publish as if it were a revision, so this script restamps it `unknown`.

const VERGEN_PLACEHOLDER: &str = "VERGEN_IDEMPOTENT_OUTPUT";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `sha(false)` means the full SHA, not "disable SHA". Short SHA is the
    // other polarity. Empty git still falls back via `default_on_error`.
    let gitcl = vergen_gitcl::Gitcl::builder().sha(false).build();
    let mut emitter = vergen_gitcl::Emitter::default();
    emitter
        .default_on_error()
        .add_instructions(&gitcl)?
        .emit()?;
    let placeholder_sha = emitter
        .cargo_rustc_env_map()
        .iter()
        .any(|(key, value)| key.name() == "VERGEN_GIT_SHA" && value == VERGEN_PLACEHOLDER);
    if placeholder_sha {
        // Cargo applies `rustc-env` instructions in order, so this one wins.
        println!("cargo:rustc-env=VERGEN_GIT_SHA=unknown");
    }
    Ok(())
}

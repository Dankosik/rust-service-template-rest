//! Stamp the git revision as `VERGEN_GIT_SHA` (see `crates/service/build.rs`)
//! and re-run when the migration set changes.
//!
//! `sqlx::migrate!` tracks the files it embedded through `include_str!`,
//! which notices an edit but not a new file; the directory dependency below
//! is what makes a freshly added migration rebuild the binary.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=../../migrations");
    let gitcl = vergen_gitcl::Gitcl::builder().sha(false).build();
    vergen_gitcl::Emitter::default()
        .default_on_error()
        .add_instructions(&gitcl)?
        .emit()?;
    Ok(())
}

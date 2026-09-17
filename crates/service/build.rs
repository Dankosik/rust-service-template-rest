//! Stamp the git revision into the binary as `VERGEN_GIT_SHA`.
//!
//! The image build has no `.git`; it sets `VERGEN_GIT_SHA` from its `VCS_REF`
//! argument and vergen emits that value verbatim. Without either, the
//! fallback keeps `env!` compiling and the config layer reports `unknown`.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let gitcl = vergen_gitcl::Gitcl::builder().sha(false).build();
    vergen_gitcl::Emitter::default()
        .default_on_error()
        .add_instructions(&gitcl)?
        .emit()?;
    Ok(())
}

//! Re-run when the migration set changes.
//!
//! `sqlx::migrate!` tracks the files it embedded through `include_str!`,
//! which notices an edit but not a new file; the directory dependency below
//! is what makes a freshly added migration rebuild the binary. Git revision
//! stamping lives in `crates/config/build.rs`.

#[allow(
    clippy::print_stdout,
    reason = "Cargo build script owns the rerun directive on stdout before runtime execution"
)]
fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}

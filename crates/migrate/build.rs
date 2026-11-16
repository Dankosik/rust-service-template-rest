//! `sqlx::migrate!` tracks embedded files through `include_str!`, which notices
//! an edit but not a new file; the directory dependency below rebuilds the
//! binary for a freshly added migration.

fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}

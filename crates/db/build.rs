// sqlx::migrate! embeds the migrations at compile time; without this, adding
// a migration file doesn't trigger a rebuild and binaries ship stale schema.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}

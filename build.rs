use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=Cargo.lock");

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let lockfile = fs::read_to_string(manifest_dir.join("Cargo.lock"))
        .expect("Cargo.lock must be available when building seiza-server");
    let entry = locked_package_entry(&lockfile, "seiza")
        .expect("Cargo.lock must contain the seiza dependency");
    let version = quoted_field(entry, "version").expect("seiza must have a locked version");
    let label = quoted_field(entry, "source")
        .and_then(git_revision)
        .map_or_else(
            || version.to_owned(),
            |revision| format!("{version}+git.{}", &revision[..8]),
        );
    println!("cargo:rustc-env=SEIZA_DEP_VERSION={label}");
}

fn locked_package_entry<'a>(lockfile: &'a str, package: &str) -> Option<&'a str> {
    lockfile
        .split("[[package]]")
        .skip(1)
        .find(|entry| quoted_field(entry, "name") == Some(package))
}

fn git_revision(source: &str) -> Option<&str> {
    let revision = source.rsplit_once('#')?.1;
    (revision.len() >= 8 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(revision)
}

fn quoted_field<'a>(entry: &'a str, field: &str) -> Option<&'a str> {
    let prefix = format!("{field} = \"");
    entry.lines().find_map(|line| {
        line.trim()
            .strip_prefix(&prefix)
            .and_then(|value| value.strip_suffix('"'))
    })
}

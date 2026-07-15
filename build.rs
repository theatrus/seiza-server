use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=Cargo.lock");

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let lockfile = fs::read_to_string(manifest_dir.join("Cargo.lock"))
        .expect("Cargo.lock must be available when building seiza-server");
    let version = locked_package_version(&lockfile, "seiza")
        .expect("Cargo.lock must contain the seiza dependency");
    println!("cargo:rustc-env=SEIZA_DEP_VERSION={version}");
}

fn locked_package_version<'a>(lockfile: &'a str, package: &str) -> Option<&'a str> {
    lockfile.split("[[package]]").skip(1).find_map(|entry| {
        (quoted_field(entry, "name") == Some(package))
            .then(|| quoted_field(entry, "version"))
            .flatten()
    })
}

fn quoted_field<'a>(entry: &'a str, field: &str) -> Option<&'a str> {
    let prefix = format!("{field} = \"");
    entry.lines().find_map(|line| {
        line.trim()
            .strip_prefix(&prefix)
            .and_then(|value| value.strip_suffix('"'))
    })
}

use std::{env, fs, path::PathBuf};

fn main() {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("integration-manifests");
    println!("cargo:rerun-if-changed={}", manifest_dir.display());

    let mut paths = fs::read_dir(&manifest_dir)
        .expect("integration-manifests directory should exist")
        .map(|entry| {
            entry
                .expect("integration manifest entry should be readable")
                .path()
        })
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    paths.sort();

    let mut generated =
        String::from("pub(crate) const BUILTIN_INTEGRATION_MANIFESTS: &[&str] = &[\n");
    for path in paths {
        let content = fs::read_to_string(&path).expect("integration manifest should be readable");
        generated.push_str(&format!("    {content:?},\n"));
    }
    generated.push_str("];\n");

    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("integration_manifests.rs");
    fs::write(output, generated).expect("generated integration manifest table should be writable");
}

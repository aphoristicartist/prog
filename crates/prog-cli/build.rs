use std::{env, fs, path::PathBuf};

fn main() {
    let integrations = read_manifests("integration-manifests");
    let mut generated =
        String::from("pub(crate) const BUILTIN_INTEGRATION_MANIFESTS: &[&str] = &[\n");
    for (_, content) in integrations {
        generated.push_str(&format!("    {content:?},\n"));
    }
    generated.push_str("];\n");
    write_table("integration_manifests.rs", generated);

    let lenses = read_manifests("assets/lenses");
    let mut generated =
        String::from("pub(crate) const BUILTIN_LENS_MANIFESTS: &[(&str, &str)] = &[\n");
    for (name, content) in lenses {
        generated.push_str(&format!("    ({name:?}, {content:?}),\n"));
    }
    generated.push_str("];\n");
    write_table("lens_manifests.rs", generated);
}

fn read_manifests(directory: &str) -> Vec<(String, String)> {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join(directory);
    println!("cargo:rerun-if-changed={}", manifest_dir.display());

    let mut paths = fs::read_dir(&manifest_dir)
        .expect("manifest directory should exist")
        .map(|entry| entry.expect("manifest entry should be readable").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    paths.sort();

    paths
        .into_iter()
        .map(|path| {
            let content = fs::read_to_string(&path).expect("manifest should be readable");
            let name = path
                .file_name()
                .unwrap()
                .to_str()
                .expect("manifest filename should be UTF-8");
            (name.to_string(), content)
        })
        .collect()
}

fn write_table(name: &str, generated: String) {
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join(name);
    fs::write(output, generated).expect("generated manifest table should be writable");
}

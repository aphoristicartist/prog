use std::{fs, path::PathBuf};

use serde_json::Value;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("manifest should be under crates/prog-cli")
        .to_path_buf()
}

fn read_json(path: &std::path::Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).expect("manifest should be readable"))
        .expect("manifest should contain JSON")
}

#[test]
fn codex_plugin_and_marketplace_are_self_consistent() {
    let root = repo_root();
    let plugin_root = root.join("plugins/prog");
    let plugin = read_json(&plugin_root.join(".codex-plugin/plugin.json"));
    let marketplace = read_json(&root.join(".agents/plugins/marketplace.json"));

    assert_eq!(plugin["name"], "prog");
    let version = plugin["version"].as_str().expect("plugin version");
    assert!(
        version == env!("CARGO_PKG_VERSION")
            || version.starts_with(&format!("{}+codex.", env!("CARGO_PKG_VERSION"))),
        "Codex plugin version {version} must follow the workspace version"
    );
    assert_eq!(plugin["skills"], "./skills/");
    assert_eq!(
        plugin["description"],
        "Agent-harness extension for bounded, cursor-backed tool results"
    );
    assert_eq!(marketplace["name"], "personal");
    assert_eq!(marketplace["plugins"][0]["name"], "prog");
    assert_eq!(
        marketplace["plugins"][0]["source"]["path"],
        "./plugins/prog"
    );

    let canonical_skill = fs::read_to_string(root.join("skills/prog/SKILL.md")).unwrap();
    let plugin_skill = fs::read_to_string(plugin_root.join("skills/prog/SKILL.md")).unwrap();
    assert_eq!(plugin_skill, canonical_skill);

    for relative in ["scripts/prog-run.sh", "scripts/doctor.sh"] {
        let path = plugin_root.join(relative);
        assert!(path.is_file(), "missing {}", path.display());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o111,
                0,
                "{} must be executable",
                path.display()
            );
        }
    }
    assert!(
        fs::read_to_string(plugin_root.join("scripts/doctor.sh"))
            .unwrap()
            .contains("prog.harness.doctor")
    );
}

#[test]
fn deepseek_harness_package_declares_a_native_bundle() {
    let root = repo_root();
    let extension_root = root.join("extensions/deepseek-harness");
    let package = read_json(&extension_root.join("package.json"));

    assert_eq!(package["name"], "@aphoristicartist/dsh-prog");
    assert_eq!(package["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(package["type"], "module");
    assert_eq!(package["main"], "./index.js");
    assert_eq!(package["dsh"]["bundle"]["patch"], "./cordis.patch.yml");
    assert!(package["peerDependencies"]["@deepseek-ai/cordis"].is_string());
    assert!(package["peerDependencies"]["@deepseek-ai/dsh-tools"].is_string());

    let patch = fs::read_to_string(extension_root.join("cordis.patch.yml")).unwrap();
    assert!(patch.contains("name: '@aphoristicartist/dsh-prog'"));
    assert!(patch.contains("minBytes: 16384"));

    let implementation = fs::read_to_string(extension_root.join("index.js")).unwrap();
    assert!(implementation.contains("tools/post-execute"));
    assert!(implementation.contains("shell: false"));
    assert!(implementation.contains("const decision = await next()"));
}

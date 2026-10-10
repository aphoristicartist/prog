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
    let packaged_skill =
        fs::read_to_string(root.join("crates/prog-cli/assets/prog-skill.md")).unwrap();
    assert_eq!(plugin_skill, canonical_skill);
    assert_eq!(packaged_skill, canonical_skill);
    assert_eq!(
        fs::read_to_string(plugin_root.join("LICENSE")).unwrap(),
        fs::read_to_string(root.join("LICENSE")).unwrap()
    );

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
    let lock = read_json(&extension_root.join("package-lock.json"));

    assert_eq!(package["name"], "@aphoristicartist/dsh-prog");
    assert_eq!(package["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(lock["version"], package["version"]);
    assert_eq!(lock["packages"][""]["version"], package["version"]);
    assert_eq!(
        lock["packages"][""]["peerDependencies"],
        package["peerDependencies"]
    );
    assert_eq!(package["type"], "module");
    assert_eq!(package["engines"]["node"], "^22.19.0 || >=24.0.0");
    assert_eq!(package["main"], "./index.js");
    assert_eq!(package["dsh"]["bundle"]["patch"], "./cordis.patch.yml");
    assert_eq!(package["peerDependencies"]["@deepseek-ai/cordis"], "^4.0.1");
    assert_eq!(
        package["peerDependencies"]["@deepseek-ai/dsh-tools"],
        "^0.1.0-rc.6"
    );
    assert_eq!(
        package["peerDependencies"]["@deepseek-ai/dsh-subprocess"],
        "^0.1.0-rc.6"
    );
    assert_eq!(
        fs::read_to_string(extension_root.join("LICENSE")).unwrap(),
        fs::read_to_string(root.join("LICENSE")).unwrap()
    );

    let patch = fs::read_to_string(extension_root.join("cordis.patch.yml")).unwrap();
    assert!(patch.contains("name: '@aphoristicartist/dsh-prog'"));
    assert!(patch.contains("minBytes: 16384"));

    let implementation = fs::read_to_string(extension_root.join("index.js")).unwrap();
    assert!(implementation.contains("tools/post-execute"));
    assert!(implementation.contains("shell: false"));
    assert!(implementation.contains("const decision = await next()"));
}

#[test]
fn claude_code_plugin_and_marketplace_are_self_consistent() {
    let root = repo_root();
    let plugin_root = root.join("plugins/prog-claude");
    let plugin = read_json(&plugin_root.join(".claude-plugin/plugin.json"));
    let marketplace = read_json(&root.join(".claude-plugin/marketplace.json"));
    assert_eq!(plugin["name"], "prog");
    assert_eq!(plugin["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(plugin["userConfig"]["stop_gate"]["type"], "boolean");
    assert_eq!(plugin["userConfig"]["stop_gate"]["default"], false);
    assert_eq!(marketplace["plugins"][0]["name"], "prog");
    assert_eq!(marketplace["plugins"][0]["source"], "./plugins/prog-claude");

    // Every hook event routes to the single tested entry point.
    let hooks = read_json(&plugin_root.join("hooks/hooks.json"));
    for event in ["SessionStart", "PostToolUse", "PostToolUseFailure", "Stop"] {
        let command = hooks["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap_or_else(|| panic!("missing {event} hook"));
        assert_eq!(
            command, "\"${CLAUDE_PLUGIN_ROOT}\"/scripts/hook.sh",
            "{event}"
        );
    }
    for event in ["PostToolUse", "PostToolUseFailure"] {
        assert_eq!(hooks["hooks"][event][0]["matcher"], "Bash", "{event}");
    }
    let script = plugin_root.join("scripts/hook.sh");
    assert!(
        fs::read_to_string(&script)
            .unwrap()
            .contains("harness hook claude-code")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            fs::metadata(&script).unwrap().permissions().mode() & 0o111,
            0,
            "hook.sh must be executable"
        );
    }
    assert_eq!(
        fs::read_to_string(plugin_root.join("skills/prog/SKILL.md")).unwrap(),
        fs::read_to_string(root.join("skills/prog/SKILL.md")).unwrap()
    );
    assert_eq!(
        fs::read_to_string(plugin_root.join("LICENSE")).unwrap(),
        fs::read_to_string(root.join("LICENSE")).unwrap()
    );
}

/// #292: the default skill teaches one loop; the full guidance lives in
/// docs/agent-reference.md.
#[test]
fn default_skill_stays_small_and_points_to_the_full_reference() {
    let root = repo_root();
    let skill = fs::read_to_string(root.join("skills/prog/SKILL.md")).unwrap();
    assert!(
        skill.len() <= 1_536,
        "skill is {} bytes; keep it at or under 1,536",
        skill.len()
    );
    assert!(skill.contains("NOT PROVEN"));
    assert!(skill.contains("prog evidence"));
    assert!(skill.contains("docs/agent-reference.md"));
    let reference = fs::read_to_string(root.join("docs/agent-reference.md")).unwrap();
    for guidance in [
        "## Verifying a Fix",
        "prog session obligation-add",
        "## Long-Running MCP Tasks",
        "## EvidenceRefs",
    ] {
        assert!(reference.contains(guidance), "reference lost: {guidance}");
    }
}

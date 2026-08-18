//! Fixed agent-integration context budget for the current CLI/skill surface.

//! The ceiling counts only context that exists before an agent performs work:
//! the top-level help, one help response for every immediate command, and the
//! portable skill. It deliberately does not hide response bytes; #139 accounts
//! for those separately in live trials.

mod support;

use support::{prog, repo_root, stdout};

const MAX_IMMEDIATE_SURFACE_BYTES: usize = 34_000;

#[test]
fn immediate_agent_surface_stays_below_the_reviewed_context_budget() {
    let top = prog(&["--help"]);
    assert!(top.status.success(), "{}", stdout(&top));
    assert_eq!(support::stderr(&top), "");

    let commands = immediate_commands(&stdout(&top));
    assert!(
        commands.len() >= 20,
        "the command parser should find the current command tree"
    );
    assert_eq!(
        commands,
        [
            "harness",
            "discover",
            "source",
            "route",
            "hints",
            "call",
            "observe",
            "run",
            "recipe",
            "init",
            "update",
            "cost",
            "paths",
            "inspect",
            "evidence",
            "search",
            "find",
            "delta",
            "status",
            "verification",
            "mcp-task",
            "session",
            "expand",
            "cache",
            "meta",
        ]
    );

    let mut surface_bytes = top.stdout.len();
    for command in &commands {
        let help = prog(&[command, "--help"]);
        assert!(help.status.success(), "{}", stdout(&help));
        assert_eq!(support::stderr(&help), "");
        surface_bytes += help.stdout.len();
    }

    let skill = std::fs::read(repo_root().join("skills/prog/SKILL.md")).unwrap();
    surface_bytes += skill.len();
    assert!(
        surface_bytes <= MAX_IMMEDIATE_SURFACE_BYTES,
        "fixed agent integration context is {surface_bytes} bytes; reduce the surface or review the budget"
    );
}

fn immediate_commands(help: &str) -> Vec<String> {
    let (commands, _) = help
        .split_once("Commands:")
        .and_then(|(_, remainder)| remainder.split_once("Options:"))
        .unwrap_or_else(|| panic!("help should contain Commands and Options sections:\n{help}"));
    commands
        .lines()
        .filter_map(|line| {
            let name = line.split_whitespace().next()?;
            (name != "help" && !name.starts_with('-')).then(|| name.to_string())
        })
        .collect()
}

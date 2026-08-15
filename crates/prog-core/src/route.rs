use std::path::Path;

use crate::{Extra, ROUTE_SCHEMA, RouteAssessment, RouteGuidance, RoutePolicy};

/// Classify an argv vector without executing, parsing a shell string, or
/// changing any argument. Exact authored policy rules always win.
pub fn classify_route(argv: &[String], policy: &RoutePolicy) -> RouteAssessment {
    if let Some(rule) = policy.rules.iter().find(|rule| rule.argv == argv) {
        return assessment(
            rule.guidance,
            Some(rule.id.clone()),
            "exact policy rule matched",
            argv.len(),
        );
    }
    let Some(program) = argv.first().and_then(|value| program_name(value)) else {
        return assessment(
            RouteGuidance::Unknown,
            None,
            "no argv was supplied",
            argv.len(),
        );
    };
    if program == "prog" {
        return assessment(
            RouteGuidance::Passthrough,
            Some("builtin.nested_prog".to_string()),
            "nested prog invocations pass through unchanged",
            argv.len(),
        );
    }
    if shell_structure_is_opaque(program, &argv[1..]) {
        return assessment(
            RouteGuidance::Passthrough,
            Some("builtin.opaque_shell".to_string()),
            "shell, pipeline, redirect, substitution, heredoc, or TTY structure is not re-parsed",
            argv.len(),
        );
    }
    if noisy_command(program, &argv[1..]) {
        return assessment(
            RouteGuidance::Progressive,
            Some("builtin.noisy_command".to_string()),
            "command family commonly emits repeated or high-volume evidence",
            argv.len(),
        );
    }
    if tiny_command(program, &argv[1..]) {
        return assessment(
            RouteGuidance::Raw,
            Some("builtin.tiny_command".to_string()),
            "command family normally returns a small direct result",
            argv.len(),
        );
    }
    assessment(
        RouteGuidance::Unknown,
        None,
        "no exact policy or conservative built-in rule matched",
        argv.len(),
    )
}

fn assessment(
    guidance: RouteGuidance,
    matched_rule: Option<String>,
    reason: &str,
    argv_count: usize,
) -> RouteAssessment {
    RouteAssessment {
        schema: ROUTE_SCHEMA.to_string(),
        guidance,
        matched_rule,
        reason: reason.to_string(),
        argv_count: argv_count.try_into().unwrap_or(u64::MAX),
        wrapper_prefix: (guidance == RouteGuidance::Progressive)
            .then(|| vec!["prog".to_string(), "run".to_string(), "--".to_string()]),
        preserves_authored_argv: true,
        semantic_substitution_allowed: false,
        extra: Extra::new(),
    }
}

fn program_name(value: &str) -> Option<&str> {
    Path::new(value).file_name()?.to_str()
}

fn shell_structure_is_opaque(program: &str, args: &[String]) -> bool {
    if matches!(
        program,
        "sh" | "bash" | "zsh" | "fish" | "csh" | "tcsh" | "pwsh" | "powershell" | "cmd"
    ) {
        return true;
    }
    if matches!(program, "docker" | "podman")
        && args.iter().any(|arg| {
            matches!(
                arg.as_str(),
                "-i" | "-t" | "-it" | "-ti" | "--interactive" | "--tty"
            )
        })
    {
        return true;
    }
    if program == "kubectl"
        && args.first().is_some_and(|arg| arg == "logs")
        && args
            .iter()
            .any(|arg| matches!(arg.as_str(), "-f" | "--follow"))
    {
        return true;
    }
    args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "|" | "||" | "&&" | ";" | ">" | ">>" | "<" | "<<" | "<<<" | "&"
        ) || arg.contains("$(")
            || arg.contains('`')
            || arg.starts_with("<<")
    })
}

fn noisy_command(program: &str, args: &[String]) -> bool {
    let first = args.first().map(String::as_str);
    match program {
        "pytest" | "py.test" | "rustc" => true,
        "cargo" => first.is_some_and(|arg| {
            matches!(arg, "test" | "check" | "clippy" | "build" | "bench" | "run")
        }),
        "go" => first == Some("test"),
        "npm" | "pnpm" | "yarn" | "bun" => {
            first.is_some_and(|arg| matches!(arg, "test" | "run" | "audit"))
        }
        "git" => first.is_some_and(|arg| matches!(arg, "diff" | "log" | "show")),
        "docker" | "podman" => first == Some("logs"),
        "kubectl" => first.is_some_and(|arg| matches!(arg, "get" | "logs" | "describe")),
        _ => false,
    }
}

fn tiny_command(program: &str, args: &[String]) -> bool {
    if matches!(
        program,
        "true" | "false" | "pwd" | "whoami" | "date" | "uname" | "echo" | "printf"
    ) {
        return true;
    }
    matches!(
        (program, args.first().map(String::as_str)),
        ("git", Some("status" | "rev-parse" | "branch"))
            | (
                "cargo" | "rustc" | "python" | "python3" | "node",
                Some("--version" | "-V")
            )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn exact_policy_precedes_every_builtin() {
        let command = argv(&["cargo", "test"]);
        let policy = RoutePolicy {
            rules: vec![crate::RouteRule {
                id: "owner.raw-cargo".to_string(),
                argv: command.clone(),
                guidance: RouteGuidance::Raw,
            }],
        };
        let result = classify_route(&command, &policy);
        assert_eq!(result.guidance, RouteGuidance::Raw);
        assert_eq!(result.matched_rule.as_deref(), Some("owner.raw-cargo"));
    }

    #[test]
    fn route_table_is_conservative_and_never_substitutes_argv() {
        let cases = [
            (argv(&["pytest", "-q"]), RouteGuidance::Progressive),
            (argv(&["cargo", "test"]), RouteGuidance::Progressive),
            (argv(&["pwd"]), RouteGuidance::Raw),
            (
                argv(&["prog", "run", "--", "pytest"]),
                RouteGuidance::Passthrough,
            ),
            (
                argv(&["bash", "-c", "pytest | tee out"]),
                RouteGuidance::Passthrough,
            ),
            (
                argv(&["cargo", "test", "|", "tee"]),
                RouteGuidance::Passthrough,
            ),
            (argv(&["custom-tool", "scan"]), RouteGuidance::Unknown),
        ];
        for (command, expected) in cases {
            let first = classify_route(&command, &RoutePolicy::default());
            let second = classify_route(&command, &RoutePolicy::default());
            assert_eq!(first, second);
            assert_eq!(first.guidance, expected, "{command:?}");
            assert!(first.preserves_authored_argv);
            assert!(!first.semantic_substitution_allowed);
        }
    }
}

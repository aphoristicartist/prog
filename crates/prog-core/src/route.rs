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

/// Whether argv runs a test, build, type-check, or lint tool. Their output is
/// verification evidence with its own parsers and verdicts, so it is never
/// reinterpreted as a generic record collection, even when it is JSON.
pub fn is_verification_command(argv: &[String]) -> bool {
    let Some(program) = argv.first().and_then(|value| program_name(value)) else {
        return false;
    };
    let first = argv.get(1).map(String::as_str);
    match program {
        "pytest" | "py.test" | "jest" | "vitest" | "mocha" | "rustc" | "tsc" | "mypy"
        | "pyright" | "eslint" | "ruff" | "biome" | "cargo" | "make" | "ctest" | "gradle"
        | "mvn" | "rspec" | "phpunit" => true,
        "go" => first.is_some_and(|arg| matches!(arg, "test" | "build" | "vet")),
        "deno" => first.is_some_and(|arg| matches!(arg, "test" | "lint" | "check")),
        "npm" | "pnpm" | "yarn" | "bun" => {
            first.is_some_and(|arg| matches!(arg, "test" | "run" | "exec" | "x"))
                || (matches!(program, "pnpm" | "yarn") && launches_test_runner(&argv[1..]))
        }
        "npx" | "bunx" | "pnpx" | "uv" | "uvx" | "poetry" | "python" | "python3" => {
            launches_test_runner(&argv[1..])
                || argv
                    .iter()
                    .skip(1)
                    .filter_map(|arg| program_name(arg))
                    .any(|arg| {
                        matches!(
                            arg,
                            "pytest"
                                | "jest"
                                | "vitest"
                                | "mypy"
                                | "ruff"
                                | "eslint"
                                | "tsc"
                                | "unittest"
                        )
                    })
        }
        "playwright" => first == Some("test"),
        "dotnet" => first.is_some_and(|arg| matches!(arg, "test" | "build")),
        _ => false,
    }
}

fn program_name(value: &str) -> Option<&str> {
    Path::new(value).file_name()?.to_str()
}

/// Whether `args` (what follows `npx`, `pnpm exec`, or `pnpm` itself) starts
/// a known test runner: Jest, Vitest, or `playwright test`. Only the first
/// non-flag argument names the package, so `npx -p jest other` is not one.
fn launches_test_runner(args: &[String]) -> bool {
    let mut rest = args.iter().filter(|arg| !arg.starts_with('-'));
    match rest.next().map(String::as_str) {
        Some("jest" | "vitest") => true,
        Some("playwright") => rest.next().is_some_and(|arg| arg == "test"),
        _ => false,
    }
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
        "pytest" | "py.test" | "rustc" | "jest" => true,
        "tsc" | "trivy" | "semgrep" => !version_or_help_query(args),
        "cargo" => first.is_some_and(|arg| {
            matches!(arg, "test" | "check" | "clippy" | "build" | "bench" | "run")
        }),
        "go" => first == Some("test"),
        "deno" => first.is_some_and(|arg| matches!(arg, "test" | "lint" | "check")),
        // Package scripts, and test runners started through the manager:
        // `pnpm exec vitest`, `npm x jest`, `pnpm vitest`, `yarn jest`.
        "npm" | "pnpm" | "yarn" | "bun" => {
            first.is_some_and(|arg| matches!(arg, "test" | "run" | "audit"))
                || (matches!(first, Some("exec" | "x")) && launches_test_runner(&args[1..]))
                || (matches!(program, "pnpm" | "yarn") && launches_test_runner(args))
        }
        "git" => first.is_some_and(|arg| matches!(arg, "diff" | "log" | "show")),
        "docker" | "podman" => first == Some("logs"),
        "kubectl" => first.is_some_and(|arg| matches!(arg, "get" | "logs" | "describe")),
        "vitest" => first == Some("run"),
        // Test runners reached through an interpreter or package runner.
        "python" | "python3" => {
            first == Some("-m")
                && matches!(args.get(1).map(String::as_str), Some("pytest" | "unittest"))
        }
        _ if program.strip_prefix("python3.").is_some_and(|minor| {
            !minor.is_empty() && minor.chars().all(|ch| ch.is_ascii_digit())
        }) =>
        {
            first == Some("-m")
                && matches!(args.get(1).map(String::as_str), Some("pytest" | "unittest"))
        }
        "npx" | "bunx" | "pnpx" => launches_test_runner(args),
        "playwright" => first == Some("test"),
        "uv" => first == Some("run"),
        "ruff" => first == Some("check"),
        "biome" => first.is_some_and(|arg| matches!(arg, "check" | "ci" | "lint")),
        "terraform" | "tofu" => first.is_some_and(|arg| matches!(arg, "plan" | "show")),
        "gh" => {
            (first == Some("api"))
                || (first == Some("run")
                    && args
                        .iter()
                        .any(|arg| matches!(arg.as_str(), "--log" | "--log-failed")))
        }
        _ => false,
    }
}

fn version_or_help_query(args: &[String]) -> bool {
    args.iter().take(2).any(|arg| {
        matches!(
            arg.as_str(),
            "--version" | "-v" | "-V" | "--help" | "-h" | "help" | "version"
        )
    })
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
                "cargo" | "rustc" | "python" | "python3" | "node" | "tsc" | "trivy" | "semgrep",
                Some("--version" | "-V")
            )
    )
}

#[cfg(test)]
mod verification_command_tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_string()).collect()
    }

    #[test]
    fn test_and_build_runners_are_verification_commands() {
        for command in [
            &["go", "test", "-json", "./..."][..],
            &["/usr/bin/cargo", "test"],
            &["npx", "vitest", "run"],
            &["uv", "run", "pytest", "-q"],
            &["python3", "-m", "pytest"],
            &["npm", "test"],
            &["jest", "--json"],
            &["pnpm", "exec", "vitest", "run"],
            &["pnpm", "vitest"],
            &["npx", "playwright", "test"],
            &["uv", "run", "python", "-m", "unittest"],
        ] {
            assert!(is_verification_command(&argv(command)), "{command:?}");
        }
        for command in [
            &["gh", "api", "repos/o/r/issues"][..],
            &["kubectl", "get", "pods", "-o", "json"],
            &["curl", "-s", "https://example.com"],
            &["go", "list", "-json"],
            &["python3", "export.py"],
            &["npx", "playwright", "install"],
            &["pnpm", "install"],
            &[],
        ] {
            assert!(!is_verification_command(&argv(command)), "{command:?}");
        }
    }
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
            // 2026 toolchain families use exact program + first-argument
            // matches only, so unknown commands stay unknown.
            (argv(&["vitest", "run"]), RouteGuidance::Progressive),
            (argv(&["playwright", "test"]), RouteGuidance::Progressive),
            (argv(&["deno", "test"]), RouteGuidance::Progressive),
            (argv(&["uv", "run", "pytest"]), RouteGuidance::Progressive),
            (argv(&["ruff", "check", "."]), RouteGuidance::Progressive),
            (argv(&["biome", "check", "."]), RouteGuidance::Progressive),
            (argv(&["tsc", "--noEmit"]), RouteGuidance::Progressive),
            (argv(&["jest", "--ci"]), RouteGuidance::Progressive),
            (
                argv(&["python3", "-m", "unittest", "-v"]),
                RouteGuidance::Progressive,
            ),
            (
                argv(&["python3.12", "-m", "pytest"]),
                RouteGuidance::Progressive,
            ),
            (argv(&["python3", "script.py"]), RouteGuidance::Unknown),
            (
                argv(&["python", "-m", "http.server"]),
                RouteGuidance::Unknown,
            ),
            (
                argv(&["npx", "--yes", "jest", "--json"]),
                RouteGuidance::Progressive,
            ),
            (
                argv(&["npx", "prettier", "--check"]),
                RouteGuidance::Unknown,
            ),
            // Runners started through a package manager (#312).
            (
                argv(&["pnpm", "exec", "vitest", "run"]),
                RouteGuidance::Progressive,
            ),
            (argv(&["npm", "exec", "jest"]), RouteGuidance::Progressive),
            (argv(&["npm", "x", "vitest"]), RouteGuidance::Progressive),
            (argv(&["bun", "x", "jest"]), RouteGuidance::Progressive),
            (argv(&["pnpm", "vitest", "run"]), RouteGuidance::Progressive),
            (argv(&["yarn", "jest", "--ci"]), RouteGuidance::Progressive),
            (
                argv(&["pnpm", "exec", "playwright", "test"]),
                RouteGuidance::Progressive,
            ),
            (
                argv(&["npx", "playwright", "test", "--reporter=line"]),
                RouteGuidance::Progressive,
            ),
            (
                argv(&["npx", "playwright", "install"]),
                RouteGuidance::Unknown,
            ),
            (
                argv(&["pnpm", "exec", "eslint", "."]),
                RouteGuidance::Unknown,
            ),
            (argv(&["npm", "exec", "prettier"]), RouteGuidance::Unknown),
            (argv(&["pnpm", "lint"]), RouteGuidance::Unknown),
            (argv(&["npm", "vitest"]), RouteGuidance::Unknown),
            (argv(&["terraform", "plan"]), RouteGuidance::Progressive),
            (
                argv(&["tofu", "show", "plan.out"]),
                RouteGuidance::Progressive,
            ),
            (argv(&["trivy", "fs", "."]), RouteGuidance::Progressive),
            (
                argv(&["semgrep", "--config", "auto", "."]),
                RouteGuidance::Progressive,
            ),
            (
                argv(&["gh", "run", "view", "--log-failed"]),
                RouteGuidance::Progressive,
            ),
            (
                argv(&["gh", "api", "/repos/prog/prog/issues"]),
                RouteGuidance::Progressive,
            ),
            // Conservative boundaries: watch modes, adjacent subcommands, and
            // version queries never inherit a family classification.
            (argv(&["vitest"]), RouteGuidance::Unknown),
            (argv(&["vitest", "--watch"]), RouteGuidance::Unknown),
            (argv(&["deno", "fmt"]), RouteGuidance::Unknown),
            (argv(&["uv", "sync"]), RouteGuidance::Unknown),
            (argv(&["terraform", "validate"]), RouteGuidance::Unknown),
            (argv(&["gh", "run", "list"]), RouteGuidance::Unknown),
            (argv(&["gh", "issue", "list"]), RouteGuidance::Unknown),
            (argv(&["tsc", "--version"]), RouteGuidance::Raw),
            (argv(&["trivy", "--version"]), RouteGuidance::Raw),
            (argv(&["semgrep", "--version"]), RouteGuidance::Raw),
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

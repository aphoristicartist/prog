//! CLI strategies receive public inputs and preceding stdout only. Grading,
//! fixture names, expected paths, and retained-payload reads live elsewhere.
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub struct Input {
    /// An artifact provided outside model context, passed verbatim to observe.
    pub bytes: Vec<u8>,
    pub goal: String,
}

#[derive(Debug, Clone, Copy)]
pub enum Strategy {
    Paths,
    Findings,
    Inspect,
}

impl Strategy {
    pub const ALL: [Self; 3] = [Self::Paths, Self::Findings, Self::Inspect];
    pub fn name(self) -> &'static str {
        match self {
            Self::Paths => "paths",
            Self::Findings => "findings",
            Self::Inspect => "inspect",
        }
    }
}

#[derive(Default)]
pub struct Options {
    pub pretty: bool,
    /// Public operation metadata; deliberately independent of fixture identity.
    pub name: Option<String>,
    pub extra_navigation: bool,
    pub budget_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// Exact argv with only isolated store and opaque cursor replaced by bindings.
    pub command: Vec<String>,
    pub stdout_bytes: u64,
    pub exit_code: Option<i32>,
    pub offered_paths: Vec<String>,
    #[serde(skip)]
    pub stdout: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub path: String,
    /// Zero-based earlier invocation that offered this exact path.
    pub step: usize,
    /// The response field the strategy was permitted to consult.
    pub field: String,
}

pub struct Execution {
    pub steps: Vec<Step>,
    pub selected: Option<Selection>,
    pub evidence: Option<Value>,
    pub cursor: Option<String>,
    pub evidence_cursor: Option<String>,
    pub evidence_path: Option<String>,
    pub stop: String,
}

impl Execution {
    fn new() -> Self {
        Self {
            steps: vec![],
            selected: None,
            evidence: None,
            cursor: None,
            evidence_cursor: None,
            evidence_path: None,
            stop: "no_candidate".into(),
        }
    }
    pub fn bytes(&self) -> u64 {
        self.steps
            .iter()
            .map(|step| {
                assert_eq!(step.stdout_bytes, step.stdout.len() as u64);
                step.stdout_bytes
            })
            .sum()
    }
    pub fn calls(&self) -> u64 {
        self.steps.len() as u64
    }

    fn invoke(
        &mut self,
        store: &Path,
        options: &Options,
        args: &[&str],
        input: Option<&[u8]>,
    ) -> Value {
        let mut argv = vec!["--dir".to_string(), store.to_str().unwrap().to_string()];
        if options.pretty {
            argv.push("--pretty".into());
        }
        if let Some(bytes) = options.budget_bytes {
            argv.extend(["--budget-bytes".into(), bytes.to_string()]);
        }
        argv.extend(args.iter().map(|s| (*s).to_string()));
        let mut child = Command::new(env!("CARGO_BIN_EXE_prog"))
            .current_dir(store)
            .args(&argv)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(bytes) = input {
            // Evaluation inputs are finite and small; the reader consumes stdin
            // before producing a bounded response.
            child.stdin.take().unwrap().write_all(bytes).unwrap();
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.stderr.is_empty(),
            "unexpected uncounted stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value =
            serde_json::from_slice(&output.stdout).expect("operational stdout must be JSON");
        let offered_paths = ["paths", "findings", "hits"]
            .into_iter()
            .flat_map(|field| value[field].as_array().into_iter().flatten())
            .filter_map(|entry| entry["path"].as_str().map(str::to_string))
            .collect();
        let mut command = vec!["prog".into()];
        command.extend(argv.into_iter().enumerate().map(|(index, arg)| {
            if index == 1 {
                "$STORE".into()
            } else if self.cursor.as_deref() == Some(arg.as_str()) {
                "$CURSOR".into()
            } else {
                arg
            }
        }));
        self.steps.push(Step {
            command,
            stdout_bytes: output.stdout.len() as u64,
            exit_code: output.status.code(),
            offered_paths,
            stdout: output.stdout,
        });
        value
    }

    fn choose(
        &self,
        value: &Value,
        field: &str,
        predicate: impl Fn(&Value) -> bool,
    ) -> Option<Selection> {
        value[field]
            .as_array()?
            .iter()
            .find(|entry| predicate(entry))?["path"]
            .as_str()
            .map(|path| Selection {
                path: path.into(),
                step: self.steps.len() - 1,
                field: field.into(),
            })
    }

    fn paths(&mut self, store: &Path, options: &Options, cursor: &str, prefix: &str) -> Value {
        self.invoke(
            store,
            options,
            &[
                "paths", cursor, "--prefix", prefix, "--limit", "8", "--depth", "2",
            ],
            None,
        )
    }

    fn path_candidate(
        &mut self,
        store: &Path,
        options: &Options,
        cursor: &str,
    ) -> Option<Selection> {
        let mut view = self.paths(store, options, cursor, "");
        // Fixed schema-aware navigation: diagnostic collections in returned path
        // listings, never a scenario-to-answer map. No exhaustive synthetic paths.
        // Up to four bounded listings allow narrowing through runs/results.
        for _ in 0..4 {
            if let Some(candidate) = self.choose(&view, "paths", |entry| {
                let path = entry["path"].as_str().unwrap_or("");
                let parts: Vec<_> = path.split('/').collect();
                parts.len() >= 3
                    && matches!(parts[parts.len() - 2], "failure_sections" | "results")
                    && parts.last().unwrap().parse::<usize>().is_ok()
            }) {
                return Some(candidate);
            }
            if let Some(lines) = self.choose(&view, "paths", |entry| {
                entry["kind"] == "array"
                    && entry["path"]
                        .as_str()
                        .is_some_and(|path| path.ends_with("/lines"))
            }) {
                let searched = self.invoke(
                    store,
                    options,
                    &[
                        "search",
                        cursor,
                        "FATAL|ERROR|error",
                        "--regex",
                        "--path",
                        &lines.path,
                        "--limit",
                        "8",
                    ],
                    None,
                );
                // Search rank is not a causal oracle. This declared heuristic
                // prefers fatal text, otherwise takes the first returned match.
                return self
                    .choose(&searched, "hits", |hit| {
                        hit["preview"].as_str().is_some_and(|s| s.contains("FATAL"))
                    })
                    .or_else(|| self.choose(&searched, "hits", |_| true));
            }
            let prefix = self
                .choose(&view, "paths", |entry| {
                    let path = entry["path"].as_str().unwrap_or("");
                    matches!(
                        path.rsplit('/').next(),
                        Some("failure_sections" | "results")
                    )
                })
                .or_else(|| {
                    self.choose(&view, "paths", |entry| {
                        let path = entry["path"].as_str().unwrap_or("");
                        let parts: Vec<_> = path.split('/').collect();
                        parts.len() >= 3
                            && parts[parts.len() - 2] == "runs"
                            && parts.last().unwrap().parse::<usize>().is_ok()
                    })
                });
            let prefix = prefix?;
            if view["prefix"] == prefix.path {
                return None;
            }
            view = self.paths(store, options, cursor, &prefix.path);
        }
        None
    }
}

pub fn run(store: &Path, input: &Input, strategy: Strategy, options: &Options) -> Execution {
    let mut execution = Execution::new();
    let capture = execution.invoke(
        store,
        options,
        &[
            "observe",
            "--stdin",
            "--mime",
            "application/json",
            "--name",
            options.name.as_deref().unwrap_or("evidence-eval"),
        ],
        Some(&input.bytes),
    );
    let Some(cursor) = capture["cursor"].as_str() else {
        execution.stop = "capture_failed".into();
        return execution;
    };
    execution.cursor = Some(cursor.into());
    if options.extra_navigation {
        execution.paths(store, options, cursor, "");
    }
    // All arms pay for the complete capture, including every finding. The
    // controlled paths/inspect comparisons intentionally do not use those findings.
    let selection = match strategy {
        Strategy::Findings => {
            // Capture remains step zero even when a control adds navigation.
            capture["findings"]
                .as_array()
                .and_then(|entries| entries.first())
                .and_then(|entry| entry["path"].as_str())
                .map(|path| Selection {
                    path: path.into(),
                    step: 0,
                    field: "findings".into(),
                })
        }
        Strategy::Inspect => {
            let inspected = execution.invoke(
                store,
                options,
                &["inspect", cursor, "--goal", &input.goal, "--limit", "5"],
                None,
            );
            execution.choose(&inspected, "findings", |_| true)
        }
        Strategy::Paths => execution.path_candidate(store, options, cursor),
    };
    let Some(selected) = selection else {
        return execution;
    };
    let evidence = execution.invoke(
        store,
        options,
        &["evidence", cursor, "--path", &selected.path],
        None,
    );
    let evidence = if evidence["omitted"]
        .as_array()
        .is_some_and(|omitted| !omitted.is_empty())
    {
        // One declared larger view, triggered only by actual omission metadata.
        // Still insufficient if that view cannot disclose the complete slice.
        execution.invoke(
            store,
            options,
            &[
                "expand",
                cursor,
                "--path",
                &selected.path,
                "--depth",
                "12",
                "--limit",
                "100",
            ],
            None,
        )
    } else {
        evidence
    };
    let field = if evidence.get("excerpt").is_some() {
        "excerpt"
    } else {
        "data_preview"
    };
    execution.evidence = evidence.get(field).cloned();
    execution.evidence_cursor = evidence["cursor"].as_str().map(str::to_string);
    // Expansions carry the selected scope in their evidence reference.
    execution.evidence_path = evidence["path"]
        .as_str()
        .or_else(|| evidence["evidence_ref"]["path"].as_str())
        .map(str::to_string);
    execution.selected = Some(selected);
    execution.stop = if evidence.get("error").is_some() {
        "retrieval_failed"
    } else {
        "candidate_retrieved"
    }
    .into();
    execution
}

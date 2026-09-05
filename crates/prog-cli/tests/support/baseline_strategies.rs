//! Deterministic strategies. This module has no grader/oracle input. A runner
//! returns observations; only its caller can decide whether they contain the
//! expected evidence. Source artifacts are supplied by fixture setup, outside
//! model context, equally for every arm. Only response bytes enter context.
// Each integration-test crate compiles this module independently and uses a
// different subset of the shared strategies and task variants.
#![allow(dead_code)]

use serde::Serialize;
use serde_json::Value;
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
    time::Instant,
};

pub const SEVERITY_PATTERN: &str = r"(?i)\b(fatal|panic|error|failed|exception)\b";
pub const STRATEGIES: [&str; 10] = [
    "raw_context",
    "head_tail_truncation",
    "native_field_selection",
    "rtk_grep_filter",
    "broad_log_search",
    "file_capture_search",
    "caveman_terse_output",
    "prog_envelope_only",
    "prog_retrieve",
    "prog_repeated_cache",
];

#[derive(Clone, Debug, Serialize)]
pub enum Source {
    Call {
        source_id: String,
        operation: String,
    },
    Observe {
        name: String,
        mime: String,
        bytes: Vec<u8>,
    },
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Task {
    /// This selector is part of the public task, not discovered evidence.
    KnownPath {
        selector: String,
        grep_term: Option<String>,
    },
    /// No answer-derived selector, prefix, or search term can be supplied.
    UnknownTarget,
}

#[derive(Clone, Debug, Serialize)]
pub struct StrategyInput {
    pub prompt: String,
    pub source: Source,
    pub raw_bytes: Vec<u8>,
    pub task: Task,
}

impl StrategyInput {
    pub fn selector(&self) -> Option<&str> {
        match &self.task {
            Task::KnownPath { selector, .. } => Some(selector),
            Task::UnknownTarget => None,
        }
    }
    pub fn mode(&self) -> &'static str {
        match self.task {
            Task::KnownPath { .. } => "known_path_recoverability",
            Task::UnknownTarget => "deterministic_discovery",
        }
    }
    fn grep_term(&self) -> Option<&str> {
        match &self.task {
            Task::KnownPath { grep_term, .. } => grep_term.as_deref(),
            // A declared narrow baseline assumption, independent of the fixture.
            Task::UnknownTarget => Some("ERROR"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Step {
    /// Executable and action argv. The constant isolated store/CWD is omitted;
    /// opaque cursor tokens are replaced by a response binding in this trace.
    pub command: Vec<String>,
    pub response_bytes: usize,
    /// Paths actually offered in this response, retained for audit/replay.
    pub finding_paths: Vec<String>,
    pub expansion: bool,
    pub cache_hit: bool,
    #[serde(skip)]
    pub response: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct Execution {
    pub unavailable: Option<String>,
    pub steps: Vec<Step>,
    pub elapsed_ms: u128,
    pub notes: Vec<String>,
    pub selected_path: Option<String>,
    /// Payload evidence, excluding generated metadata. This is not a grade.
    pub evidence: Vec<u8>,
}

impl Execution {
    pub fn response_bytes(&self) -> usize {
        self.steps
            .iter()
            .map(|s| {
                assert_eq!(s.response_bytes, s.response.len());
                s.response.len()
            })
            .sum()
    }
    pub fn tool_calls(&self) -> usize {
        self.steps.iter().filter(|s| !s.command.is_empty()).count()
    }
    fn unavailable(reason: &str) -> Self {
        Self {
            unavailable: Some(reason.to_string()),
            ..Self::default()
        }
    }
    fn record(&mut self, args: &[&str], response: Vec<u8>, prog: bool) -> Value {
        let value: Value = serde_json::from_slice(&response).unwrap_or(Value::Null);
        let mut command: Vec<String> = args
            .iter()
            .map(|arg| {
                if prog && arg.starts_with("pc1_") {
                    "$CURSOR".to_string()
                } else {
                    (*arg).to_string()
                }
            })
            .collect();
        if prog {
            command.insert(0, "prog".to_string());
        }
        let finding_paths = if prog {
            value["findings"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|finding| finding["path"].as_str().map(str::to_string))
                .collect()
        } else {
            Vec::new()
        };
        self.steps.push(Step {
            command,
            finding_paths,
            response_bytes: response.len(),
            expansion: prog && args.first() == Some(&"expand"),
            cache_hit: prog && value["cache"]["status"] == "hit",
            response,
        });
        value
    }
    pub fn prog(&mut self, root: &Path, args: &[&str], stdin: Option<&[u8]>) -> Value {
        let output = timed_prog(root, args, stdin);
        assert_success(&output.output);
        self.elapsed_ms += output.elapsed_ms;
        self.record(args, output.output.stdout, true)
    }
    fn python(
        &mut self,
        root: &Path,
        script: &str,
        args: &[&str],
        input: Option<&[u8]>,
    ) -> Vec<u8> {
        let started = Instant::now();
        let mut command = Command::new("python3");
        command.current_dir(root).args(["-c", script]).args(args);
        let output = execute(command, input);
        assert_success(&output);
        self.elapsed_ms += started.elapsed().as_millis();
        let mut argv = vec!["python3", "-c", script];
        argv.extend_from_slice(args);
        self.record(&argv, output.stdout.clone(), false);
        output.stdout
    }
}

pub fn run(root: &Path, input: &StrategyInput, strategy: &str) -> Execution {
    match strategy {
        "raw_context" | "caveman_terse_output" | "head_tail_truncation" => {
            let bytes = if strategy == "head_tail_truncation" {
                head_tail(&input.raw_bytes, 4096)
            } else {
                input.raw_bytes.clone()
            };
            let mut result = Execution {
                evidence: bytes.clone(),
                ..Execution::default()
            };
            result.record(&[], bytes, false);
            result
                .notes
                .push("raw artifact availability; no agent answer is generated".to_string());
            result
        }
        "native_field_selection" => {
            let Some(selector) = input.selector() else {
                return Execution::unavailable("no public selector for an unknown target");
            };
            if serde_json::from_slice::<Value>(&input.raw_bytes).is_err() {
                return Execution::unavailable(
                    "native JSON selection does not support this artifact format",
                );
            }
            let mut result = Execution::default();
            result.evidence = result.python(root, SELECT_JSON, &[selector], Some(&input.raw_bytes));
            result
        }
        "rtk_grep_filter" | "broad_log_search" => {
            let pattern = if strategy == "broad_log_search" {
                SEVERITY_PATTERN.to_string()
            } else {
                let Some(term) = input.grep_term() else {
                    return Execution::unavailable("no public grep term for this task");
                };
                // Python's literal mode avoids treating prompt text as a regex.
                term.to_string()
            };
            let mut result = Execution::default();
            let mode = if strategy == "broad_log_search" {
                "regex"
            } else {
                "literal"
            };
            result.evidence = result.python(
                root,
                SEARCH_LINES,
                &[mode, &pattern],
                Some(&input.raw_bytes),
            );
            result
                .notes
                .push("line search can return an entire minified JSON artifact".to_string());
            result
        }
        "file_capture_search" => {
            // One real capture writes the shared raw artifact to a file and
            // emits a bounded receipt. Search reads that file without putting
            // its full contents in context. Count both actual stdout responses.
            let artifact_dir = tempfile::tempdir().unwrap();
            let mut result = Execution::default();
            result.python(
                artifact_dir.path(),
                CAPTURE_FILE,
                &[],
                Some(&input.raw_bytes),
            );
            result.evidence = match &input.task {
                Task::KnownPath {
                    grep_term: Some(term),
                    ..
                } => result.python(artifact_dir.path(), SEARCH_FILE, &["literal", term], None),
                _ => result.python(
                    artifact_dir.path(),
                    SEARCH_FILE,
                    &["regex", SEVERITY_PATTERN],
                    None,
                ),
            };
            assert_eq!(
                fs::read(artifact_dir.path().join("capture.txt")).unwrap(),
                input.raw_bytes
            );
            result.notes.push("capture once to a local file, then search it; receipt and search stdout are both counted".to_string());
            result
        }
        "prog_envelope_only" | "prog_retrieve" | "prog_repeated_cache" => {
            run_prog(root, input, strategy)
        }
        _ => panic!("unknown strategy {strategy}"),
    }
}

fn run_prog(root: &Path, input: &StrategyInput, strategy: &str) -> Execution {
    let mut result = Execution::default();
    let initial = match &input.source {
        Source::Call {
            source_id,
            operation,
        } => result.prog(root, &["call", source_id, operation, "--args", "{}"], None),
        Source::Observe { name, mime, bytes } => result.prog(
            root,
            &["observe", "--stdin", "--mime", mime, "--name", name],
            Some(bytes),
        ),
    };
    result.evidence = serde_json::to_vec(&initial["data_preview"]).unwrap();
    if strategy == "prog_envelope_only" {
        return result;
    }
    let cursor = initial["cursor"]
        .as_str()
        .expect("capture must provide a cursor");
    let selected = if let Some(selector) = input.selector() {
        result
            .notes
            .push("exact selector is supplied publicly; this measures recoverability".to_string());
        Some(selector.to_string())
    } else {
        // The first returned finding is already ranked. Inspect only if the
        // initial response supplies none; no oracle-controlled retry is possible.
        let mut selected = first_finding_path(&initial);
        if selected.is_none() {
            let inspected = result.prog(root, &["inspect", cursor, "--goal", "root_cause"], None);
            selected = first_finding_path(&inspected);
        }
        result.notes.push(
            "expand the first returned finding; inspect only if the first view has no findings"
                .to_string(),
        );
        selected
    };
    if let Some(path) = selected {
        let expanded = result.prog(root, &["expand", cursor, "--path", &path], None);
        result.evidence = serde_json::to_vec(&expanded["data_preview"]).unwrap();
        if strategy == "prog_repeated_cache" {
            let repeated = result.prog(root, &["expand", cursor, "--path", &path], None);
            result.evidence = serde_json::to_vec(&repeated["data_preview"]).unwrap();
        }
        result.selected_path = Some(path);
    } else {
        result.notes.push(
            "no candidate was returned; insufficient evidence, with no exact-path fallback"
                .to_string(),
        );
    }
    result
}

pub fn first_finding_path(value: &Value) -> Option<String> {
    value["findings"].as_array()?.first()?["path"]
        .as_str()
        .map(str::to_string)
}

pub struct TimedOutput {
    pub output: Output,
    pub elapsed_ms: u128,
}
pub fn timed_prog(root: &Path, args: &[&str], stdin: Option<&[u8]>) -> TimedOutput {
    let started = Instant::now();
    let mut command = Command::new(env!("CARGO_BIN_EXE_prog"));
    command.current_dir(root).arg("--dir").arg(root).args(args);
    TimedOutput {
        output: execute(command, stdin),
        elapsed_ms: started.elapsed().as_millis(),
    }
}
fn execute(mut command: Command, stdin: Option<&[u8]>) -> Output {
    command.stdin(Stdio::null());
    if let Some(input) = stdin {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        child.wait_with_output().unwrap()
    } else {
        command.output().unwrap()
    }
}
pub fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
}
fn head_tail(bytes: &[u8], cap: usize) -> Vec<u8> {
    if bytes.len() <= cap {
        return bytes.to_vec();
    }
    [&bytes[..cap / 2], &bytes[bytes.len() - cap / 2..]].concat()
}
const SELECT_JSON: &str = "import json,sys\nv=json.load(sys.stdin)\ntry:\n for p in sys.argv[1].split('/')[1:]:\n  p=p.replace('~1','/').replace('~0','~'); v=v[int(p)] if isinstance(v,list) else v[p]\nexcept (KeyError,IndexError,ValueError,TypeError):\n v=None\nprint(json.dumps(v,separators=(',',':')))";
const SEARCH_LINES: &str = "import re,sys\ntext=sys.stdin.read()\npattern=sys.argv[2]\nfor n,line in enumerate(text.splitlines(),1):\n if (re.search(pattern,line) if sys.argv[1]=='regex' else pattern in line):\n  print(str(n)+':'+line)";
const CAPTURE_FILE: &str = "import json,pathlib,sys\nb=sys.stdin.buffer.read()\npathlib.Path('capture.txt').write_bytes(b)\nprint(json.dumps({'path':'capture.txt','bytes':len(b)},separators=(',',':')))";
const SEARCH_FILE: &str = "import pathlib,re,sys\ntext=pathlib.Path('capture.txt').read_text()\npattern=sys.argv[2]\nfor n,line in enumerate(text.splitlines(),1):\n if (re.search(pattern,line) if sys.argv[1]=='regex' else pattern in line):\n  print(str(n)+':'+line)";

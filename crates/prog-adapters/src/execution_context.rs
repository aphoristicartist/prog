//! Transient process inputs shared by cache identity and execution.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use tokio::process::Command;

/// A resolved working directory and an exact snapshot of the inherited
/// environment. Values may be secrets: this type deliberately has no Debug or
/// serialization implementation and must never be included in provenance.
pub struct ExecutionContext {
    working_dir: PathBuf,
    environment: BTreeMap<OsString, OsString>,
}

impl ExecutionContext {
    /// Resolve relative directories against the caller and capture all ambient
    /// environment dependencies, including non-UTF-8 values and PATH.
    pub fn inherit(working_dir: Option<&Path>) -> io::Result<Self> {
        let caller_dir = std::env::current_dir()?;
        let working_dir = match working_dir {
            Some(directory) => caller_dir.join(directory),
            None => caller_dir,
        }
        .canonicalize()?;
        if !working_dir.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "process working directory must be a directory",
            ));
        }
        Ok(Self {
            working_dir,
            environment: std::env::vars_os().collect(),
        })
    }

    /// A transient component of the enclosing call fingerprint. The caller
    /// hashes this together with the source, arguments, and policy, and persists
    /// only that final key, never this component or its underlying inputs.
    pub fn cache_scope(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(b"prog.execution_context.v1");
        hash_part(&mut hash, self.working_dir.as_os_str().as_encoded_bytes());
        hash.update((self.environment.len() as u64).to_be_bytes());
        for (key, value) in &self.environment {
            hash_part(&mut hash, key.as_encoded_bytes());
            hash_part(&mut hash, value.as_encoded_bytes());
        }
        format!("{:x}", hash.finalize())
    }

    /// Apply before configured environment overrides. Clearing ambient values
    /// ensures execution cannot silently inherit a newer environment snapshot.
    pub(crate) fn configure(&self, command: &mut Command) {
        command
            .current_dir(&self.working_dir)
            .env_clear()
            .envs(&self.environment);
    }
}

fn hash_part(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn scope_distinguishes_missing_empty_non_utf8_and_ambiguous_boundaries() {
        fn scope(entries: &[(&[u8], &[u8])]) -> String {
            ExecutionContext {
                working_dir: PathBuf::from("/fixture"),
                environment: entries
                    .iter()
                    .map(|(key, value)| {
                        (
                            OsString::from_vec(key.to_vec()),
                            OsString::from_vec(value.to_vec()),
                        )
                    })
                    .collect(),
            }
            .cache_scope()
        }
        assert_ne!(scope(&[]), scope(&[(b"CONTEXT", b"")]));
        assert_ne!(
            scope(&[(b"CONTEXT", b"\xff")]),
            scope(&[(b"CONTEXT", b"\xfe")])
        );
        assert_ne!(scope(&[(b"A", b"BC")]), scope(&[(b"AB", b"C")]));
        assert_eq!(
            scope(&[(b"A", b"1"), (b"B", b"2")]),
            scope(&[(b"B", b"2"), (b"A", b"1")])
        );
    }

    #[tokio::test]
    async fn execution_uses_only_the_snapshot_with_explicit_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let context = ExecutionContext {
            working_dir: dir.path().canonicalize().unwrap(),
            environment: BTreeMap::from([(
                OsString::from("FROZEN_VALUE"),
                OsString::from("before"),
            )]),
        };
        let mut command = Command::new("/bin/sh");
        // These pending command inputs must be replaced by the frozen snapshot.
        command
            .env("AMBIENT_VALUE", "must not survive")
            .env("FROZEN_VALUE", "after")
            .current_dir("/");
        context.configure(&mut command);
        command.env("EXPLICIT_VALUE", "configured").args([
            "-c",
            "printf '%s\\n' \"$FROZEN_VALUE\" \"${AMBIENT_VALUE-unset}\" \"$EXPLICIT_VALUE\"; pwd",
        ]);
        let output = command.output().await.unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!(
                "before\nunset\nconfigured\n{}\n",
                dir.path().canonicalize().unwrap().display()
            )
        );
    }
}

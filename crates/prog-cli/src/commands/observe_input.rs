//! Bounded whole-artifact acquisition. Rejected bytes never reach a parser or store.

use std::{
    fs::{File, OpenOptions},
    io::{self, Read},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::OpenOptionsExt,
    },
    time::Duration,
};

use prog_core::{
    BudgetSource, CaptureBudget, CaptureCompleteness, CaptureLimit, CaptureScope,
    CaptureStopReason, CoreError, Extra, Result,
};
use tokio::signal::unix::{SignalKind, signal};

use crate::ObserveArgs;

pub(crate) const DEFAULT_MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) const DEFAULT_INPUT_TIMEOUT_MS: u64 = 30_000;

pub(crate) fn budget(args: &ObserveArgs) -> CaptureBudget {
    CaptureBudget {
        source: if args.max_input_bytes.is_some() || args.timeout_ms.is_some() {
            BudgetSource::Invocation
        } else {
            BudgetSource::Default
        },
        limits: vec![CaptureLimit {
            scope: "artifact".to_string(),
            max_bytes: Some(args.max_input_bytes.unwrap_or(DEFAULT_MAX_INPUT_BYTES)),
            max_duration_ms: Some(args.timeout_ms.unwrap_or(DEFAULT_INPUT_TIMEOUT_MS)),
            max_work_units: None,
            extra: Extra::new(),
        }],
        extra: Extra::new(),
    }
}

struct InputReader {
    file: File,
    // dup shares the inherited open-file description, including status flags.
    // Restore those flags on success, rejection, cancellation, and I/O failure.
    restore_flags: Option<libc::c_int>,
}

impl InputReader {
    fn open(args: &ObserveArgs) -> Result<Self> {
        if let Some(path) = &args.file {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(path)
                .map_err(|error| CoreError::BadArgs {
                    operation: "observe".to_string(),
                    reason: format!(
                        "file '{}' could not be read: {error}",
                        path.to_string_lossy()
                    ),
                })?;
            if !file.metadata()?.is_file() {
                return Err(CoreError::BadArgs {
                    operation: "observe".to_string(),
                    reason: "--file requires a regular file; use --stdin for pipes or devices"
                        .to_string(),
                });
            }
            return Ok(Self {
                file,
                restore_flags: None,
            });
        }
        if !args.stdin {
            return Err(CoreError::BadArgs {
                operation: "observe".to_string(),
                reason: "pass --file <path> or --stdin".to_string(),
            });
        }
        // SAFETY: fcntl duplicates an existing descriptor. Ownership transfers
        // to File only after checking for the negative error return.
        let fd = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_DUPFD_CLOEXEC, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error().into());
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(Self {
            file,
            restore_flags: Some(flags),
        })
    }
}

impl Drop for InputReader {
    fn drop(&mut self) {
        if let Some(flags) = self.restore_flags {
            // SAFETY: self still owns this descriptor throughout Drop.
            loop {
                let restored = unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_SETFL, flags) };
                if restored >= 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted
                {
                    break;
                }
            }
        }
    }
}

struct Stopped {
    reason: CaptureStopReason,
    signal: Option<i32>,
}

impl From<CaptureStopReason> for Stopped {
    fn from(reason: CaptureStopReason) -> Self {
        Self {
            reason,
            signal: None,
        }
    }
}

async fn read_to_cap(
    reader: &mut InputReader,
    cap: u64,
    captured: &mut u64,
) -> std::result::Result<Vec<u8>, Stopped> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        // Even an always-readable file/device must yield to deadline/signal
        // handling. No blocking stdin worker survives cancellation.
        tokio::task::yield_now().await;
        let remaining = cap.saturating_sub(*captured);
        let probe_len = remaining.min((chunk.len() - 1) as u64) as usize + 1;
        match reader.file.read(&mut chunk[..probe_len]) {
            Ok(0) => return Ok(bytes),
            Ok(count) => {
                *captured += count as u64;
                if *captured > cap {
                    return Err(CaptureStopReason::ByteLimit.into());
                }
                bytes.extend_from_slice(&chunk[..count]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(CaptureStopReason::Unavailable.into()),
        }
    }
}

pub(crate) async fn read(args: &ObserveArgs) -> Result<Vec<u8>> {
    // Install both handlers before opening/reading the input. The descriptor
    // becomes nonblocking only after cancellation handling is available.
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let deadline = tokio::time::sleep(Duration::from_millis(
        args.timeout_ms.unwrap_or(DEFAULT_INPUT_TIMEOUT_MS),
    ));
    tokio::pin!(deadline);
    let mut reader = InputReader::open(args)?;
    let mut captured = 0;
    let outcome = tokio::select! {
        biased;
        _ = interrupt.recv() => Err(Stopped { reason: CaptureStopReason::Cancelled, signal: Some(libc::SIGINT) }),
        _ = terminate.recv() => Err(Stopped { reason: CaptureStopReason::Cancelled, signal: Some(libc::SIGTERM) }),
        _ = &mut deadline => Err(CaptureStopReason::Timeout.into()),
        result = read_to_cap(&mut reader, args.max_input_bytes.unwrap_or(DEFAULT_MAX_INPUT_BYTES), &mut captured) => result,
    };
    outcome.map_err(|stopped| {
        let mut extra = Extra::new();
        if let Some(signal) = stopped.signal {
            extra.insert("signal".to_string(), serde_json::json!(signal));
        }
        CoreError::CaptureStopped {
            operation: "observe".to_string(),
            capture: Box::new(CaptureCompleteness {
                total_bytes: None,
                captured_bytes: captured,
                stored_bytes: 0,
                stop_reason: stopped.reason,
                budget: budget(args),
                affected: vec![CaptureScope {
                    scope: "artifact".to_string(),
                    total_bytes: None,
                    captured_bytes: captured,
                    stop_reason: stopped.reason,
                    extra: Extra::new(),
                }],
                can_prove_absence: false,
                extra,
            }),
        }
    })
}

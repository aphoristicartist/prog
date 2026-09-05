//! Bounded lifecycle for a child process and its stdout/stderr readers.

use std::{future::Future, io, process::ExitStatus, time::Duration};

use tokio::{
    process::{Child, Command},
    task::JoinHandle,
    time::Instant,
};

/// Prepare a command for capture. Its process group belongs to this capture,
/// and dropping an unfinished child must terminate the immediate process.
pub fn configure_capture_process(command: &mut Command) {
    command.kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureInterruption {
    Timeout,
    Signal(i32),
}

#[derive(Debug)]
pub enum ProcessCapture<T> {
    Complete {
        status: ExitStatus,
        stdout: T,
        stderr: T,
    },
    Interrupted {
        reason: CaptureInterruption,
        stdout: Option<T>,
        stderr: Option<T>,
    },
}

/// Resolve child exit and both readers under one absolute deadline. The child
/// must have been spawned with [`configure_capture_process`]. The caller owns
/// signal policy; pass a pending future when the adapter has no signal handler.
/// Reader tasks must use asynchronous, cancellation-safe pipe reads.
pub async fn capture_process<T>(
    child: Child,
    stdout: JoinHandle<io::Result<T>>,
    stderr: JoinHandle<io::Result<T>>,
    deadline: Instant,
    cancellation: impl Future<Output = i32>,
) -> io::Result<ProcessCapture<T>> {
    let mut process = CaptureChild::new(child);
    let mut stdout = Reader::new(stdout);
    let mut stderr = Reader::new(stderr);
    let mut status = None;
    tokio::pin!(cancellation);
    let timeout = tokio::time::sleep_until(deadline);
    tokio::pin!(timeout);

    let interrupted = loop {
        tokio::select! {
            biased;
            signal = &mut cancellation => break Ok(CaptureInterruption::Signal(signal)),
            _ = &mut timeout => break Ok(CaptureInterruption::Timeout),
            result = process.child.wait(), if status.is_none() => {
                match result {
                    Ok(value) => status = Some(value),
                    Err(error) => break Err(error),
                }
            }
            result = &mut stdout.task, if !stdout.done => {
                if let Err(error) = stdout.record(result) { break Err(error); }
            }
            result = &mut stderr.task, if !stderr.done => {
                if let Err(error) = stderr.record(result) { break Err(error); }
            }
        }
        if stdout.done
            && stderr.done
            && let Some(status) = status
        {
            process.group.disarm();
            return Ok(ProcessCapture::Complete {
                status,
                stdout: stdout.value.take().expect("stdout completed successfully"),
                stderr: stderr.value.take().expect("stderr completed successfully"),
            });
        }
    };

    process.group.terminate();
    let _ = process.child.start_kill();
    let _ = tokio::time::timeout(Duration::from_millis(100), process.child.wait()).await;
    let (stdout, stderr) = tokio::join!(stdout.finish_or_abort(), stderr.finish_or_abort());
    Ok(ProcessCapture::Interrupted {
        reason: interrupted?,
        stdout,
        stderr,
    })
}

struct CaptureChild {
    child: Child,
    group: OwnedProcessGroup,
}

impl CaptureChild {
    fn new(child: Child) -> Self {
        Self {
            group: OwnedProcessGroup::new(child.id()),
            child,
        }
    }
}

/// Own the group created by `configure_capture_process`, even after the
/// immediate child is reaped. Disarm only after successful transport cleanup.
pub(crate) struct OwnedProcessGroup {
    // Child::id() becomes None after wait() reaps the immediate process.
    // Its descendants may still belong to this group and hold the pipes open.
    group_id: Option<u32>,
}

impl OwnedProcessGroup {
    pub(crate) fn new(group_id: Option<u32>) -> Self {
        Self { group_id }
    }

    pub(crate) fn disarm(&mut self) {
        self.group_id = None;
    }

    pub(crate) fn terminate(&mut self) {
        let group_id = self.group_id.take();
        #[cfg(unix)]
        if let Some(pid) = group_id.and_then(|pid| i32::try_from(pid).ok()) {
            // configure_capture_process sets PGID to the child's original PID.
            let _ = unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
    }
}

impl Drop for OwnedProcessGroup {
    fn drop(&mut self) {
        self.terminate();
    }
}

struct Reader<T> {
    task: JoinHandle<io::Result<T>>,
    done: bool,
    value: Option<T>,
}

impl<T> Reader<T> {
    fn new(task: JoinHandle<io::Result<T>>) -> Self {
        Self {
            task,
            done: false,
            value: None,
        }
    }

    fn record(&mut self, result: Result<io::Result<T>, tokio::task::JoinError>) -> io::Result<()> {
        self.done = true;
        self.value = Some(result.map_err(io::Error::other)??);
        Ok(())
    }

    async fn finish_or_abort(&mut self) -> Option<T> {
        if !self.done {
            match tokio::time::timeout(Duration::from_millis(25), &mut self.task).await {
                Ok(result) => {
                    let _ = self.record(result);
                }
                Err(_) => {
                    self.task.abort();
                    let _ = (&mut self.task).await;
                    self.done = true;
                }
            }
        }
        self.value.take()
    }
}

impl<T> Drop for Reader<T> {
    fn drop(&mut self) {
        if !self.done {
            self.task.abort();
        }
    }
}

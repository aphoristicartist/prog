//! Bounded diagnostic capture whose task and partial progress remain owned.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use prog_core::redact_sensitive_text;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    task::JoinHandle,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum StderrStopReason {
    Eof,
    Timeout,
    ShutdownTimeout,
    ShutdownFailed,
    ReadError,
    ReaderFailed,
    #[default]
    Unavailable,
}

impl StderrStopReason {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Eof => "eof",
            Self::Timeout => "timeout",
            Self::ShutdownTimeout => "shutdown_timeout",
            Self::ShutdownFailed => "shutdown_failed",
            Self::ReadError => "read_error",
            Self::ReaderFailed => "reader_failed",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct Capture {
    bytes: Vec<u8>,
    observed_bytes: usize,
    pub(super) stop_reason: StderrStopReason,
}

impl Capture {
    pub(super) fn complete(&self) -> bool {
        self.stop_reason == StderrStopReason::Eof
    }
}

pub(super) struct StderrDrain {
    task: Option<JoinHandle<()>>,
    capture: Arc<Mutex<Capture>>,
}

impl StderrDrain {
    pub(super) fn spawn<R: AsyncRead + Unpin + Send + 'static>(mut reader: R, cap: usize) -> Self {
        let capture = Arc::new(Mutex::new(Capture::default()));
        let progress = Arc::clone(&capture);
        let task = tokio::spawn(async move {
            let mut buffer = [0u8; 8192];
            loop {
                let read = reader.read(&mut buffer).await;
                {
                    let mut capture = progress.lock().unwrap_or_else(|error| error.into_inner());
                    match read {
                        Ok(0) => {
                            capture.stop_reason = StderrStopReason::Eof;
                            break;
                        }
                        Ok(read) => {
                            capture.observed_bytes = capture.observed_bytes.saturating_add(read);
                            let retained = read.min(cap.saturating_sub(capture.bytes.len()));
                            capture.bytes.extend_from_slice(&buffer[..retained]);
                        }
                        Err(_) => {
                            capture.stop_reason = StderrStopReason::ReadError;
                            break;
                        }
                    }
                }
                // Also bound cancellation latency for an always-ready reader.
                tokio::task::yield_now().await;
            }
        });
        Self {
            task: Some(task),
            capture,
        }
    }

    pub(super) async fn finish(
        &mut self,
        wait: Duration,
        timeout_reason: StderrStopReason,
    ) -> Capture {
        let mut interrupted = None;
        if let Some(task) = self.task.as_mut() {
            // Borrow the handle: dropping this future must leave an abortable
            // task in the owner, including while waiting for cancellation.
            match tokio::time::timeout(wait, &mut *task).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => interrupted = Some(StderrStopReason::ReaderFailed),
                Err(_) => {
                    interrupted = Some(timeout_reason);
                    task.abort();
                    let _ = task.await;
                }
            }
        }
        self.task = None;
        let mut capture = std::mem::take(
            &mut *self
                .capture
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        );
        if let Some(reason) = interrupted {
            capture.stop_reason = reason;
        }
        capture
    }
}

impl Drop for StderrDrain {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

pub(super) fn normalize_text_capture(capture: &Capture) -> Value {
    let text = String::from_utf8_lossy(&capture.bytes);
    let lines: Vec<String> = text
        .lines()
        .map(|line| redact_sensitive_text(line).0)
        .collect();
    let head: Vec<Value> = lines.iter().take(10).map(|line| json!(line)).collect();
    let tail_start = lines.len().saturating_sub(10).max(head.len());
    let tail: Vec<Value> = lines
        .iter()
        .skip(tail_start)
        .map(|line| json!(line))
        .collect();
    let all_bytes_retained = capture.bytes.len() == capture.observed_bytes;
    json!({
        "format": "text",
        "head": head,
        "tail": tail,
        "line_count": (capture.complete() && all_bytes_retained).then_some(lines.len()),
        "captured_line_count": lines.len(),
        "byte_count": capture.complete().then_some(capture.observed_bytes),
        "observed_byte_count": capture.observed_bytes,
        "captured_byte_count": capture.bytes.len(),
        "complete": capture.complete(),
        "stop_reason": capture.stop_reason.as_str(),
        "truncated": !capture.complete() || !all_bytes_retained || lines.len() > head.len() + tail.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io,
        pin::Pin,
        task::{Context, Poll},
    };
    use tokio::{
        io::{AsyncWriteExt, ReadBuf},
        sync::oneshot,
    };

    struct TrackedReader<R> {
        reader: R,
        dropped: Option<oneshot::Sender<()>>,
    }

    impl<R: AsyncRead + Unpin> AsyncRead for TrackedReader<R> {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.reader).poll_read(cx, buf)
        }
    }

    impl<R> Drop for TrackedReader<R> {
        fn drop(&mut self) {
            if let Some(dropped) = self.dropped.take() {
                let _ = dropped.send(());
            }
        }
    }

    fn tracked<R>(reader: R) -> (TrackedReader<R>, oneshot::Receiver<()>) {
        let (dropped, receiver) = oneshot::channel();
        (
            TrackedReader {
                reader,
                dropped: Some(dropped),
            },
            receiver,
        )
    }

    async fn guarded<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(2), future)
            .await
            .expect("independent test deadline")
    }

    #[tokio::test]
    async fn eof_is_distinct_from_unavailable_and_prefix_limits() {
        for (input, cap) in [
            (b"".as_slice(), 20),
            (b"one\ntwo\n", 20),
            (b"one\ntwo\n", 4),
            (b"one\n", 0),
        ] {
            let mut drain = StderrDrain::spawn(input, cap);
            let capture =
                guarded(drain.finish(Duration::from_secs(1), StderrStopReason::Timeout)).await;
            let value = normalize_text_capture(&capture);
            assert_eq!(value["complete"], true);
            assert_eq!(value["stop_reason"], "eof");
            assert_eq!(value["byte_count"], input.len());
            assert_eq!(value["captured_byte_count"], input.len().min(cap));
            assert_eq!(value["truncated"], input.len() > cap);
            if input.len() > cap {
                assert!(value["line_count"].is_null());
            }
        }
        let unknown = normalize_text_capture(&Capture::default());
        assert_eq!(unknown["complete"], false);
        assert!(unknown["byte_count"].is_null());
        assert!(unknown["line_count"].is_null());
    }

    #[tokio::test]
    async fn timeout_preserves_redacted_prefix_and_releases_reader_in_live_runtime() {
        let (reader, mut writer) = tokio::io::duplex(1024);
        let (reader, dropped) = tracked(reader);
        let mut drain = StderrDrain::spawn(reader, 64);
        let prefix = b"diagnostic marker\nAuthorization: Bearer secret-mcp-token\n";
        writer.write_all(prefix).await.unwrap();
        let capture =
            guarded(drain.finish(Duration::from_millis(50), StderrStopReason::Timeout)).await;
        guarded(dropped).await.unwrap();
        assert!(writer.write_all(b"unread").await.is_err());
        let value = normalize_text_capture(&capture);
        assert_eq!(value["complete"], false);
        assert_eq!(value["stop_reason"], "timeout");
        assert_eq!(value["observed_byte_count"], prefix.len());
        assert!(value["byte_count"].is_null());
        assert!(value["line_count"].is_null());
        assert_eq!(value["head"][0], "diagnostic marker");
        assert!(!value.to_string().contains("secret-mcp-token"));
    }

    #[tokio::test]
    async fn dropping_owner_before_poll_or_during_finish_aborts_reader() {
        for started in [false, true] {
            let (reader, _writer) = tokio::io::duplex(16);
            let (reader, dropped) = tracked(reader);
            let mut drain = StderrDrain::spawn(reader, 16);
            if started {
                assert!(
                    tokio::time::timeout(
                        Duration::from_millis(10),
                        drain.finish(Duration::from_secs(60), StderrStopReason::Timeout)
                    )
                    .await
                    .is_err()
                );
            }
            drop(drain);
            guarded(dropped).await.unwrap();
        }
    }

    struct FaultyReader {
        sent_prefix: bool,
        panic: bool,
    }

    impl AsyncRead for FaultyReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            if !self.sent_prefix {
                self.sent_prefix = true;
                buf.put_slice(b"before error\n");
                return Poll::Ready(Ok(()));
            }
            assert!(!self.panic, "injected reader failure");
            Poll::Ready(Err(io::Error::other("injected read error")))
        }
    }

    #[tokio::test]
    async fn read_and_join_errors_preserve_prefix_without_claiming_eof() {
        for panic in [false, true] {
            let mut drain = StderrDrain::spawn(
                FaultyReader {
                    sent_prefix: false,
                    panic,
                },
                64,
            );
            let capture =
                guarded(drain.finish(Duration::from_secs(1), StderrStopReason::Timeout)).await;
            let value = normalize_text_capture(&capture);
            assert_eq!(value["head"][0], "before error");
            assert_eq!(value["observed_byte_count"], 13);
            assert!(value["byte_count"].is_null());
            assert_eq!(
                value["stop_reason"],
                if panic { "reader_failed" } else { "read_error" }
            );
        }
    }

    struct AlwaysReady;

    impl AsyncRead for AlwaysReady {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            buf.put_slice(b"continuing output\n");
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn continuously_ready_reader_remains_bounded_and_cancellable() {
        let (reader, dropped) = tracked(AlwaysReady);
        let mut drain = StderrDrain::spawn(reader, 3);
        let capture =
            guarded(drain.finish(Duration::from_millis(20), StderrStopReason::Timeout)).await;
        guarded(dropped).await.unwrap();
        assert_eq!(capture.bytes, b"con");
        assert!(capture.observed_bytes > 3);
        assert!(!capture.complete());
    }
}

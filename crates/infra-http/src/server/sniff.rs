//! The protocol-sniff read deadline in front of hyper-util's `auto` builder.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::{Instant, Sleep};

/// The HTTP/2 connection preface, which hyper-util's protocol sniff compares
/// the first bytes of a connection against.
pub(super) const HTTP2_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// A socket whose reads fail once `timeout` passes with the protocol still
/// undecided.
///
/// hyper-util's sniff reads until the bytes stop matching the HTTP/2 preface
/// and has no timer (hyper #3756), so a client that connects and sends
/// nothing, or only the start of the preface, would never be timed out.
/// hyper's HTTP/1 header timer and HTTP/2 keep-alive start once the protocol
/// is decided; from then on this is a pass-through. The failed read ends the
/// connection without a response.
pub(super) struct SniffDeadline<IO> {
    io: IO,
    /// The preface bytes matched so far and the deadline, until decided.
    undecided: Option<(usize, Pin<Box<Sleep>>)>,
}

impl<IO> SniffDeadline<IO> {
    pub(super) fn new(io: IO, timeout: Duration) -> Self {
        Self {
            io,
            undecided: Some((0, Box::pin(tokio::time::sleep(timeout)))),
        }
    }
}

impl<IO: AsyncRead + Unpin> AsyncRead for SniffDeadline<IO> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let Some((matched, deadline)) = this.undecided.as_mut() else {
            return Pin::new(&mut this.io).poll_read(cx, buf);
        };
        if Instant::now() >= deadline.deadline() {
            return Poll::Ready(Err(io::ErrorKind::TimedOut.into()));
        }
        let filled = buf.filled().len();
        match Pin::new(&mut this.io).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                let read = &buf.filled()[filled..];
                let rest = &HTTP2_PREFACE[*matched..];
                // Undecided only while everything read is a strict prefix of
                // the preface; the end of the stream decides as well.
                if !read.is_empty() && read.len() < rest.len() && rest.starts_with(read) {
                    *matched += read.len();
                } else {
                    this.undecided = None;
                }
                Poll::Ready(Ok(()))
            }
            Poll::Pending => deadline
                .as_mut()
                .poll(cx)
                .map(|()| Err(io::ErrorKind::TimedOut.into())),
            failed @ Poll::Ready(Err(_)) => failed,
        }
    }
}

impl<IO: AsyncWrite + Unpin> AsyncWrite for SniffDeadline<IO> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().io).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_shutdown(cx)
    }

    // hyper writes a response head and body as one vectored write; the
    // default implementation would send them as separate segments.
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().io).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }
}

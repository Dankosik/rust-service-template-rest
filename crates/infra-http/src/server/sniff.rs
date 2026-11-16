use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::{Instant, Sleep};

pub(super) const HTTP2_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

pub(super) struct SniffDeadline<IO> {
    io: IO,
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

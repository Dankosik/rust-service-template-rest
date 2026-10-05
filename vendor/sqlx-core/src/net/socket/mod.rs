use std::future::Future;
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

pub use buffered::{BufferedSocket, WriteBuffer};
use bytes::BufMut;
use cfg_if::cfg_if;

use crate::io::ReadBuf;

mod buffered;

pub trait Socket: Send + Sync + Unpin + 'static {
    fn try_read(&mut self, buf: &mut dyn ReadBuf) -> io::Result<usize>;

    fn try_write(&mut self, buf: &[u8]) -> io::Result<usize>;

    fn poll_read_ready(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>>;

    fn poll_write_ready(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>>;

    fn poll_flush(&mut self, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // `flush()` is a no-op for TCP/UDS
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>>;

    fn read<'a, B: ReadBuf>(&'a mut self, buf: &'a mut B) -> Read<'a, Self, B>
    where
        Self: Sized,
    {
        Read { socket: self, buf }
    }

    fn write<'a>(&'a mut self, buf: &'a [u8]) -> Write<'a, Self>
    where
        Self: Sized,
    {
        Write { socket: self, buf }
    }

    fn flush(&mut self) -> Flush<'_, Self>
    where
        Self: Sized,
    {
        Flush { socket: self }
    }

    fn shutdown(&mut self) -> Shutdown<'_, Self>
    where
        Self: Sized,
    {
        Shutdown { socket: self }
    }
}

pub struct Read<'a, S: ?Sized, B> {
    socket: &'a mut S,
    buf: &'a mut B,
}

impl<S: ?Sized, B> Future for Read<'_, S, B>
where
    S: Socket,
    B: ReadBuf,
{
    type Output = io::Result<usize>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;

        while this.buf.has_remaining_mut() {
            match this.socket.try_read(&mut *this.buf) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    ready!(this.socket.poll_read_ready(cx))?;
                }
                ready => return Poll::Ready(ready),
            }
        }

        Poll::Ready(Ok(0))
    }
}

pub struct Write<'a, S: ?Sized> {
    socket: &'a mut S,
    buf: &'a [u8],
}

impl<S: ?Sized> Future for Write<'_, S>
where
    S: Socket,
{
    type Output = io::Result<usize>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;

        while !this.buf.is_empty() {
            match this.socket.try_write(this.buf) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    ready!(this.socket.poll_write_ready(cx))?;
                }
                ready => return Poll::Ready(ready),
            }
        }

        Poll::Ready(Ok(0))
    }
}

pub struct Flush<'a, S: ?Sized> {
    socket: &'a mut S,
}

impl<S: Socket + ?Sized> Future for Flush<'_, S> {
    type Output = io::Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.socket.poll_flush(cx)
    }
}

pub struct Shutdown<'a, S: ?Sized> {
    socket: &'a mut S,
}

impl<S: ?Sized> Future for Shutdown<'_, S>
where
    S: Socket,
{
    type Output = io::Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.socket.poll_shutdown(cx)
    }
}

pub trait WithSocket {
    type Output;

    fn with_socket<S: Socket>(self, socket: S) -> impl Future<Output = Self::Output> + Send;
}

pub struct SocketIntoBox;

impl WithSocket for SocketIntoBox {
    type Output = Box<dyn Socket>;

    async fn with_socket<S: Socket>(self, socket: S) -> Self::Output {
        Box::new(socket)
    }
}

impl<S: Socket + ?Sized> Socket for Box<S> {
    fn try_read(&mut self, buf: &mut dyn ReadBuf) -> io::Result<usize> {
        (**self).try_read(buf)
    }

    fn try_write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (**self).try_write(buf)
    }

    fn poll_read_ready(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        (**self).poll_read_ready(cx)
    }

    fn poll_write_ready(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        (**self).poll_write_ready(cx)
    }

    fn poll_flush(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        (**self).poll_flush(cx)
    }

    fn poll_shutdown(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        (**self).poll_shutdown(cx)
    }
}

pub async fn connect_tcp<Ws: WithSocket>(
    host: &str,
    port: u16,
    with_socket: Ws,
) -> crate::Result<Ws::Output> {
    #[cfg(feature = "_rt-tokio")]
    if crate::rt::rt_tokio::available() {
        let addresses = tokio::net::lookup_host((host, port)).await?;
        let stream = connect_tcp_tokio(addresses).await?;
        return Ok(with_socket.with_socket(stream).await);
    }

    cfg_if! {
        if #[cfg(feature = "_rt-async-io")] {
            Ok(with_socket.with_socket(connect_tcp_async_io(host, port).await?).await)
        } else {
            crate::rt::missing_rt((host, port, with_socket))
        }
    }
}

// Keep candidate ownership below the protocol continuation: only the winner
// can perform TLS or database opening, and cancellation drops every attempt.
#[cfg(feature = "_rt-tokio")]
async fn connect_tcp_tokio(
    addresses: impl IntoIterator<Item = std::net::SocketAddr>,
) -> io::Result<tokio::net::TcpStream> {
    use futures_util::stream::{FuturesUnordered, StreamExt};

    let mut attempts: FuturesUnordered<_> =
        addresses
            .into_iter()
            .enumerate()
            .map(|(index, address)| async move {
                (index, tokio::net::TcpStream::connect(address).await)
            })
            .collect();
    let mut last_error = None;
    while let Some((index, result)) = attempts.next().await {
        match result {
            Ok(stream) => {
                drop(attempts);
                return Ok(stream);
            }
            Err(error) => {
                // Match Tokio's serial connect error, regardless of which
                // candidate finishes last in this concurrent race.
                if last_error
                    .as_ref()
                    .is_none_or(|(last_index, _)| index > *last_index)
                {
                    last_error = Some((index, error));
                }
            }
        }
    }
    Err(last_error.map(|(_, error)| error).unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "could not resolve to any addresses",
        )
    }))
}

/// Open a TCP socket to `host` and `port`.
///
/// If `host` is a hostname, attempt to connect to each address it resolves to.
///
/// This implements the same behavior as [`tokio::net::TcpStream::connect()`].
#[cfg(feature = "_rt-async-io")]
async fn connect_tcp_async_io(host: &str, port: u16) -> crate::Result<impl Socket> {
    use async_io::Async;
    use std::net::{IpAddr, TcpStream, ToSocketAddrs};

    // IPv6 addresses in URLs will be wrapped in brackets and the `url` crate doesn't trim those.
    let host = host.trim_matches(&['[', ']'][..]);

    if let Ok(addr) = host.parse::<IpAddr>() {
        return Ok(Async::<TcpStream>::connect((addr, port)).await?);
    }

    let host = host.to_string();

    let addresses = crate::rt::spawn_blocking(move || {
        let addr = (host.as_str(), port);
        ToSocketAddrs::to_socket_addrs(&addr)
    })
    .await?;

    let mut last_err = None;

    // Loop through all the Socket Addresses that the hostname resolves to
    for socket_addr in addresses {
        match Async::<TcpStream>::connect(socket_addr).await {
            Ok(stream) => return Ok(stream),
            Err(e) => last_err = Some(e),
        }
    }

    // If we reach this point, it means we failed to connect to any of the addresses.
    // Return the last error we encountered, or a custom error if the hostname didn't resolve to any address.
    Err(last_err
        .unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "Hostname did not resolve to any addresses",
            )
        })
        .into())
}

/// Connect a Unix Domain Socket at the given path.
///
/// Returns an error if Unix Domain Sockets are not supported on this platform.
pub async fn connect_uds<P: AsRef<Path>, Ws: WithSocket>(
    path: P,
    with_socket: Ws,
) -> crate::Result<Ws::Output> {
    #[cfg(unix)]
    {
        #[cfg(feature = "_rt-tokio")]
        if crate::rt::rt_tokio::available() {
            use tokio::net::UnixStream;

            let stream = UnixStream::connect(path).await?;

            return Ok(with_socket.with_socket(stream).await);
        }

        cfg_if! {
            if #[cfg(feature = "_rt-async-io")] {
                use async_io::Async;
                use std::os::unix::net::UnixStream;

                let stream = Async::<UnixStream>::connect(path).await?;

                Ok(with_socket.with_socket(stream).await)
            } else {
                crate::rt::missing_rt((path, with_socket))
            }
        }
    }

    #[cfg(not(unix))]
    {
        drop((path, with_socket));

        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Unix domain sockets are not supported on this platform",
        )
        .into())
    }
}

#[cfg(all(test, feature = "_rt-tokio"))]
mod tests {
    use super::connect_tcp_tokio;
    use std::future::Future;
    use std::io;
    use std::net::SocketAddr;
    use std::task::Poll;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpSocket, TcpStream};

    fn run(test: impl Future<Output = ()>) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(10), test)
                    .await
                    .expect("native socket test is bounded");
            });
    }

    // The existing outbound HTTP fixture uses this same loopback technique:
    // keep the accept queue full so further TCP handshakes remain pending.
    async fn unresponsive_listener() -> (SocketAddr, TcpListener, Vec<TcpStream>) {
        let socket = TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let listener = socket.listen(1).unwrap();
        let address = listener.local_addr().unwrap();
        let mut queued = Vec::new();
        for _ in 0..32 {
            match tokio::time::timeout(Duration::from_millis(100), TcpStream::connect(address))
                .await
            {
                Ok(stream) => queued.push(stream.unwrap()),
                Err(_) => return (address, listener, queued),
            }
        }
        panic!("the loopback accept queue never filled");
    }

    #[test]
    fn pending_first_candidate_does_not_starve_a_healthy_socket() {
        run(async {
            let (pending, _listener, _queued) = unresponsive_listener().await;
            let healthy = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let mut stream = tokio::time::timeout(
                Duration::from_secs(1),
                connect_tcp_tokio([pending, healthy.local_addr().unwrap()]),
            )
            .await
            .expect("a pending first candidate must not spend the caller's budget")
            .unwrap();
            assert_eq!(stream.peer_addr().unwrap(), healthy.local_addr().unwrap());
            let (mut peer, _) = healthy.accept().await.unwrap();
            stream.write_all(b"selected").await.unwrap();
            let mut received = [0; 8];
            peer.read_exact(&mut received).await.unwrap();
            assert_eq!(&received, b"selected");
        });
    }

    #[test]
    fn empty_resolution_preserves_tokio_invalid_input() {
        run(async {
            assert_eq!(
                connect_tcp_tokio([]).await.unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        });
    }

    #[test]
    fn all_failure_reports_last_resolver_address_even_when_it_fails_first() {
        run(async {
            let (pending, listener, _queued) = unresponsive_listener().await;
            // Broadcast cannot be connected as TCP; unlike the loopback
            // refusal below, its immediate OS failure has a distinct code.
            let last: SocketAddr = "255.255.255.255:9".parse().unwrap();
            let expected = TcpStream::connect(last).await.unwrap_err();
            assert_ne!(expected.kind(), io::ErrorKind::ConnectionRefused);
            let mut connect = Box::pin(connect_tcp_tokio([pending, last]));
            // Start every attempt while the first address is still pending.
            std::future::poll_fn(|cx| {
                assert!(matches!(connect.as_mut().poll(cx), Poll::Pending));
                Poll::Ready(())
            })
            .await;
            // The earlier resolver address now refuses its in-flight dial;
            // that later completion must not replace the last address's error.
            drop(listener);
            let error = connect.await.unwrap_err();
            assert_eq!(error.kind(), expected.kind());
            assert_eq!(error.raw_os_error(), expected.raw_os_error());
        });
    }
}

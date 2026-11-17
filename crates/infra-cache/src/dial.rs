use std::io;
use std::net::SocketAddr;
use std::pin::pin;
use std::time::Duration;

use futures_util::stream::{FuturesUnordered, StreamExt};
use redis::aio::MultiplexedConnection;
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_util::either::Either;
use tokio_util::task::AbortOnDropHandle;

const HAPPY_EYEBALLS_DELAY: Duration = Duration::from_millis(250);
const KEEPALIVE_TIME: Duration = Duration::from_secs(30);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
const KEEPALIVE_RETRIES: u32 = 3;
#[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
const USER_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct Dial {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) tls: Option<Tls>,
}

pub(crate) struct Tls {
    pub(crate) connector: TlsConnector,
    pub(crate) server_name: ServerName<'static>,
}

pub(crate) struct Dialed {
    pub(crate) connection: MultiplexedConnection,
    pub(crate) driver: AbortOnDropHandle<()>,
}

impl Dial {
    pub(crate) async fn connect(
        &self,
        settings: &redis::RedisConnectionInfo,
        config: redis::AsyncConnectionConfig,
    ) -> redis::RedisResult<Dialed> {
        let tcp = self.connect_tcp().await?;
        let stream = match &self.tls {
            None => Either::Left(tcp),
            Some(tls) => Either::Right(tls.connector.connect(tls.server_name.clone(), tcp).await?),
        };
        let (connection, driver) =
            MultiplexedConnection::new_with_config(settings, stream, config).await?;
        Ok(Dialed {
            connection,
            driver: AbortOnDropHandle::new(tokio::spawn(driver)),
        })
    }

    async fn connect_tcp(&self) -> io::Result<TcpStream> {
        let addresses = tokio::net::lookup_host((self.host.as_str(), self.port)).await?;
        connect_staggered(addresses, connect_address).await
    }
}

async fn connect_staggered<F, Fut, T>(
    addresses: impl IntoIterator<Item = SocketAddr>,
    mut dial: F,
) -> io::Result<T>
where
    F: FnMut(SocketAddr) -> Fut,
    Fut: Future<Output = io::Result<T>>,
{
    let mut candidates = addresses.into_iter();
    let Some(first) = candidates.next() else {
        return Err(io::ErrorKind::NotFound.into());
    };

    let mut in_flight = FuturesUnordered::new();
    in_flight.push(dial(first));
    let mut last_error = None;

    for next in candidates {
        let mut delay = pin!(tokio::time::sleep(HAPPY_EYEBALLS_DELAY));
        while !in_flight.is_empty() {
            let outcome = tokio::select! {
                biased;
                result = in_flight.next() => result,
                () = &mut delay => None,
            };
            match outcome {
                Some(Ok(stream)) => return Ok(stream),
                Some(Err(error)) => last_error = Some(error),
                None => break,
            }
        }
        in_flight.push(dial(next));
    }

    while let Some(result) = in_flight.next().await {
        match result {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
    }

    Err(last_error.unwrap_or_else(|| io::ErrorKind::NotFound.into()))
}

async fn connect_address(address: SocketAddr) -> io::Result<TcpStream> {
    let stream = TcpStream::connect(address).await?;
    stream.set_nodelay(true)?;
    let socket = socket2::SockRef::from(&stream);
    socket.set_tcp_keepalive(
        &socket2::TcpKeepalive::new()
            .with_time(KEEPALIVE_TIME)
            .with_interval(KEEPALIVE_INTERVAL)
            .with_retries(KEEPALIVE_RETRIES),
    )?;
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    socket.set_tcp_user_timeout(Some(USER_TIMEOUT))?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    use std::sync::Arc;
    use std::time::Duration;

    use parking_lot::Mutex;
    use tokio::time::{Instant, sleep};

    use super::{HAPPY_EYEBALLS_DELAY, connect_staggered};

    const IPV6_ADDR: SocketAddr = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 6379);
    const IPV4_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 6379);
    const FALLBACK_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 6380);

    #[tokio::test(start_paused = true)]
    async fn first_prompt_success_skips_second_address() {
        let start = Instant::now();
        let attempts = Mutex::new(Vec::new());
        let immediate = connect_staggered([IPV6_ADDR, IPV4_ADDR], |address| {
            attempts.lock().push((address, start.elapsed()));
            async move {
                if address == IPV6_ADDR {
                    return Ok(address);
                }
                Err(io::Error::from(io::ErrorKind::ConnectionRefused))
            }
        })
        .await
        .expect("first address connects immediately");

        assert_eq!(immediate, IPV6_ADDR);
        assert_eq!(start.elapsed(), Duration::ZERO);
        let immediate_attempts = std::mem::take(&mut *attempts.lock());
        assert_eq!(immediate_attempts, vec![(IPV6_ADDR, Duration::ZERO)]);

        let sub_delay = Duration::from_millis(50);
        let before_delay = connect_staggered([IPV6_ADDR, IPV4_ADDR], |address| {
            attempts.lock().push((address, start.elapsed()));
            async move {
                if address == IPV6_ADDR {
                    sleep(sub_delay).await;
                    return Ok(address);
                }
                Err(io::Error::from(io::ErrorKind::ConnectionRefused))
            }
        })
        .await
        .expect("first address connects before stagger delay");

        assert_eq!(before_delay, IPV6_ADDR);
        assert_eq!(start.elapsed(), sub_delay);
        let sub_delay_attempts = std::mem::take(&mut *attempts.lock());
        assert_eq!(sub_delay_attempts, vec![(IPV6_ADDR, Duration::ZERO)]);
    }

    #[tokio::test(start_paused = true)]
    async fn immediate_failure_dials_second_address_without_delay() {
        let start = Instant::now();
        let attempts = Mutex::new(Vec::new());
        let connected = connect_staggered([IPV6_ADDR, IPV4_ADDR], |address| {
            attempts.lock().push((address, start.elapsed()));
            async move {
                if address == IPV6_ADDR {
                    return Err(io::Error::from(io::ErrorKind::ConnectionRefused));
                }
                Ok(address)
            }
        })
        .await
        .expect("second address connects immediately after refusal");

        assert_eq!(connected, IPV4_ADDR);
        assert_eq!(start.elapsed(), Duration::ZERO);
        let recorded = std::mem::take(&mut *attempts.lock());
        assert_eq!(
            recorded,
            vec![(IPV6_ADDR, Duration::ZERO), (IPV4_ADDR, Duration::ZERO)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_first_address_starts_second_after_happy_eyeballs_delay() {
        let start = Instant::now();
        let attempts = Mutex::new(Vec::new());
        let stalled_lifetime = Arc::new(());
        let connected = connect_staggered([IPV6_ADDR, IPV4_ADDR], |address| {
            attempts.lock().push((address, start.elapsed()));
            let marker = (address == IPV6_ADDR).then(|| stalled_lifetime.clone());
            async move {
                if address == IPV6_ADDR {
                    let result = std::future::pending().await;
                    drop(marker);
                    return result;
                }
                io::Result::Ok(address)
            }
        })
        .await
        .expect("second address wins after stagger delay");

        assert_eq!(connected, IPV4_ADDR);
        assert_eq!(start.elapsed(), HAPPY_EYEBALLS_DELAY);
        assert_eq!(Arc::strong_count(&stalled_lifetime), 1);
        let recorded = std::mem::take(&mut *attempts.lock());
        assert_eq!(
            recorded,
            vec![
                (IPV6_ADDR, Duration::ZERO),
                (IPV4_ADDR, HAPPY_EYEBALLS_DELAY),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn intermediate_failure_preserves_stagger_schedule_while_attempt_in_flight() {
        let start = Instant::now();
        let attempts = Mutex::new(Vec::new());
        let connected = connect_staggered([IPV6_ADDR, IPV4_ADDR, FALLBACK_ADDR], |address| {
            attempts.lock().push((address, start.elapsed()));
            async move {
                if address == IPV6_ADDR {
                    return std::future::pending().await;
                }
                if address == IPV4_ADDR {
                    sleep(Duration::from_millis(50)).await;
                    return Err(io::Error::from(io::ErrorKind::ConnectionReset));
                }
                Ok(address)
            }
        })
        .await
        .expect("third address wins on original stagger cadence");

        assert_eq!(connected, FALLBACK_ADDR);
        assert_eq!(start.elapsed(), HAPPY_EYEBALLS_DELAY * 2);
        let recorded = std::mem::take(&mut *attempts.lock());
        assert_eq!(
            recorded,
            vec![
                (IPV6_ADDR, Duration::ZERO),
                (IPV4_ADDR, HAPPY_EYEBALLS_DELAY),
                (FALLBACK_ADDR, HAPPY_EYEBALLS_DELAY * 2),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn empty_addresses_returns_not_found() {
        let mut called = false;
        let error = connect_staggered(std::iter::empty(), |_address| {
            called = true;
            async { io::Result::Ok(IPV4_ADDR) }
        })
        .await
        .expect_err("empty address list must fail");

        assert!(!called);
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[tokio::test(start_paused = true)]
    async fn all_addresses_failing_returns_last_observed_error() {
        let sequential_error = connect_staggered([IPV6_ADDR, IPV4_ADDR], |address| async move {
            if address == IPV6_ADDR {
                return Err(io::Error::from(io::ErrorKind::ConnectionRefused));
            }
            Err::<SocketAddr, _>(io::Error::from(io::ErrorKind::PermissionDenied))
        })
        .await
        .expect_err("all addresses fail sequentially");
        assert_eq!(sequential_error.kind(), io::ErrorKind::PermissionDenied);

        let start = Instant::now();
        let slow_failure = HAPPY_EYEBALLS_DELAY + Duration::from_millis(50);
        let out_of_order_error = connect_staggered(
            [IPV6_ADDR, IPV4_ADDR, FALLBACK_ADDR],
            |address| async move {
                if address == IPV6_ADDR {
                    return Err(io::Error::from(io::ErrorKind::ConnectionRefused));
                }
                if address == IPV4_ADDR {
                    sleep(slow_failure).await;
                    return Err(io::Error::from(io::ErrorKind::TimedOut));
                }
                Err::<SocketAddr, _>(io::Error::from(io::ErrorKind::AddrNotAvailable))
            },
        )
        .await
        .expect_err("all addresses fail with out-of-order completion");

        assert_eq!(out_of_order_error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(start.elapsed(), slow_failure);
    }
}

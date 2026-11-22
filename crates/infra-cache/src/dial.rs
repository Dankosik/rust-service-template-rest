use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use redis::aio::MultiplexedConnection;
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_util::either::Either;
use tokio_util::task::{AbortOnDropHandle, TaskTracker};

use crate::ServerEndpoint;
use crate::admission::Admitted;

const KEEPALIVE_TIME: Duration = Duration::from_secs(30);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
const KEEPALIVE_RETRIES: u32 = 3;
#[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
const TCP_USER_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) enum Transport {
    Plaintext,
    Tls(Tls),
}

impl Transport {
    pub(crate) const fn is_tls(&self) -> bool {
        matches!(self, Self::Tls(_))
    }
}

pub(crate) struct Tls {
    pub(crate) connector: TlsConnector,
    pub(crate) server_name: ServerName<'static>,
}

pub(crate) struct Dialed {
    pub(crate) connection: MultiplexedConnection,
    pub(crate) driver: AbortOnDropHandle<()>,
}

impl Admitted {
    pub(crate) async fn connect(
        &self,
        settings: &redis::RedisConnectionInfo,
        config: redis::AsyncConnectionConfig,
        tasks: &TaskTracker,
    ) -> redis::RedisResult<Dialed> {
        let tcp = connect_tcp(&self.server).await?;
        let stream = match &self.transport {
            Transport::Plaintext => Either::Left(tcp),
            Transport::Tls(tls) => {
                Either::Right(tls.connector.connect(tls.server_name.clone(), tcp).await?)
            }
        };
        let (connection, driver) =
            MultiplexedConnection::new_with_config(settings, stream, config).await?;
        Ok(Dialed {
            connection,
            driver: AbortOnDropHandle::new(tasks.spawn(driver)),
        })
    }
}

async fn connect_tcp(server: &ServerEndpoint) -> io::Result<TcpStream> {
    let mut addresses = tokio::net::lookup_host((server.host.as_str(), server.port))
        .await?
        .peekable();
    if addresses.peek().is_none() {
        return Err(io::ErrorKind::NotFound.into());
    }
    let (stream, _) = futures_util::future::select_ok(
        addresses.map(|address| Box::pin(connect_address(address))),
    )
    .await?;
    Ok(stream)
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
    socket.set_tcp_user_timeout(Some(TCP_USER_TIMEOUT))?;
    Ok(stream)
}

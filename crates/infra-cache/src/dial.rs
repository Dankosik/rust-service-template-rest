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

const KEEPALIVE_TIME: Duration = Duration::from_secs(30);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
const KEEPALIVE_RETRIES: u32 = 3;
#[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
const USER_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct Tls {
    pub(crate) connector: TlsConnector,
    pub(crate) server_name: ServerName<'static>,
}

pub(crate) struct Dialed {
    pub(crate) connection: MultiplexedConnection,
    pub(crate) driver: AbortOnDropHandle<()>,
}

impl ServerEndpoint {
    pub(crate) async fn connect(
        &self,
        tls: Option<&Tls>,
        settings: &redis::RedisConnectionInfo,
        config: redis::AsyncConnectionConfig,
        tasks: &TaskTracker,
    ) -> redis::RedisResult<Dialed> {
        let tcp = self.connect_tcp().await?;
        let stream = match tls {
            None => Either::Left(tcp),
            Some(tls) => Either::Right(tls.connector.connect(tls.server_name.clone(), tcp).await?),
        };
        let (connection, driver) =
            MultiplexedConnection::new_with_config(settings, stream, config).await?;
        Ok(Dialed {
            connection,
            driver: AbortOnDropHandle::new(tasks.spawn(driver)),
        })
    }

    async fn connect_tcp(&self) -> io::Result<TcpStream> {
        let addresses: Vec<SocketAddr> = tokio::net::lookup_host((self.host.as_str(), self.port))
            .await?
            .collect();
        if addresses.is_empty() {
            return Err(io::ErrorKind::NotFound.into());
        }
        let (stream, _) = futures_util::future::select_ok(
            addresses
                .into_iter()
                .map(|address| Box::pin(connect_address(address))),
        )
        .await?;
        Ok(stream)
    }
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

use std::{fmt, future::Future, io, net::SocketAddr, pin::Pin, time::Duration};

use hickory_resolver::{
    Resolver as HickoryResolver,
    config::ResolveHosts,
    net::runtime::{RuntimeProvider, Spawn, TokioTime, iocompat::AsyncIoTokioAsStd},
};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use tokio::{
    net::{TcpSocket, TcpStream, UdpSocket},
    time::timeout,
};
use tokio_util::{
    sync::CancellationToken,
    task::{TaskTracker, task_tracker::TaskTrackerToken},
};

use crate::{ResolveError, admit_address};

/// Reqwest DNS resolver with public-address admission and tracked work.
#[derive(Clone)]
pub struct PublicAddressResolver {
    config: hickory_resolver::config::ResolverConfig,
    options: hickory_resolver::config::ResolverOpts,
    tracker: TaskTracker,
    cancel: CancellationToken,
}

impl fmt::Debug for PublicAddressResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PublicAddressResolver([RUNTIME_OWNED])")
    }
}

impl PublicAddressResolver {
    /// Snapshots system resolver configuration without performing a lookup.
    ///
    /// # Errors
    ///
    /// Returns `ResolveError::Configuration` when system DNS settings cannot be read.
    pub fn new(tracker: TaskTracker, cancel: CancellationToken) -> Result<Self, ResolveError> {
        let (config, options) = hickory_resolver::system_conf::read_system_conf()
            .map_err(|_| ResolveError::Configuration)?;
        Ok(Self {
            config,
            options,
            tracker,
            cancel,
        })
    }

    async fn lookup(&self, host: String) -> Result<Addrs, ResolveError> {
        // This token exists before the cancellation check. The lookup and all
        // RuntimeProvider/Spawn clones retain it until their work is gone.
        let lookup_scope = self.tracker.token();
        let lookup_cancel = self.cancel.child_token();
        let runtime = TrackedRuntime::new(
            self.tracker.clone(),
            self.cancel.clone(),
            lookup_cancel.clone(),
            lookup_scope.clone(),
        );
        let mut builder = HickoryResolver::builder_with_config(self.config.clone(), runtime);
        *builder.options_mut() = self.options.clone();
        builder.options_mut().use_hosts_file = ResolveHosts::Never;
        let resolver = builder.build().map_err(|_| ResolveError::Configuration)?;
        // Declare this after the resolver so cancellation is signalled before
        // Hickory drops any spawned handles on future cancellation/drop.
        let _cancel_on_drop = CancelOnDrop(lookup_cancel.clone());

        let lookup = async move {
            let response = resolver
                .lookup_ip(host)
                .await
                .map_err(|_| ResolveError::Lookup)?;
            let addresses: Vec<_> = response.iter().collect();
            if addresses.is_empty()
                || addresses
                    .iter()
                    .any(|address| admit_address(*address).is_err())
            {
                return Err(ResolveError::Denied);
            }
            Ok(Box::new(
                addresses
                    .into_iter()
                    .map(|address| SocketAddr::new(address, 0)),
            ) as Addrs)
        };
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(ResolveError::Cancelled),
            () = lookup_cancel.cancelled() => Err(ResolveError::Cancelled),
            result = lookup => result,
        }
    }
}

impl Resolve for PublicAddressResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let resolver = self.clone();
        let host = name.as_str().to_owned();
        Box::pin(async move { resolver.lookup(host).await.map_err(Into::into) })
    }
}

struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[derive(Clone)]
struct TrackedRuntime {
    tracker: TaskTracker,
    root_cancel: CancellationToken,
    lookup_cancel: CancellationToken,
    scope: TaskTrackerToken,
}

impl TrackedRuntime {
    fn new(
        tracker: TaskTracker,
        root_cancel: CancellationToken,
        lookup_cancel: CancellationToken,
        scope: TaskTrackerToken,
    ) -> Self {
        Self {
            tracker,
            root_cancel,
            lookup_cancel,
            scope,
        }
    }
}

#[derive(Clone)]
struct TrackedHandle {
    tracker: TaskTracker,
    root_cancel: CancellationToken,
    lookup_cancel: CancellationToken,
    scope: TaskTrackerToken,
}

impl Spawn for TrackedHandle {
    fn spawn_bg(&mut self, future: impl Future<Output = ()> + Send + 'static) {
        if self.root_cancel.is_cancelled() || self.lookup_cancel.is_cancelled() {
            return;
        }
        let root_cancel = self.root_cancel.clone();
        let lookup_cancel = self.lookup_cancel.clone();
        let scope = self.scope.clone();
        self.tracker.spawn(async move {
            let _scope = scope;
            tokio::select! {
                biased;
                () = root_cancel.cancelled() => {},
                () = lookup_cancel.cancelled() => {},
                () = future => {},
            }
        });
    }
}

impl RuntimeProvider for TrackedRuntime {
    type Handle = TrackedHandle;
    type Timer = TokioTime;
    type Udp = UdpSocket;
    type Tcp = AsyncIoTokioAsStd<TcpStream>;

    fn create_handle(&self) -> Self::Handle {
        TrackedHandle {
            tracker: self.tracker.clone(),
            root_cancel: self.root_cancel.clone(),
            lookup_cancel: self.lookup_cancel.clone(),
            scope: self.scope.clone(),
        }
    }

    fn connect_tcp(
        &self,
        server_addr: SocketAddr,
        bind_addr: Option<SocketAddr>,
        wait_for: Option<Duration>,
    ) -> Pin<Box<dyn Send + Future<Output = Result<Self::Tcp, io::Error>>>> {
        let root_cancel = self.root_cancel.clone();
        let lookup_cancel = self.lookup_cancel.clone();
        let scope = self.scope.clone();
        Box::pin(async move {
            let _scope = scope;
            if root_cancel.is_cancelled() || lookup_cancel.is_cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "egress DNS cancelled",
                ));
            }
            let socket = match server_addr {
                SocketAddr::V4(_) => TcpSocket::new_v4(),
                SocketAddr::V6(_) => TcpSocket::new_v6(),
            }?;
            if let Some(bind_addr) = bind_addr {
                socket.bind(bind_addr)?;
            }
            socket.set_nodelay(true)?;
            let connect = socket.connect(server_addr);
            let wait_for = wait_for.unwrap_or(Duration::from_secs(5));
            tokio::select! {
                biased;
                () = root_cancel.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "egress DNS cancelled")),
                () = lookup_cancel.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "egress DNS cancelled")),
                result = timeout(wait_for, connect) => match result {
                    Ok(Ok(stream)) => Ok(AsyncIoTokioAsStd(stream)),
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "egress DNS TCP connect timed out")),
                },
            }
        })
    }

    fn bind_udp(
        &self,
        local_addr: SocketAddr,
        _server_addr: SocketAddr,
    ) -> Pin<Box<dyn Send + Future<Output = Result<Self::Udp, io::Error>>>> {
        let root_cancel = self.root_cancel.clone();
        let lookup_cancel = self.lookup_cancel.clone();
        let scope = self.scope.clone();
        Box::pin(async move {
            let _scope = scope;
            tokio::select! {
                biased;
                () = root_cancel.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "egress DNS cancelled")),
                () = lookup_cancel.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "egress DNS cancelled")),
                result = UdpSocket::bind(local_addr) => result,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use hickory_resolver::net::runtime::Spawn;
    use tokio_util::{sync::CancellationToken, task::TaskTracker};

    use super::TrackedHandle;

    #[tokio::test]
    async fn cancelled_late_spawn_never_polls_dns_work() {
        let tracker = TaskTracker::new();
        let root_cancel = CancellationToken::new();
        let lookup_cancel = root_cancel.child_token();
        let mut handle = TrackedHandle {
            tracker: tracker.clone(),
            root_cancel: root_cancel.clone(),
            lookup_cancel,
            scope: tracker.token(),
        };
        let polled = Arc::new(AtomicBool::new(false));
        root_cancel.cancel();
        let observed = polled.clone();
        handle.spawn_bg(async move {
            observed.store(true, Ordering::SeqCst);
        });
        tokio::task::yield_now().await;
        assert!(!polled.load(Ordering::SeqCst));

        drop(handle);
        tracker.close();
        tokio::time::timeout(Duration::from_secs(1), tracker.wait())
            .await
            .expect("late spawn must not retain the tracker");
    }

    #[tokio::test]
    async fn cancellation_drops_tracked_dns_wrapper_before_join() {
        let tracker = TaskTracker::new();
        let root_cancel = CancellationToken::new();
        let mut handle = TrackedHandle {
            tracker: tracker.clone(),
            root_cancel: root_cancel.clone(),
            lookup_cancel: root_cancel.child_token(),
            scope: tracker.token(),
        };
        handle.spawn_bg(std::future::pending());
        root_cancel.cancel();

        drop(handle);
        tracker.close();
        tokio::time::timeout(Duration::from_secs(1), tracker.wait())
            .await
            .expect("cancellation must release tracked DNS work");
    }
}

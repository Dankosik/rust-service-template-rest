//! DNS resolution whose work and connection targets stay under auth ownership.

use std::{
    future::Future,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    pin::Pin,
    time::Duration,
};

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

use crate::Failure;

/// Auth-private reqwest resolver.  It snapshots system DNS configuration once
/// and constructs a fresh Hickory resolver for each provider lookup.
#[derive(Clone)]
pub(super) struct Resolver {
    config: hickory_resolver::config::ResolverConfig,
    options: hickory_resolver::config::ResolverOpts,
    tracker: TaskTracker,
    cancel: CancellationToken,
}

impl Resolver {
    pub(super) fn new(tracker: TaskTracker, cancel: CancellationToken) -> Result<Self, Failure> {
        let (config, options) =
            hickory_resolver::system_conf::read_system_conf().map_err(|_| Failure::Unavailable)?;
        Ok(Self {
            config,
            options,
            tracker,
            cancel,
        })
    }

    async fn lookup(&self, host: String) -> Result<Addrs, io::Error> {
        // This token exists before the cancellation check.  The lookup and all
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
        let resolver = builder
            .build()
            .map_err(|_| io::Error::other("resolver configuration unavailable"))?;
        // Declare this after the resolver so cancellation is signalled before
        // Hickory drops any spawned handles on future cancellation/drop.
        let _cancel_on_drop = CancelOnDrop(lookup_cancel.clone());

        let lookup = async move {
            let response = resolver
                .lookup_ip(host)
                .await
                .map_err(|_| io::Error::other("provider DNS lookup failed"))?;
            let addresses: Vec<_> = response.iter().collect();
            if addresses.is_empty()
                || addresses
                    .iter()
                    .any(|address| admit_address(*address).is_err())
            {
                return Err(io::Error::other("provider DNS address denied"));
            }
            Ok(Box::new(
                addresses
                    .into_iter()
                    .map(|address| SocketAddr::new(address, 0)),
            ) as Addrs)
        };
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "provider DNS cancelled")),
            () = lookup_cancel.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "provider DNS cancelled")),
            result = lookup => result,
        }
    }
}

impl Resolve for Resolver {
    fn resolve(&self, name: Name) -> Resolving {
        let resolver = self.clone();
        let host = name.as_str().to_owned();
        Box::pin(async move { resolver.lookup(host).await.map_err(Into::into) })
    }
}

// template:begin oidc-jwt:authn-dns-raw-answer-resolver
/// Test-only resolver that places raw answers immediately upstream of the
/// production address predicate.  It cannot alter the production resolver or
/// admit a private destination.
#[cfg(test)]
#[derive(Clone)]
pub(super) struct RawAnswerResolver {
    host: String,
    answers: Vec<IpAddr>,
}

#[cfg(test)]
impl RawAnswerResolver {
    pub(super) fn new(host: &str, answers: Vec<IpAddr>) -> Self {
        Self {
            host: host.to_owned(),
            answers,
        }
    }
}

#[cfg(test)]
impl Resolve for RawAnswerResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = self.host.clone();
        let answers = self.answers.clone();
        Box::pin(async move {
            if !name.as_str().eq_ignore_ascii_case(&host)
                || answers.is_empty()
                || answers
                    .iter()
                    .any(|address| admit_address(*address).is_err())
            {
                return Err(io::Error::other("provider DNS address denied").into());
            }
            Ok(Box::new(
                answers
                    .into_iter()
                    .map(|address| SocketAddr::new(address, 0)),
            ) as Addrs)
        })
    }
}
// template:end oidc-jwt:authn-dns-raw-answer-resolver

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
                    "provider DNS cancelled",
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
                () = root_cancel.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "provider DNS cancelled")),
                () = lookup_cancel.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "provider DNS cancelled")),
                result = timeout(wait_for, connect) => match result {
                    Ok(Ok(stream)) => Ok(AsyncIoTokioAsStd(stream)),
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "provider DNS TCP connect timed out")),
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
                () = root_cancel.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "provider DNS cancelled")),
                () = lookup_cancel.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "provider DNS cancelled")),
                result = UdpSocket::bind(local_addr) => result,
            }
        })
    }
}

pub(super) fn admit_address(address: IpAddr) -> Result<(), Failure> {
    if is_public_global(address) {
        Ok(())
    } else {
        Err(Failure::Unavailable)
    }
}

fn is_public_global(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_public_global_v4(address),
        IpAddr::V6(address) => address
            .to_ipv4_mapped()
            .map_or_else(|| is_public_global_v6(address), is_public_global_v4),
    }
}

fn is_public_global_v4(address: Ipv4Addr) -> bool {
    let [a, b, c, d] = address.octets();
    if a == 192 && b == 0 && c == 0 && matches!(d, 9 | 10) {
        return true;
    }
    !matches!(
        (a, b, c),
        (0 | 10 | 127 | 224..=255, _, _)
            | (100, 64..=127, _)
            | (169, 254, _)
            | (172, 16..=31, _)
            | (192, 0, 0 | 2)
            | (192, 168, _)
            | (192, 88, 99)
            | (198, 18..=19, _)
            | (198, 51, 100)
            | (203, 0, 113)
    )
}

fn is_public_global_v6(address: Ipv6Addr) -> bool {
    if address.is_unspecified()
        || address.is_loopback()
        || address.is_multicast()
        || address.is_unique_local()
        || address.is_unicast_link_local()
    {
        return false;
    }
    let segments = address.segments();
    if segments[0] == 0x2001 && segments[1] <= 0x01ff {
        return is_global_2001_exception(segments);
    }
    !matches!(
        segments,
        [0x0064, 0xff9b, 0x0001, _, _, _, _, _]
            | [0x0100, 0, 0, 0 | 1, _, _, _, _]
            | [0x2001, 0x0db8, _, _, _, _, _, _]
            | [0x3fff, 0x0000..=0x0fff, _, _, _, _, _, _]
            | [0x5f00, _, _, _, _, _, _, _]
    )
}

/// The IANA `2001::/23` protocol-assignment block is non-global except for
/// its specifically registered more-specific allocations.
fn is_global_2001_exception(segments: [u16; 8]) -> bool {
    matches!(
        segments,
        [0x2001, 0x0001, 0, 0, 0, 0, 0, 1..=3]
            | [0x2001, 0x0003 | 0x0020..=0x003f, _, _, _, _, _, _]
            | [0x2001, 0x0004, 0x0112, _, _, _, _, _]
    )
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr, Ipv6Addr},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use hickory_resolver::net::runtime::Spawn;
    use tokio_util::{sync::CancellationToken, task::TaskTracker};

    use super::{TrackedHandle, is_public_global};

    #[test]
    fn denies_special_and_mapped_addresses() {
        for address in [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(192, 0, 0, 8)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            IpAddr::V6("::ffff:127.0.0.1".parse().expect("mapped test address")),
            IpAddr::V6("2001:db8::1".parse().expect("documentation test address")),
        ] {
            assert!(!is_public_global(address), "{address} must be denied");
        }
    }

    #[test]
    fn admits_public_global_addresses_and_registry_exceptions() {
        for address in [
            IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
            IpAddr::V4(Ipv4Addr::new(192, 0, 0, 9)),
            IpAddr::V4(Ipv4Addr::new(192, 0, 1, 1)),
            IpAddr::V4(Ipv4Addr::new(192, 2, 1, 1)),
            IpAddr::V6("2001:1::1".parse().expect("PCP exception")),
            IpAddr::V6("2001:1::2".parse().expect("TURN exception")),
            IpAddr::V6("2001:1::3".parse().expect("DNS-SD exception")),
            IpAddr::V6("2001:3::1".parse().expect("AMT exception")),
            IpAddr::V6("2001:4:112::1".parse().expect("AS112 exception")),
            IpAddr::V6("2001:20::1".parse().expect("ORCHIDv2 exception")),
            IpAddr::V6("2001:3f::1".parse().expect("DET exception")),
            IpAddr::V6("2606:4700:4700::1111".parse().expect("public test address")),
        ] {
            assert!(is_public_global(address), "{address} must be admitted");
        }
    }

    #[test]
    fn denies_current_iana_non_global_prefixes_at_their_exact_boundaries() {
        for address in [
            IpAddr::V4(Ipv4Addr::new(192, 0, 0, 8)),
            IpAddr::V4(Ipv4Addr::new(192, 0, 0, 11)),
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 0)),
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 255)),
            IpAddr::V6("100::1".parse().expect("discard-only prefix")),
            IpAddr::V6("100:0:0:1::1".parse().expect("dummy prefix")),
            IpAddr::V6("5f00::1".parse().expect("SRv6 prefix")),
            IpAddr::V6("2001:1::4".parse().expect("non-exception host")),
            IpAddr::V6("2001:2::1".parse().expect("benchmark prefix")),
            IpAddr::V6("2001:4:111::1".parse().expect("AS112 adjacent prefix")),
            IpAddr::V6("2001:10::1".parse().expect("deprecated ORCHID prefix")),
            IpAddr::V6("2001:40::1".parse().expect("IETF assignment boundary")),
        ] {
            assert!(!is_public_global(address), "{address} must be denied");
        }
    }

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

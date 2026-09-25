use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use ipnet::{Ipv4Net, Ipv6Net};

use crate::ResolveError;

// Policy snapshot reviewed 2026-09-25 against the IANA special-purpose
// registries: https://www.iana.org/assignments/iana-ipv4-special-registry/ and
// https://www.iana.org/assignments/iana-ipv6-special-registry/.
// Azure WireServer is a public literal outside the broader prefixes, so it
// takes precedence over the two IANA allow exceptions below.
// https://learn.microsoft.com/en-us/azure/virtual-network/what-is-ip-address-168-63-129-16
const IPV4_METADATA: Ipv4Addr = Ipv4Addr::new(168, 63, 129, 16);
const IPV4_EXCEPTIONS: [Ipv4Addr; 2] = [Ipv4Addr::new(192, 0, 0, 9), Ipv4Addr::new(192, 0, 0, 10)];
const IPV4_DENIED: [Ipv4Net; 15] = [
    Ipv4Net::new_assert(Ipv4Addr::UNSPECIFIED, 8),
    Ipv4Net::new_assert(Ipv4Addr::new(10, 0, 0, 0), 8),
    Ipv4Net::new_assert(Ipv4Addr::new(100, 64, 0, 0), 10),
    Ipv4Net::new_assert(Ipv4Addr::new(127, 0, 0, 0), 8),
    Ipv4Net::new_assert(Ipv4Addr::new(169, 254, 0, 0), 16),
    Ipv4Net::new_assert(Ipv4Addr::new(172, 16, 0, 0), 12),
    Ipv4Net::new_assert(Ipv4Addr::new(192, 0, 0, 0), 24),
    Ipv4Net::new_assert(Ipv4Addr::new(192, 0, 2, 0), 24),
    Ipv4Net::new_assert(Ipv4Addr::new(192, 88, 99, 0), 24),
    Ipv4Net::new_assert(Ipv4Addr::new(192, 168, 0, 0), 16),
    Ipv4Net::new_assert(Ipv4Addr::new(198, 18, 0, 0), 15),
    Ipv4Net::new_assert(Ipv4Addr::new(198, 51, 100, 0), 24),
    Ipv4Net::new_assert(Ipv4Addr::new(203, 0, 113, 0), 24),
    Ipv4Net::new_assert(Ipv4Addr::new(224, 0, 0, 0), 4),
    Ipv4Net::new_assert(Ipv4Addr::new(240, 0, 0, 0), 4),
];
const WELL_KNOWN_NAT64: Ipv6Net =
    Ipv6Net::new_assert(Ipv6Addr::new(0x0064, 0xff9b, 0, 0, 0, 0, 0, 0), 96);
const IPV6_GLOBAL: Ipv6Net = Ipv6Net::new_assert(Ipv6Addr::new(0x2000, 0, 0, 0, 0, 0, 0, 0), 3);
const IPV6_EXCEPTIONS: [Ipv6Net; 7] = [
    Ipv6Net::new_assert(Ipv6Addr::new(0x2001, 0x0001, 0, 0, 0, 0, 0, 1), 128),
    Ipv6Net::new_assert(Ipv6Addr::new(0x2001, 0x0001, 0, 0, 0, 0, 0, 2), 128),
    Ipv6Net::new_assert(Ipv6Addr::new(0x2001, 0x0001, 0, 0, 0, 0, 0, 3), 128),
    Ipv6Net::new_assert(Ipv6Addr::new(0x2001, 0x0003, 0, 0, 0, 0, 0, 0), 32),
    Ipv6Net::new_assert(Ipv6Addr::new(0x2001, 0x0004, 0x0112, 0, 0, 0, 0, 0), 48),
    Ipv6Net::new_assert(Ipv6Addr::new(0x2001, 0x0020, 0, 0, 0, 0, 0, 0), 28),
    Ipv6Net::new_assert(Ipv6Addr::new(0x2001, 0x0030, 0, 0, 0, 0, 0, 0), 28),
];
const IPV6_DENIED: [Ipv6Net; 4] = [
    Ipv6Net::new_assert(Ipv6Addr::new(0x2001, 0, 0, 0, 0, 0, 0, 0), 23),
    Ipv6Net::new_assert(Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 0), 32),
    Ipv6Net::new_assert(Ipv6Addr::new(0x2002, 0, 0, 0, 0, 0, 0, 0), 16),
    Ipv6Net::new_assert(Ipv6Addr::new(0x3fff, 0, 0, 0, 0, 0, 0, 0), 20),
];

/// Refuses addresses that are not admitted public connection destinations.
///
/// # Errors
///
/// Returns `ResolveError::Denied` for non-public or ambiguous addresses.
pub fn admit_address(address: IpAddr) -> Result<(), ResolveError> {
    if is_public_global(address) {
        Ok(())
    } else {
        Err(ResolveError::Denied)
    }
}

/// Admits a complete DNS answer set for a single connection attempt.
///
/// A mixed answer set is refused rather than filtered so every address reqwest
/// may connect to has passed the same public-address policy.
///
/// # Errors
///
/// Returns [`ResolveError::Denied`] when the answer set is empty or contains a
/// non-public address.
pub fn admit_answers(addresses: Vec<IpAddr>) -> Result<Vec<SocketAddr>, ResolveError> {
    if addresses.is_empty()
        || addresses
            .iter()
            .any(|address| admit_address(*address).is_err())
    {
        return Err(ResolveError::Denied);
    }

    Ok(addresses
        .into_iter()
        .map(|address| SocketAddr::new(address, 0))
        .collect())
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
    address != IPV4_METADATA
        && (IPV4_EXCEPTIONS.contains(&address)
            || !IPV4_DENIED.iter().any(|network| network.contains(&address)))
}

fn is_public_global_v6(address: Ipv6Addr) -> bool {
    if WELL_KNOWN_NAT64.contains(&address) {
        let octets = address.octets();
        return is_public_global_v4(Ipv4Addr::new(
            octets[12], octets[13], octets[14], octets[15],
        ));
    }

    IPV6_GLOBAL.contains(&address)
        && (IPV6_EXCEPTIONS
            .iter()
            .any(|network| network.contains(&address))
            || !IPV6_DENIED.iter().any(|network| network.contains(&address)))
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    use super::{admit_address, admit_answers};

    #[test]
    fn denies_special_and_mapped_addresses() {
        for address in [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(169, 254, 170, 2)),
            IpAddr::V4(Ipv4Addr::new(169, 254, 170, 23)),
            IpAddr::V4(Ipv4Addr::new(168, 63, 129, 16)),
            IpAddr::V4(Ipv4Addr::new(100, 100, 100, 200)),
            IpAddr::V4(Ipv4Addr::new(224, 0, 0, 0)),
            IpAddr::V4(Ipv4Addr::new(240, 0, 0, 0)),
            IpAddr::V4(Ipv4Addr::new(192, 0, 0, 8)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            IpAddr::V6("::2".parse().expect("reserved IPv6 address")),
            IpAddr::V6("::ffff:127.0.0.1".parse().expect("mapped test address")),
            IpAddr::V6(
                "::ffff:168.63.129.16"
                    .parse()
                    .expect("mapped Azure WireServer"),
            ),
            IpAddr::V6("2001:db8::1".parse().expect("documentation test address")),
            IpAddr::V6("2002::1".parse().expect("6to4 test address")),
            IpAddr::V6(
                "2002:808:808::1"
                    .parse()
                    .expect("public-embedded 6to4 address"),
            ),
            IpAddr::V6("fec0::1".parse().expect("reserved test address")),
            IpAddr::V6("fd00:ec2::254".parse().expect("AWS IPv6 metadata address")),
            IpAddr::V6("4000::1".parse().expect("reserved test address")),
            IpAddr::V6(
                "64:ff9b::c0a8:1"
                    .parse()
                    .expect("private NAT64 test address"),
            ),
            IpAddr::V6(
                "64:ff9b::a83f:8110"
                    .parse()
                    .expect("Azure WireServer NAT64 address"),
            ),
        ] {
            assert!(admit_address(address).is_err(), "{address} must be denied");
        }
    }

    #[test]
    fn admits_public_global_addresses_and_registry_exceptions() {
        for address in [
            IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
            IpAddr::V4(Ipv4Addr::new(192, 0, 0, 9)),
            IpAddr::V4(Ipv4Addr::new(192, 0, 0, 10)),
            IpAddr::V4(Ipv4Addr::new(192, 0, 1, 1)),
            IpAddr::V4(Ipv4Addr::new(192, 2, 1, 1)),
            IpAddr::V6("::ffff:8.8.8.8".parse().expect("public mapped address")),
            IpAddr::V6("2001:1::1".parse().expect("PCP exception")),
            IpAddr::V6("2001:1::2".parse().expect("TURN exception")),
            IpAddr::V6("2001:1::3".parse().expect("DNS-SD exception")),
            IpAddr::V6("2001:3::1".parse().expect("AMT exception")),
            IpAddr::V6("2001:4:112::1".parse().expect("AS112 exception")),
            IpAddr::V6("2001:20::1".parse().expect("ORCHIDv2 exception")),
            IpAddr::V6("2001:3f::1".parse().expect("DET exception")),
            IpAddr::V6("2606:4700:4700::1111".parse().expect("public test address")),
            IpAddr::V6(
                "64:ff9b::808:808"
                    .parse()
                    .expect("public NAT64 test address"),
            ),
            IpAddr::V6(
                "64:ff9b::c000:9"
                    .parse()
                    .expect("public exception NAT64 address"),
            ),
        ] {
            assert!(admit_address(address).is_ok(), "{address} must be admitted");
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
            assert!(admit_address(address).is_err(), "{address} must be denied");
        }
    }

    #[test]
    fn admits_only_complete_public_answer_sets() {
        let public_v4 = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
        let public_v6 = IpAddr::V6("2606:4700:4700::1111".parse().expect("public IPv6"));

        assert!(
            admit_answers(Vec::new()).is_err(),
            "empty answers must deny"
        );
        assert!(admit_answers(vec![public_v4, IpAddr::V6(Ipv6Addr::LOCALHOST)]).is_err());
        assert_eq!(
            admit_answers(vec![public_v4, public_v6, public_v4]).expect("public answers admit"),
            vec![
                SocketAddr::from(([8, 8, 8, 8], 0)),
                SocketAddr::new(public_v6, 0),
                SocketAddr::from(([8, 8, 8, 8], 0)),
            ],
            "admitted answers retain order, duplicates, and reqwest-owned ports",
        );
    }
}

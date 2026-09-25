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

    /// `(address, admitted)` pairs taken from the IANA special-purpose
    /// registries and the metadata literals above, not from the policy tables.
    /// Each table entry appears at both ends, with public neighbours where a
    /// changed prefix length would otherwise go unnoticed.
    const REVIEWED_CORPUS: &[(&str, bool)] = &[
        ("0.0.0.0", false),
        ("0.255.255.255", false),
        ("1.1.1.1", true),
        ("8.8.8.8", true),
        ("9.255.255.255", true),
        ("10.0.0.0", false),
        ("10.255.255.255", false),
        ("11.0.0.0", true),
        ("100.63.255.255", true),
        ("100.64.0.0", false),
        ("100.100.100.200", false), // Alibaba Cloud metadata
        ("100.127.255.255", false),
        ("100.128.0.0", true),
        ("126.255.255.255", true),
        ("127.0.0.0", false),
        ("127.0.0.1", false),
        ("127.255.255.255", false),
        ("128.0.0.0", true),
        ("168.63.129.15", true),
        ("168.63.129.16", false), // Azure WireServer
        ("168.63.129.17", true),
        ("169.253.255.255", true),
        ("169.254.0.0", false),
        ("169.254.169.254", false), // AWS, Azure, GCP and OCI metadata
        ("169.254.170.2", false),   // AWS ECS task metadata
        ("169.254.170.23", false),  // AWS EKS Pod Identity
        ("169.254.255.255", false),
        ("169.255.0.0", true),
        ("172.15.255.255", true),
        ("172.16.0.0", false),
        ("172.31.255.255", false),
        ("172.32.0.0", true),
        ("192.0.0.0", false),
        ("192.0.0.8", false),
        ("192.0.0.9", true),
        ("192.0.0.10", true),
        ("192.0.0.11", false),
        ("192.0.0.170", false),
        ("192.0.0.255", false),
        ("192.0.1.1", true),
        ("192.0.2.0", false),
        ("192.0.2.255", false),
        ("192.0.3.0", true),
        ("192.2.1.1", true),
        ("192.31.196.1", true),
        ("192.52.193.1", true),
        ("192.88.98.255", true),
        ("192.88.99.0", false),
        ("192.88.99.255", false),
        ("192.167.255.255", true),
        ("192.168.0.0", false),
        ("192.168.255.255", false),
        ("192.169.0.0", true),
        ("192.175.48.1", true),
        ("198.17.255.255", true),
        ("198.18.0.0", false),
        ("198.19.255.255", false),
        ("198.20.0.0", true),
        ("198.51.100.0", false),
        ("198.51.100.255", false),
        ("198.51.101.0", true),
        ("203.0.112.255", true),
        ("203.0.113.0", false),
        ("203.0.113.255", false),
        ("223.255.255.255", true),
        ("224.0.0.0", false),
        ("239.255.255.255", false),
        ("240.0.0.0", false),
        ("255.255.255.255", false),
        ("::ffff:8.8.8.8", true),
        ("::ffff:127.0.0.1", false),
        ("::ffff:168.63.129.16", false),
        ("::ffff:169.254.169.254", false),
        ("::ffff:172.16.0.1", false),
        ("::a9fe:a9fe", false), // deprecated IPv4-compatible form
        ("64:ff9b::808:808", true),
        ("64:ff9b::c000:9", true),
        ("64:ff9b::a83f:8110", false),
        ("64:ff9b::a9fe:a9fe", false),
        ("64:ff9b::ac10:1", false),
        ("64:ff9b::c0a8:1", false),
        ("64:ff9b::1:808:808", false),
        ("64:ff9b:1::1", false),
        ("::", false),
        ("::1", false),
        ("::2", false),
        ("100::1", false),
        ("100:0:0:1::1", false),
        ("1fff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", false),
        ("2001::", false),
        ("2001:1::", false),
        ("2001:1::1", true),
        ("2001:1::2", true),
        ("2001:1::3", true),
        ("2001:1::4", false),
        ("2001:2::1", false),
        ("2001:3::", true),
        ("2001:3:ffff:ffff:ffff:ffff:ffff:ffff", true),
        ("2001:4::", false),
        ("2001:4:111::1", false),
        ("2001:4:112::", true),
        ("2001:4:112:ffff:ffff:ffff:ffff:ffff", true),
        ("2001:4:113::", false),
        ("2001:10::1", false),
        ("2001:1f:ffff:ffff:ffff:ffff:ffff:ffff", false),
        ("2001:20::", true),
        ("2001:2f:ffff:ffff:ffff:ffff:ffff:ffff", true),
        ("2001:30::", true),
        ("2001:3f:ffff:ffff:ffff:ffff:ffff:ffff", true),
        ("2001:40::", false),
        ("2001:1ff:ffff:ffff:ffff:ffff:ffff:ffff", false),
        ("2001:200::", true),
        ("2001:db7:ffff:ffff:ffff:ffff:ffff:ffff", true),
        ("2001:db8::", false),
        ("2001:db8:ffff:ffff:ffff:ffff:ffff:ffff", false),
        ("2001:db9::", true),
        ("2002::", false),
        ("2002:808:808::1", false),
        ("2002:ffff:ffff:ffff:ffff:ffff:ffff:ffff", false),
        ("2003::", true),
        ("2606:4700:4700::1111", true),
        ("2620:4f:8000::1", true),
        ("3fff::", false),
        ("3fff:fff:ffff:ffff:ffff:ffff:ffff:ffff", false),
        ("3fff:1000::", true),
        ("4000::", false),
        ("5f00::1", false),
        ("fc00::", false),
        ("fd00:ec2::254", false), // AWS IPv6 metadata
        ("fe80::1", false),
        ("fec0::1", false),
        ("ff02::1", false),
    ];

    #[test]
    fn admission_matches_the_reviewed_registry_corpus() {
        let mismatches: Vec<_> = REVIEWED_CORPUS
            .iter()
            .filter(|(address, admitted)| {
                let address: IpAddr = address.parse().expect("corpus address");
                admit_address(address).is_ok() != *admitted
            })
            .collect();
        assert!(
            mismatches.is_empty(),
            "policy disagrees with the reviewed corpus: {mismatches:?}"
        );
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

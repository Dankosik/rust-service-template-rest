use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::ResolveError;

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
    let segments = address.segments();
    if is_well_known_nat64(segments) {
        return is_public_global_v4(Ipv4Addr::from(
            (u32::from(segments[6]) << 16) | u32::from(segments[7]),
        ));
    }
    if segments[0] & 0xe000 != 0x2000 || segments[0] == 0x2002 {
        return false;
    }
    if segments[0] == 0x2001 && segments[1] <= 0x01ff {
        return is_global_2001_exception(segments);
    }
    !matches!(
        segments,
        [0x2001, 0x0db8, _, _, _, _, _, _] | [0x3fff, 0x0000..=0x0fff, _, _, _, _, _, _]
    )
}

fn is_well_known_nat64(segments: [u16; 8]) -> bool {
    matches!(segments, [0x0064, 0xff9b, 0, 0, 0, 0, _, _])
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
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use super::admit_address;

    #[test]
    fn denies_special_and_mapped_addresses() {
        for address in [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(192, 0, 0, 8)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            IpAddr::V6("::2".parse().expect("reserved IPv6 address")),
            IpAddr::V6("::ffff:127.0.0.1".parse().expect("mapped test address")),
            IpAddr::V6("2001:db8::1".parse().expect("documentation test address")),
            IpAddr::V6("2002::1".parse().expect("6to4 test address")),
            IpAddr::V6(
                "2002:808:808::1"
                    .parse()
                    .expect("public-embedded 6to4 address"),
            ),
            IpAddr::V6("fec0::1".parse().expect("reserved test address")),
            IpAddr::V6("4000::1".parse().expect("reserved test address")),
            IpAddr::V6(
                "64:ff9b::c0a8:1"
                    .parse()
                    .expect("private NAT64 test address"),
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
}

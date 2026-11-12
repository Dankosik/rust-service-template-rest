//! JSON Web Algorithms, RFC 7518.

/// RFC 7518 §6.2.1.2 and §6.2.1.3: each P-256 coordinate is exactly 32
/// octets, leading zeros kept.
const P256_COORDINATE_BYTES: usize = 32;

/// The uncompressed SEC 1 point (`0x04 || x || y`, SEC 1 §2.3.3) for the
/// decoded `x` and `y` members of a P-256 JWK; `None` when either coordinate
/// is not exactly 32 octets. Whether the point lies on the curve is left to
/// the caller's key parser.
#[must_use]
pub fn p256_point(x: &[u8], y: &[u8]) -> Option<[u8; 1 + 2 * P256_COORDINATE_BYTES]> {
    let x: &[u8; P256_COORDINATE_BYTES] = x.try_into().ok()?;
    let y: &[u8; P256_COORDINATE_BYTES] = y.try_into().ok()?;
    let mut point = [0; 1 + 2 * P256_COORDINATE_BYTES];
    point[0] = 0x04;
    point[1..=P256_COORDINATE_BYTES].copy_from_slice(x);
    point[1 + P256_COORDINATE_BYTES..].copy_from_slice(y);
    Some(point)
}

#[cfg(test)]
mod tests {
    use super::p256_point;

    #[test]
    fn a_p256_point_is_the_uncompressed_tag_then_both_coordinates() {
        let point = p256_point(&[1; 32], &[2; 32]).expect("32-octet coordinates");
        assert_eq!(point[0], 0x04);
        assert_eq!(point[1..33], [1; 32]);
        assert_eq!(point[33..], [2; 32]);
    }

    #[test]
    fn a_coordinate_of_any_other_length_makes_no_point() {
        // A coordinate with its leading zero dropped is 31 octets.
        assert_eq!(p256_point(&[1; 31], &[2; 32]), None);
        assert_eq!(p256_point(&[1; 32], &[2; 33]), None);
        assert_eq!(p256_point(&[], &[]), None);
    }
}

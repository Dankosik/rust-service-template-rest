/// RFC 7518 §6.2.1.2, §6.2.1.3
const P256_COORDINATE_BYTES: usize = 32;
const UNCOMPRESSED_POINT_TAG: u8 = 0x04;

/// SEC 1 §2.3.3
#[must_use]
pub fn p256_point(x: &[u8], y: &[u8]) -> Option<[u8; 1 + 2 * P256_COORDINATE_BYTES]> {
    let x: &[u8; P256_COORDINATE_BYTES] = x.try_into().ok()?;
    let y: &[u8; P256_COORDINATE_BYTES] = y.try_into().ok()?;
    let mut point = [0; 1 + 2 * P256_COORDINATE_BYTES];
    point[0] = UNCOMPRESSED_POINT_TAG;
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
        assert_eq!(p256_point(&[1; 31], &[2; 32]), None);
        assert_eq!(p256_point(&[1; 32], &[2; 33]), None);
        assert_eq!(p256_point(&[], &[]), None);
    }
}

pub fn access_token(value: &str) -> bool {
    (6..=128).contains(&value.len()) && value.bytes().all(|b| (32..=126).contains(&b))
}
pub fn hotspot(ssid: &str, password: &str) -> bool {
    !ssid.is_empty()
        && ssid.len() <= 32
        && password.len() <= 63
        && !ssid.contains('\0')
        && !password.contains('\0')
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identical_byte_boundaries() {
        for n in [5, 6, 128, 129] {
            assert_eq!(access_token(&"a".repeat(n)), (6..=128).contains(&n))
        }
        assert!(!access_token(&format!("{}\n", "a".repeat(24))));
        assert!(!access_token(&"中".repeat(24)));
        assert!(hotspot(&"中".repeat(10), &"中".repeat(21)));
        assert!(!hotspot(&"中".repeat(11), "password"));
        assert!(!hotspot("ssid", &"中".repeat(22)));
        assert!(!hotspot("bad\0ssid", "password"));
    }
}

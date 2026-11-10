//! Binary plist boundary limits before recursive Value construction.
use crate::{Error, Result};
pub use plist::{Dictionary, Value};
use std::io::Cursor;
pub fn decode(b: &[u8]) -> Result<Value> {
    if b.len() < 40 || b.len() > 262144 || !b.starts_with(b"bplist00") {
        return Err(Error::Malformed);
    }
    let count = u64::from_be_bytes(b[b.len() - 24..b.len() - 16].try_into().unwrap());
    if count > 4096 {
        return Err(Error::Limit);
    }
    // Walk bounded events first: cycles are rejected by plist's binary reader; cap both depth and expanded references.
    let reader = plist::stream::Reader::new(Cursor::new(b));
    let mut depth = 0usize;
    let mut events = 0usize;
    for event in reader {
        events += 1;
        if events > 8192 {
            return Err(Error::Limit);
        }
        match event.map_err(|_| Error::Malformed)? {
            plist::stream::Event::StartArray(_) | plist::stream::Event::StartDictionary(_) => {
                depth += 1;
                if depth > 32 {
                    return Err(Error::Limit);
                }
            }
            plist::stream::Event::EndCollection => {
                depth = depth.checked_sub(1).ok_or(Error::Malformed)?
            }
            _ => {}
        }
    }
    Value::from_reader(Cursor::new(b)).map_err(|_| Error::Malformed)
}
pub fn encode(v: &Value) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    v.to_writer_binary(&mut b).map_err(|_| Error::Malformed)?;
    if b.len() > 262144 {
        return Err(Error::Limit);
    }
    Ok(b)
}
pub fn dict(items: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Dictionary(items.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}
pub fn number(v: u64) -> Value {
    Value::Integer(v.into())
}
/// AirPlay's opaque stream IDs and seeds, not sizes or ports.
/// An 8-byte plist integer with bit 63 set decodes as signed; preserve its full bit pattern.
pub fn stream_identifier(value: &Value) -> Option<u64> {
    value
        .as_unsigned_integer()
        .or_else(|| value.as_signed_integer().map(|n| n as u64))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stream_identifier_preserves_unsigned_bits_from_binary_plist() {
        for (signed, expected) in [
            (-1i64, "18446744073709551615"),
            (i64::MIN, "9223372036854775808"),
            (77, "77"),
        ] {
            let encoded = encode(&dict([
                ("seed", Value::Integer(signed.into())),
                ("streamConnectionID", Value::Integer(signed.into())),
            ]))
            .unwrap();
            let decoded = decode(&encoded).unwrap();
            let fields = decoded.as_dictionary().unwrap();
            for key in ["seed", "streamConnectionID"] {
                assert_eq!(
                    stream_identifier(&fields[key]).unwrap().to_string(),
                    expected
                );
                if signed < 0 {
                    assert!(fields[key].as_unsigned_integer().is_none());
                }
            }
        }
        let large = decode(&encode(&number(u64::MAX)).unwrap()).unwrap();
        assert_eq!(stream_identifier(&large), Some(u64::MAX));
        assert_eq!(stream_identifier(&Value::String("-1".into())), None);
        assert_eq!(stream_identifier(&Value::Real(-1.0)), None);
    }
    #[test]
    fn reject_deep_nested_before_value_recursion() {
        let mut v = number(1);
        for _ in 0..40 {
            v = Value::Array(vec![v])
        }
        assert_eq!(decode(&encode(&v).unwrap()), Err(Error::Limit));
    }
    #[test]
    fn dictionary_and_bad_object_count() {
        let v = dict([("type", number(110))]);
        let mut b = encode(&v).unwrap();
        assert_eq!(decode(&b).unwrap(), v);
        let n = b.len();
        b[n - 24..n - 16].copy_from_slice(&u64::MAX.to_be_bytes());
        assert_eq!(decode(&b), Err(Error::Limit));
    }
}

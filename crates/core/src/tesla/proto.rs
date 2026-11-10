//! Minimal read-only subset of Tesla's protobuf wire format. Unknown fields are skipped.
use super::{Error, Result};
#[derive(Clone, Copy)]
pub enum Value<'a> {
    Int(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
    Fixed64,
}
pub struct Message<'a>(Vec<(u32, Value<'a>)>);
fn varint(input: &mut &[u8]) -> Result<u64> {
    let mut value = 0u64;
    for shift in (0..70).step_by(7) {
        let (&b, rest) = input.split_first().ok_or(Error::Malformed)?;
        *input = rest;
        if shift == 63 && b > 1 {
            return Err(Error::Malformed);
        }
        value |= u64::from(b & 127) << shift;
        if b & 128 == 0 {
            return Ok(value);
        }
    }
    Err(Error::Malformed)
}
fn take<'a>(input: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
    if input.len() < n {
        return Err(Error::Malformed);
    }
    let (head, tail) = input.split_at(n);
    *input = tail;
    Ok(head)
}
impl<'a> Message<'a> {
    pub fn parse(mut input: &'a [u8]) -> Result<Self> {
        if input.len() > 16384 {
            return Err(Error::Malformed);
        }
        let mut fields = Vec::new();
        while !input.is_empty() {
            let tag = varint(&mut input)?;
            let field = u32::try_from(tag >> 3).map_err(|_| Error::Malformed)?;
            if field == 0 || field > 0x1fff_ffff || fields.len() >= 2048 {
                return Err(Error::Malformed);
            }
            let value = match tag & 7 {
                0 => Value::Int(varint(&mut input)?),
                1 => {
                    take(&mut input, 8)?;
                    Value::Fixed64
                }
                2 => {
                    let n = usize::try_from(varint(&mut input)?).map_err(|_| Error::Malformed)?;
                    Value::Bytes(take(&mut input, n)?)
                }
                5 => Value::Fixed32(u32::from_le_bytes(take(&mut input, 4)?.try_into().unwrap())),
                _ => return Err(Error::Malformed),
            };
            fields.push((field, value));
        }
        Ok(Self(fields))
    }
    fn one(&self, field: u32) -> Result<Option<Value<'a>>> {
        let mut values = self.0.iter().filter(|(n, _)| *n == field);
        let value = values.next().map(|(_, v)| *v);
        if values.next().is_some() {
            return Err(Error::Malformed);
        }
        Ok(value)
    }
    pub fn bytes(&self, field: u32) -> Result<Option<&'a [u8]>> {
        match self.one(field)? {
            None => Ok(None),
            Some(Value::Bytes(v)) => Ok(Some(v)),
            _ => Err(Error::Malformed),
        }
    }
    pub fn int(&self, field: u32) -> Result<Option<u64>> {
        match self.one(field)? {
            None => Ok(None),
            Some(Value::Int(v)) => Ok(Some(v)),
            _ => Err(Error::Malformed),
        }
    }
    pub fn fixed(&self, field: u32) -> Result<Option<u32>> {
        match self.one(field)? {
            None => Ok(None),
            Some(Value::Fixed32(v)) => Ok(Some(v)),
            _ => Err(Error::Malformed),
        }
    }
    pub fn nested(&self, field: u32) -> Result<Option<Message<'a>>> {
        self.bytes(field)?.map(Self::parse).transpose()
    }
}
fn put_var(mut v: u64, out: &mut Vec<u8>) {
    while v >= 128 {
        out.push((v as u8) | 128);
        v >>= 7;
    }
    out.push(v as u8);
}
pub fn bytes(field: u32, v: &[u8], out: &mut Vec<u8>) {
    put_var(u64::from(field) * 8 + 2, out);
    put_var(v.len() as u64, out);
    out.extend(v);
}
pub fn int(field: u32, v: u64, out: &mut Vec<u8>) {
    put_var(u64::from(field) * 8, out);
    put_var(v, out);
}
pub fn fixed(field: u32, v: u32, out: &mut Vec<u8>) {
    put_var(u64::from(field) * 8 + 5, out);
    out.extend(v.to_le_bytes());
}
pub fn nested(field: u32, v: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    bytes(field, v, &mut out);
    out
}

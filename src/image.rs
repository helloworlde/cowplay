//! Static ELF packaging boundary; OpenWrt keeps its AArch64-only entry point.
fn u16le(b: &[u8], p: usize) -> crate::RuntimeResult<u16> {
    Ok(u16::from_le_bytes(
        b.get(p..p + 2).ok_or("truncated ELF")?.try_into()?,
    ))
}
fn u32le(b: &[u8], p: usize) -> crate::RuntimeResult<u32> {
    Ok(u32::from_le_bytes(
        b.get(p..p + 4).ok_or("truncated ELF")?.try_into()?,
    ))
}
fn u64le(b: &[u8], p: usize) -> crate::RuntimeResult<u64> {
    Ok(u64::from_le_bytes(
        b.get(p..p + 8).ok_or("truncated ELF")?.try_into()?,
    ))
}
pub fn validate(bytes: &[u8]) -> crate::RuntimeResult<()> {
    validate_arch(bytes, "arm64")
}
pub fn validate_arch(bytes: &[u8], arch: &str) -> crate::RuntimeResult<()> {
    let machine = match arch {
        "arm64" => 183,
        "x86_64" => 62,
        _ => return Err("unsupported ELF architecture".into()),
    };
    if bytes.get(..7) != Some(b"\x7fELF\x02\x01\x01")
        || !matches!(u16le(bytes, 16)?, 2 | 3)
        || u16le(bytes, 18)? != machine
        || u16le(bytes, 52)? != 64
        || u16le(bytes, 54)? != 56
    {
        let label = if arch == "arm64" { "AArch64" } else { "x86_64" };
        return Err(format!("requires ELF64 little-endian {label} executable").into());
    }
    let start = usize::try_from(u64le(bytes, 32)?)?;
    let count = usize::from(u16le(bytes, 56)?);
    if count == 0 || count > 128 {
        return Err("invalid ELF program headers".into());
    }
    for i in 0..count {
        let p = start.checked_add(i * 56).ok_or("ELF overflow")?;
        if p.checked_add(56).is_none_or(|end| end > bytes.len()) {
            return Err("truncated ELF program header".into());
        }
        match u32le(bytes, p)? {
            3 => return Err("dynamic loader is not allowed in this static package".into()),
            2 => {
                let offset = usize::try_from(u64le(bytes, p + 8)?)?;
                let len = usize::try_from(u64le(bytes, p + 32)?)?;
                let data = bytes
                    .get(offset..offset.checked_add(len).ok_or("ELF overflow")?)
                    .ok_or("truncated ELF dynamic section")?;
                for entry in data.chunks_exact(16) {
                    if u64le(entry, 0)? == 1 {
                        return Err("shared-library dependencies are not allowed".into());
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Vec<u8> {
        let mut b = vec![0; 120];
        b[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        b[16..18].copy_from_slice(&2u16.to_le_bytes());
        b[18..20].copy_from_slice(&183u16.to_le_bytes());
        b[32..40].copy_from_slice(&64u64.to_le_bytes());
        b[52..54].copy_from_slice(&64u16.to_le_bytes());
        b[54..56].copy_from_slice(&56u16.to_le_bytes());
        b[56..58].copy_from_slice(&1u16.to_le_bytes());
        b
    }
    #[test]
    fn rejects_host_arch_and_runtime_loader() {
        let mut b = fixture();
        assert!(validate(&b).is_ok());
        b[18..20].copy_from_slice(&62u16.to_le_bytes());
        assert!(validate(&b).is_err());
        assert!(validate_arch(&b, "x86_64").is_ok());
        assert!(validate_arch(&b, "armv7").is_err());
        b[18..20].copy_from_slice(&183u16.to_le_bytes());
        b[64..68].copy_from_slice(&3u32.to_le_bytes());
        assert!(validate(&b).is_err());
        assert!(validate(b"Mach-O").is_err());
    }
}

use std::{io, path::Path};
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    id: Vec<u8>,
    key: [u8; 32],
}
pub fn read(path: &Path) -> io::Result<Option<(Vec<u8>, [u8; 32])>> {
    match std::fs::File::open(path) {
        Ok(file) => {
            use std::io::Read;
            let mut bytes = Vec::new();
            file.take(4097).read_to_end(&mut bytes)?;
            if bytes.len() > 4096 {
                return Err(io::Error::other("pairing size"));
            }
            let r: Record =
                serde_json::from_slice(&bytes).map_err(|_| io::Error::other("pairing format"))?;
            if r.id.is_empty() || r.id.len() > 128 {
                return Err(io::Error::other("pairing identity length"));
            }
            Ok(Some((r.id, r.key)))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
pub fn write(path: &Path, id: &[u8], key: &[u8; 32]) -> io::Result<()> {
    if id.is_empty() || id.len() > 128 {
        return Err(io::Error::other("pairing identity length"));
    }
    let record = serde_json::to_vec(&Record { id: id.to_vec(), key: *key }).map_err(io::Error::other)?;
    crate::settings::write_private(path, &record)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restart_reads_complete_record_and_ignores_interrupted_candidate() {
        let dir = std::env::temp_dir().join(format!("f50-pairing-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("record.json");
        write(&path, b"synthetic-controller", &[7; 32]).unwrap();
        // A temporary left by a power loss during the next write (see `settings::replace`).
        std::fs::write(dir.join(format!(".record.json.{}.0.tmp", std::process::id())), b"truncated").unwrap();
        assert_eq!(
            read(&path).unwrap(),
            Some((b"synthetic-controller".to_vec(), [7; 32]))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}

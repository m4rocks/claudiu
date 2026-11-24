//! Usage limits shared by harnesses, saved as one reading per group of models, plus shared log/time helpers.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::store::UsageLimits;

/// One group of models that share limits, by the name the provider gives it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Pool {
    pub name: String,
    pub usage: UsageLimits,
}

const HEAD_BYTES: u64 = 128 * 1024;
const TAIL_BYTES: u64 = 768 * 1024;

/// Read the head and tail chunks of a potentially large log file.
pub(crate) fn read_chunks(path: &Path) -> std::io::Result<(String, String, bool, i64, i64)> {
    let mut f = File::open(path)?;
    let meta = f.metadata()?;
    let len = meta.len();
    let modified = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
    let created = meta.created().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(modified, |d| d.as_secs() as i64);
    if len <= HEAD_BYTES + TAIL_BYTES {
        let mut buf = Vec::with_capacity(len as usize);
        f.read_to_end(&mut buf)?;
        let s = String::from_utf8_lossy(&buf).into_owned();
        return Ok((s, String::new(), true, created, modified));
    }
    let mut head = vec![0u8; HEAD_BYTES as usize];
    f.read_exact(&mut head)?;
    f.seek(SeekFrom::Start(len - TAIL_BYTES))?;
    let mut tail = vec![0u8; TAIL_BYTES as usize];
    f.read_exact(&mut tail)?;
    // Both chunks start/end mid-line: drop the partial leading line of the tail; head's last line fails to parse and is skipped.
    let tail = String::from_utf8_lossy(&tail).into_owned();
    let tail = tail.split_once('\n').map_or(String::new(), |(_, rest)| rest.to_string());
    Ok((String::from_utf8_lossy(&head).into_owned(), tail, false, created, modified))
}

/// Parse `2026-10-05T14:10:53.395Z` to unix seconds.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok().map(|d| d.unix_timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-10-05T14:10:53.395Z"), Some(1791209453));
        assert_eq!(parse_rfc3339("nope"), None);
    }
}

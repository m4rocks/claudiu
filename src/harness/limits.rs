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

/// Merge `incoming` into `current` per window, never letting an older reading overwrite a newer one.
/// Returns `true` when any window changed.
pub fn merge_usage(current: &mut UsageLimits, incoming: &UsageLimits) -> bool {
    let mut changed = false;
    for (slot, next) in [(&mut current.five_hour, &incoming.five_hour), (&mut current.seven_day, &incoming.seven_day)] {
        if let Some(next) = next
            && slot.as_ref().is_none_or(|cur| next.seen_at >= cur.seen_at && cur != next)
        {
            *slot = Some(next.clone());
            changed = true;
        }
    }
    changed
}

/// Merge incoming pools with previously stored pools so a slower in-flight probe cannot clobber a newer live reading.
pub fn merge_pools(current: &[Pool], incoming: Vec<Pool>) -> Vec<Pool> {
    incoming
        .into_iter()
        .map(|mut pool| {
            if let Some(prev) = current.iter().find(|p| p.name == pool.name) {
                let mut merged = prev.usage.clone();
                merge_usage(&mut merged, &pool.usage);
                pool.usage = merged;
            }
            pool
        })
        .collect()
}

/// Whether any window in `usage` has crossed its `resets_at` boundary since it was observed.
pub fn has_expired_window(usage: &UsageLimits, now: i64) -> bool {
    [usage.five_hour.as_ref(), usage.seven_day.as_ref()]
        .into_iter()
        .flatten()
        .any(|w| w.resets_at.is_some_and(|reset| reset <= now && w.seen_at < reset))
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

    #[test]
    fn merge_usage_keeps_newer_readings_and_detects_expired_windows() {
        use crate::store::RateLimit;
        let mut cur = UsageLimits {
            five_hour: Some(RateLimit { utilization: Some(0.4), resets_at: Some(500), seen_at: 200 }),
            seven_day: None,
        };
        let older = UsageLimits {
            five_hour: Some(RateLimit { utilization: Some(0.1), resets_at: Some(500), seen_at: 100 }),
            seven_day: Some(RateLimit { utilization: Some(0.2), resets_at: Some(900), seen_at: 100 }),
        };
        assert!(merge_usage(&mut cur, &older));
        assert_eq!(cur.five_hour.as_ref().unwrap().utilization, Some(0.4), "older probe must not overwrite newer live reading");
        assert_eq!(cur.seven_day.as_ref().unwrap().utilization, Some(0.2), "missing window is still filled");
        assert!(!has_expired_window(&cur, 499));
        assert!(has_expired_window(&cur, 500));
    }
}

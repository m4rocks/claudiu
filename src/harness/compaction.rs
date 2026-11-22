#[derive(Clone, Debug)]
pub struct Policy {
    pub enabled: bool,
    pub running: bool,
    pub native_controlled: bool,
    attempted: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self { enabled: true, running: false, native_controlled: false, attempted: false }
    }
}

impl Policy {
    pub fn observe(&mut self, used: u64, size: u64) {
        if !self.running && size > 0 && (used as u128) * 100 < (size as u128) * 80 {
            self.attempted = false;
        }
    }

    pub fn take_due(&mut self, context: Option<(u64, u64)>, supported: bool) -> bool {
        let full = context.is_some_and(|(used, size)| size > 0 && (used as u128) * 100 >= (size as u128) * 80);
        if !self.enabled || !supported || self.running || self.attempted || !full {
            return false;
        }
        self.attempted = true;
        self.running = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_at_eighty_percent_and_rearm_only_after_usage_drops() {
        let mut policy = Policy::default();
        assert!(!policy.take_due(Some((799, 1000)), true));
        assert!(policy.take_due(Some((800, 1000)), true));
        assert!(!policy.take_due(Some((900, 1000)), true));
        policy.running = false;
        assert!(!policy.take_due(Some((900, 1000)), true), "stale usage or a failed compaction must not loop");
        policy.observe(200, 1000);
        assert!(policy.take_due(Some((800, 1000)), true));
    }

    #[test]
    fn unsupported_disabled_or_unknown_context_never_compacts() {
        let mut policy = Policy::default();
        assert!(!policy.take_due(None, true));
        assert!(!policy.take_due(Some((100, 0)), true));
        assert!(!policy.take_due(Some((900, 1000)), false));
        policy.enabled = false;
        assert!(!policy.take_due(Some((900, 1000)), true));
        policy.enabled = true;
        assert!(policy.take_due(Some((u64::MAX, u64::MAX)), true));
    }
}

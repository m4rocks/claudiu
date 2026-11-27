pub const THRESHOLD_PERCENT: u64 = 90;
pub const MIN_THRESHOLD_PERCENT: u64 = 50;
pub const MAX_THRESHOLD_PERCENT: u64 = 95;
pub const THRESHOLD_STEP: u64 = 5;

pub fn clamp_threshold(percent: u64) -> u64 {
    percent.clamp(MIN_THRESHOLD_PERCENT, MAX_THRESHOLD_PERCENT)
}

#[derive(Clone, Debug)]
pub struct Policy {
    pub enabled: bool,
    pub threshold_percent: u64,
    pub running: bool,
    pub native_controlled: bool,
    attempted: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self { enabled: true, threshold_percent: THRESHOLD_PERCENT, running: false, native_controlled: false, attempted: false }
    }
}

impl Policy {
    pub fn observe(&mut self, used: u64, size: u64) {
        let threshold = clamp_threshold(self.threshold_percent);
        if !self.running && size > 0 && (used as u128) * 100 < (size as u128) * u128::from(threshold) {
            self.attempted = false;
        }
    }

    /// Manual and automatic attempts share the same rearm rule while reported usage remains high.
    pub fn started(&mut self) {
        self.running = true;
        self.attempted = true;
    }

    pub fn take_due(&mut self, context: Option<(u64, u64)>, supported: bool) -> bool {
        let threshold = clamp_threshold(self.threshold_percent);
        let full = context.is_some_and(|(used, size)| size > 0 && (used as u128) * 100 >= (size as u128) * u128::from(threshold));
        if !self.enabled || !supported || self.running || self.attempted || !full {
            return false;
        }
        self.started();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_at_threshold_and_rearm_only_after_usage_drops() {
        let mut policy = Policy::default();
        assert!(!policy.take_due(Some((899, 1000)), true));
        assert!(policy.take_due(Some((900, 1000)), true));
        assert!(!policy.take_due(Some((900, 1000)), true));
        policy.running = false;
        assert!(!policy.take_due(Some((950, 1000)), true), "stale usage or a failed compaction must not loop");
        policy.observe(200, 1000);
        assert!(policy.take_due(Some((900, 1000)), true));
    }

    #[test]
    fn custom_threshold_triggers_and_rearms_at_configured_percent() {
        let mut policy = Policy { threshold_percent: 75, ..Default::default() };
        assert!(!policy.take_due(Some((749, 1000)), true));
        assert!(policy.take_due(Some((750, 1000)), true));
        policy.running = false;
        policy.observe(750, 1000);
        assert!(!policy.take_due(Some((800, 1000)), true));
        policy.observe(749, 1000);
        assert!(policy.take_due(Some((750, 1000)), true));
        assert_eq!(clamp_threshold(10), MIN_THRESHOLD_PERCENT);
        assert_eq!(clamp_threshold(100), MAX_THRESHOLD_PERCENT);
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

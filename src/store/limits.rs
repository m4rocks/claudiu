use super::*;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RateLimit {
    /// 0.0..=1.0 when known.
    pub utilization: Option<f32>,
    /// Unix seconds.
    pub resets_at: Option<i64>,
    /// When m4code observed this value.
    pub seen_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageLimits {
    pub five_hour: Option<RateLimit>,
    pub seven_day: Option<RateLimit>,
}

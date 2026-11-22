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
pub struct AccountUsage {
    pub five_hour: Option<RateLimit>,
    pub seven_day: Option<RateLimit>,
}

impl Account {
    pub fn is_main(&self) -> bool {
        self.dir.is_none()
    }

    /// Email when known, else which kind of account it is.
    pub fn name(&self) -> String {
        match (&self.email, self.is_main()) {
            (Some(e), _) => e.clone(),
            (None, true) => "Main account".into(),
            (None, false) => "Not signed in".into(),
        }
    }
}

/// Id of the main account: the user's normal Claude Code (`~/.claude`).
pub const MAIN_ACCOUNT: &str = "main";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub id: Id,
    /// Alt account's Claude Code config folder (see `claude/accounts.rs`); `None` is the main account.
    #[serde(default)]
    pub dir: Option<PathBuf>,
    /// From `claude auth status`; `None` until read, or when signed out.
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub usage: AccountUsage,
}


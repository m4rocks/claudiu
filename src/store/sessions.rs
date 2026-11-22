use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionKind {
    Claude,
    Shell,
}

/// Whether Claude Code's own record of the session was found on the last scan.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExternalState {
    /// Created by Claudiu, not yet seen on disk (or never scanned).
    #[default]
    Unknown,
    Present,
    /// Previously seen, absent from the last successful scan. Kept in history, flagged stale.
    Missing,
}

/// A GitHub issue or pull request Claude attached to a session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attached {
    /// `owner/name` of a repository other than the session's project; `None` is the project's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub pull: bool,
    pub number: u32,
    /// Both come from GitHub (`app/attached_sync.rs`), never from the model.
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub state: Option<crate::github::model::State>,
}

impl Attached {
    /// Whether this is that item. GitHub's repository names ignore case.
    pub fn is(&self, repo: Option<&str>, pull: bool, number: u32) -> bool {
        let same_repo = match (self.repo.as_deref(), repo) {
            (None, None) => true,
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            _ => false,
        };
        same_repo && self.pull == pull && self.number == number
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClaudeMeta {
    /// The harness's id for the session; empty until a new session's harness names it.
    pub session_id: String,
    /// The harness the session runs in. Sessions from before there was a choice ran in Claude Code.
    #[serde(default)]
    pub harness: crate::harness::Harness,
    pub model: Option<String>,
    /// The model and effort last picked in this session, as the harness's option values; a resumed session asks for
    /// them again, since harnesses start a loaded session on their own defaults.
    #[serde(default)]
    pub chosen_model: Option<String>,
    #[serde(default)]
    pub chosen_effort: Option<String>,
    pub context_tokens: Option<u64>,
    /// Exact context usage (0..=1) and window size as reported by Claude Code's status line, when seen.
    #[serde(default)]
    pub context_fraction: Option<f32>,
    #[serde(default)]
    pub context_window: Option<u64>,
    /// Prompt-cache lifetime in seconds (300 or 3600), read from the last cache write in the transcript.
    #[serde(default)]
    pub cache_ttl: Option<u64>,
    /// Unix time of the last request that read or wrote the prompt cache (the moment its TTL restarted).
    #[serde(default)]
    pub cache_at: Option<i64>,
    /// The `/config` auto-compact switch, the `advisorModel` setting and the last slash command, as the mod reports them.
    #[serde(default)]
    pub auto_compact: Option<bool>,
    #[serde(default)]
    pub advisor: Option<String>,
    /// Background compaction ("Precompute compaction" in `/config`); `None` while Claude Code does not offer it.
    #[serde(default)]
    pub precompute_compact: Option<bool>,
    /// The name Claude Code held for the session (its last `/rename`) when Claudiu last read the transcript. A different
    /// one later means the user renamed it inside Claude Code.
    #[serde(default)]
    pub claude_title: Option<String>,
    /// Claude in Chrome: the session is (re)started with `--chrome`. Claudiu's own choice, so it survives restarts.
    #[serde(default)]
    pub chrome: bool,
    /// The account this conversation runs under. It starts there again after closing it or restarting Claudiu, until a
    /// hand-off moves it. `None` until Claudiu first starts it.
    #[serde(default)]
    pub account: Option<Id>,
    /// The advisor declared for the current conversation. A model change is saved in `advisor` at once but only
    /// becomes `advisor_active` on `/clear` or `/compact`; turning the advisor off/on changes it immediately.
    #[serde(default)]
    pub advisor_active: Option<String>,
    /// Tokens at which auto-compaction runs (the context bar's full mark); `None` while it is off or unknown.
    #[serde(default)]
    pub compact_at: Option<u64>,
    /// Issues and pull requests Claude attached to this session (the `attach_github` tool), oldest first.
    #[serde(default)]
    pub attached: Vec<Attached>,
    /// Ids of the sessions this one is linked with (the other side lists this one too).
    #[serde(default)]
    pub linked: Vec<Id>,
    pub transcript: Option<PathBuf>,
    #[serde(default)]
    pub external: ExternalState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: Id,
    pub kind: SessionKind,
    /// Title as known from Claude Code (or a default). Never edited by the user.
    pub title: String,
    /// Claudiu-only display label; wins over `title` when set.
    #[serde(default)]
    pub custom_title: Option<String>,
    /// Tab title Claude chose through Claudiu's former `set_tab_title` tool. Nothing sets it any more; a session that
    /// had one keeps showing it until it is renamed. Beats `title`, loses to `custom_title`.
    #[serde(default)]
    pub auto_title: Option<String>,
    pub cwd: PathBuf,
    pub project_id: Option<Id>,
    pub created_at: i64,
    pub last_active: i64,
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// Removed from Claudiu's index. The underlying Claude transcript is untouched.
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub claude: Option<ClaudeMeta>,
}

impl SessionRecord {
    pub fn display_title(&self) -> &str {
        self.chosen_title().unwrap_or(&self.title)
    }

    /// The name given in Claudiu (manual rename, else Claude's own MCP title); what Claude Code is told to use.
    pub fn chosen_title(&self) -> Option<&str> {
        [&self.custom_title, &self.auto_title].into_iter().flatten().map(|t| t.trim()).find(|t| !t.is_empty())
    }

    /// The harness's id for the session, once it has one.
    pub fn claude_id(&self) -> Option<&str> {
        self.claude.as_ref().map(|c| c.session_id.as_str()).filter(|id| !id.is_empty())
    }
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: Id,
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub git: Option<GitInfo>,
    #[serde(default = "yes")]
    pub collapsed: bool,
    pub added_at: i64,
}


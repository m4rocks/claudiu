use super::*;

/// A name the user gave the session inside Claude Code since the last read, to show in Claudiu too. Records what
/// Claude Code holds either way. Only a change counts: Claude Code keeps the old name until a rename made in Claudiu
/// reaches it, and that rename must not be undone meanwhile. So a name is taken only when Claudiu has none of its own,
/// or when Claude Code already held Claudiu's name; any other change is Claude Code catching up with an older rename.
fn adopt_claude_title(rec: &mut SessionRecord, held: Option<&str>) -> Option<String> {
    let held = held.map(str::trim).filter(|t| !t.is_empty())?;
    let ours = rec.custom_title.clone();
    let meta = rec.claude.as_mut()?;
    let before = meta.claude_title.replace(held.to_string());
    let confirmed = match ours {
        None => true,
        // Claude Code holds names trimmed; an exact match would never confirm a name with spaces around it again.
        Some(ours) => before.as_deref() == Some(ours.trim()),
    };
    (before.as_deref() != Some(held) && confirmed).then(|| held.to_string())
}

/// What we learned about one Claude Code session from its on-disk record.
#[derive(Clone, Debug, PartialEq)]
pub struct ExternalSession {
    pub session_id: String,
    pub cwd: Option<PathBuf>,
    pub title: Option<String>,
    /// The name the user gave it in Claude Code (`/rename`), apart from the generated title in `title`.
    pub custom_title: Option<String>,
    pub created_at: i64,
    pub last_active: i64,
    pub model: Option<String>,
    pub context_tokens: Option<u64>,
    pub cache_ttl: Option<u64>,
    pub cache_at: Option<i64>,
    pub transcript: PathBuf,
}

#[derive(Debug, Default, PartialEq)]
pub struct ReconcileStats {
    pub added: usize,
    pub updated: usize,
    pub marked_missing: usize,
}

/// Startup reconciliation (CLAUDE.md "Session import and synchronization"):
/// - external records are matched by Claude session id, so sessions Claudiu launched itself never duplicate;
/// - Claudiu-only metadata (custom title, hidden, project) is preserved;
/// - nothing is ever deleted: absent records are flagged `Missing`, and only when the scan itself succeeded.
pub fn reconcile(data: &mut Data, external: &[ExternalSession], scan_ok: bool) -> ReconcileStats {
    let mut stats = ReconcileStats::default();
    let projects = data.projects.clone();

    for ext in external {
        let existing = data.sessions.iter_mut().find(|s| s.claude_id() == Some(ext.session_id.as_str()));
        match existing {
            Some(rec) => {
                let before = rec.clone();
                if let Some(title) = ext.title.as_ref().filter(|t| !t.trim().is_empty()) {
                    rec.title = title.clone();
                }
                if let Some(name) = adopt_claude_title(rec, ext.custom_title.as_deref()) {
                    rec.custom_title = Some(name);
                }
                rec.last_active = rec.last_active.max(ext.last_active);
                if rec.project_id.is_none()
                    && let Some(cwd) = &ext.cwd {
                        rec.project_id = project_for(&projects, cwd);
                    }
                let meta = rec.claude.get_or_insert_with(|| ClaudeMeta { session_id: ext.session_id.clone(), ..Default::default() });
                meta.external = ExternalState::Present;
                meta.transcript = Some(ext.transcript.clone());
                if ext.model.is_some() {
                    meta.model = ext.model.clone();
                }
                if ext.context_tokens.is_some() {
                    meta.context_tokens = ext.context_tokens;
                }
                if ext.cache_ttl.is_some() {
                    meta.cache_ttl = ext.cache_ttl;
                }
                if ext.cache_at.is_some() {
                    meta.cache_at = ext.cache_at;
                }
                if *rec != before {
                    stats.updated += 1;
                }
            }
            None => {
                let Some(cwd) = ext.cwd.clone() else { continue };
                data.sessions.push(SessionRecord {
                    id: new_id(),
                    kind: SessionKind::Claude,
                    title: ext.title.clone().unwrap_or_else(|| "Untitled session".into()),
                    custom_title: None,
                    auto_title: None,
                    project_id: project_for(&projects, &cwd),
                    cwd,
                    created_at: ext.created_at,
                    last_active: ext.last_active,
                    exit_code: None,
                    hidden: false,
                    claude: Some(ClaudeMeta {
                        session_id: ext.session_id.clone(),
                        model: ext.model.clone(),
                        context_tokens: ext.context_tokens,
                        transcript: Some(ext.transcript.clone()),
                        external: ExternalState::Present,
                        ..Default::default()
                    }),
                });
                stats.added += 1;
            }
        }
    }

    if scan_ok {
        for rec in &mut data.sessions {
            let Some(meta) = rec.claude.as_mut() else { continue };
            if meta.external == ExternalState::Present && !external.iter().any(|e| e.session_id == meta.session_id) {
                meta.external = ExternalState::Missing;
                stats.marked_missing += 1;
            }
        }
    }
    stats
}


//! Extensible per-user-message metadata and actions: sent timestamp and action buttons (starting with
//! "Restore checkpoint & retry with higher thinking").

use std::collections::BTreeMap;
use std::path::PathBuf;

use gpui::{AnyElement, ClickEvent, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div, px};
use gpui_kit::{assets::IconName, base::{AlertDialog, AlertDialogDescription, AlertDialogPopup, AlertDialogTitle}};
use uuid::Uuid;

use super::{Phase, Queued, ThreadView, queue, review};
use crate::git::snapshot;
use crate::harness::thread::Entry;
use crate::panels::scale;
use crate::panels::theme::theme;
use crate::panels::ui::{button as btn, tooltip::tooltip};
use crate::panels::widgets::icon;

pub(super) struct RetryConfirm {
    pub row: usize,
    pub prompt: String,
    pub effort: String,
    pub effort_label: String,
    pub option_id: String,
    pub has_own_checkpoint: bool,
    pub later_checkpoints: usize,
}

/// Synthetic text that m4code attached to a user message (e.g. checkpoint-restore retry context or
/// local file/image attachments), always prepended inside the user bubble as a dimmed quote block
/// so the user can tell it apart from what they typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AttachedSystemNote {
    pub text: String,
}

const RETRY_RESTORED_LATEST_PREFIX: &str =
    "I restored the files to the checkpoint before my previous message. Please try again:";
const RETRY_RESTORED_EARLIER_PREFIX: &str =
    "I restored the files to the checkpoint before an earlier message (undoing later file changes). Please retry that request:";
const RETRY_EARLIER_PREFIX: &str = "Please revisit and retry this earlier request:";

const RETRY_PREFIXES: &[&str] = &[
    RETRY_RESTORED_LATEST_PREFIX,
    RETRY_RESTORED_EARLIER_PREFIX,
    RETRY_EARLIER_PREFIX,
];

/// Split synthetic m4code-attached text (retry prefixes, local file/image attachments) out of a user
/// message so the bubble always prepends those parts as dimmed quote blocks above the user's text.
pub(super) fn split_attached_system_notes(text: &str) -> (Vec<AttachedSystemNote>, &str) {
    let mut notes = Vec::new();
    let mut body = text;

    // Leading retry-with-higher-thinking system prefixes.
    loop {
        let mut matched = false;
        for &prefix in RETRY_PREFIXES {
            if let Some(rest) = body.strip_prefix(prefix).and_then(|s| s.strip_prefix("\n\n")) {
                notes.push(AttachedSystemNote { text: prefix.to_string() });
                body = rest;
                matched = true;
                break;
            }
        }
        if !matched {
            break;
        }
    }

    let header = crate::panels::attachments::ATTACHED_FILES_HEADER;
    // Leading attachments block (attachments-only or `/slash` command after attachments).
    if let Some(after_header) = body.strip_prefix(header) {
        let mut end = header.len();
        for line in after_header.split_inclusive('\n') {
            if line.starts_with("- ") {
                end += line.len();
            } else {
                break;
            }
        }
        if end > header.len() {
            let block = body[..end].trim_end();
            notes.push(AttachedSystemNote { text: block.to_string() });
            body = body[end..].trim_start_matches('\n');
        }
    } else if let Some(idx) = body.rfind(&format!("\n\n{header}")) {
        // Trailing attachments block (ordinary user text followed by attached local files) —
        // still returned in `notes` so it renders prepended above the user's text.
        let block_start = idx + 2;
        let after_header = &body[block_start + header.len()..];
        let all_list_lines = !after_header.trim().is_empty()
            && after_header.lines().all(|line| line.starts_with("- ") || line.is_empty());
        if all_list_lines {
            let block = body[block_start..].trim_end();
            notes.push(AttachedSystemNote { text: block.to_string() });
            body = &body[..idx];
        }
    }

    (notes, body)
}

/// Strip any leading retry system prefix so retrying an already-retried message doesn't stack prefixes.
fn strip_retry_prefixes(mut text: &str) -> &str {
    loop {
        let mut matched = false;
        for &prefix in RETRY_PREFIXES {
            if let Some(rest) = text.strip_prefix(prefix).and_then(|s| s.strip_prefix("\n\n")) {
                text = rest;
                matched = true;
                break;
            }
        }
        if !matched {
            return text;
        }
    }
}

pub(super) fn render_attached_system_note(note: &AttachedSystemNote) -> AnyElement {
    div()
        .w_full()
        .pl(px(10.0))
        .py(px(2.0))
        .border_l_2()
        .border_color(theme().muted_foreground)
        .text_size(scale::px(12.0))
        .text_color(theme().muted_foreground)
        .child(note.text.clone())
        .into_any_element()
}

/// Optional notice shown above a user message on the right side (icon + text).
/// Add new variants here as other per-message notices are introduced.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum UserNotice {
    ResentHigherThinking { effort: String },
}

impl UserNotice {
    pub(super) fn icon(&self) -> IconName {
        match self {
            Self::ResentHigherThinking { .. } => IconName::Brain,
        }
    }

    pub(super) fn text(&self) -> String {
        match self {
            Self::ResentHigherThinking { effort } => format!("Resent with {effort} thinking"),
        }
    }
}

/// A retry waiting for the live `thought_level` option change (and permission preservation) to be confirmed before dispatching.
pub(super) struct PendingRetry {
    pub option_id: String,
    pub expected_effort: String,
    pub effort_label: String,
    pub prompt: String,
    pub expected_permission: Option<crate::harness::permission::Permission>,
    pub expected_mode: Option<String>,
    pub permission_restored: bool,
}

type ActionHandler = Box<dyn Fn(&mut ThreadView, &mut Window, &mut Context<ThreadView>) + 'static>;

/// One icon button in a user message's action bar. New actions only need an entry in `user_actions_for`.
/// `on_click: None` renders the button disabled while still showing its hover tooltip.
pub(super) struct UserAction {
    pub id: SharedString,
    pub icon: IconName,
    pub tooltip: SharedString,
    pub on_click: Option<ActionHandler>,
}

/// Whether `entry` is a visible message typed by the user (not a hidden prompt or subagent report).
pub(super) fn is_visible_user_entry(entry: &Entry) -> bool {
    matches!(
        entry,
        Entry::User(text)
            if !crate::harness::thread::is_hidden_prompt(text)
                && crate::harness::thread::parse_subagent_report(text).is_none()
    )
}

/// 0-based ordinal among visible user messages up to and including `row`. Stable across local notices and replays.
pub(super) fn visible_user_ordinal(entries: &[Entry], row: usize) -> Option<usize> {
    let entry = entries.get(row)?;
    if !is_visible_user_entry(entry) {
        return None;
    }
    Some(entries[..row].iter().filter(|e| is_visible_user_entry(e)).count())
}

fn timestamps_path(record: &str) -> PathBuf {
    crate::datadir::data_dir().join("user-timestamps").join(format!("{}.json", crate::datadir::safe_file_stem(record)))
}

pub(super) fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

pub(super) fn load_timestamps(record: &str) -> BTreeMap<usize, i64> {
    if record.is_empty() {
        return BTreeMap::new();
    }
    std::fs::read(timestamps_path(record))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(super) fn save_timestamps(record: &str, timestamps: &BTreeMap<usize, i64>) {
    if record.is_empty() {
        return;
    }
    if timestamps.is_empty() {
        clear_timestamps(record);
        return;
    }
    if let Ok(json) = serde_json::to_string(timestamps) {
        let _ = crate::datadir::write_atomic(&timestamps_path(record), &json);
    }
}

pub(crate) fn clear_timestamps(record: &str) {
    if !record.is_empty() {
        let _ = std::fs::remove_file(timestamps_path(record));
    }
}

fn notices_path(record: &str) -> PathBuf {
    crate::datadir::data_dir().join("user-notices").join(format!("{}.json", crate::datadir::safe_file_stem(record)))
}

pub(super) fn load_notices(record: &str) -> BTreeMap<usize, UserNotice> {
    if record.is_empty() {
        return BTreeMap::new();
    }
    std::fs::read(notices_path(record))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(super) fn save_notices(record: &str, notices: &BTreeMap<usize, UserNotice>) {
    if record.is_empty() {
        return;
    }
    if notices.is_empty() {
        clear_notices(record);
        return;
    }
    if let Ok(json) = serde_json::to_string(notices) {
        let _ = crate::datadir::write_atomic(&notices_path(record), &json);
    }
}

pub(crate) fn clear_notices(record: &str) {
    if !record.is_empty() {
        let _ = std::fs::remove_file(notices_path(record));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CalendarParts {
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
}

fn local_parts(ts: i64) -> Option<CalendarParts> {
    #[cfg(unix)]
    {
        let t = ts as libc::time_t;
        let mut tm = std::mem::MaybeUninit::<libc::tm>::zeroed();
        // SAFETY: `localtime_r` writes into our stack-allocated `tm` and is thread-safe.
        let ptr = unsafe { libc::localtime_r(&t, tm.as_mut_ptr()) };
        if !ptr.is_null() {
            let tm = unsafe { tm.assume_init() };
            return Some(CalendarParts {
                year: tm.tm_year + 1900,
                month: (tm.tm_mon + 1) as u8,
                day: tm.tm_mday as u8,
                hour: tm.tm_hour as u8,
                minute: tm.tm_min as u8,
            });
        }
    }
    let dt = time::OffsetDateTime::from_unix_timestamp(ts).ok()?;
    Some(CalendarParts {
        year: dt.year(),
        month: u8::from(dt.month()),
        day: dt.day(),
        hour: dt.hour(),
        minute: dt.minute(),
    })
}

fn format_clock(hour: u8, minute: u8) -> String {
    let suffix = if hour < 12 { "AM" } else { "PM" };
    let h12 = match hour % 12 {
        0 => 12,
        h => h,
    };
    format!("{h12}:{minute:02} {suffix}")
}

fn month_short(month: u8) -> &'static str {
    match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "",
    }
}

/// Short time shown under a user message, e.g. `"1:07 AM"`.
pub(super) fn format_sent_time(ts: i64) -> Option<String> {
    let p = local_parts(ts)?;
    Some(format_clock(p.hour, p.minute))
}

/// Full date and time shown in the timestamp's hover tooltip, e.g. `"Nov 27, 2026 at 1:07 AM"`.
pub(super) fn format_sent_full(ts: i64) -> Option<String> {
    let p = local_parts(ts)?;
    Some(format!("{} {}, {} at {}", month_short(p.month), p.day, p.year, format_clock(p.hour, p.minute)))
}

enum NextThinking {
    /// Current model offers no live `thought_level` option.
    Unsupported,
    /// Current model's live `thought_level` is already at its highest choice.
    AtMax,
    /// `(option_id, next_value, display_label)`.
    Higher(String, String, String),
}

/// Restore `turns` (`(id, cwd, before, patch_path)`) from newest to oldest. If any step fails partway through,
/// re-apply every already-reversed patch forward so the working tree is never left half rolled back.
fn restore_checkpoints_atomic(turns: Vec<(Uuid, PathBuf, String, PathBuf)>) -> Result<Vec<Uuid>, String> {
    let mut restored_ids = Vec::with_capacity(turns.len());
    let mut applied: Vec<(PathBuf, String)> = Vec::with_capacity(turns.len());
    for (id, cwd, before, path) in turns {
        let patch = match std::fs::read_to_string(&path) {
            Ok(p) => p,
            Err(error) => {
                rollback_applied(&applied);
                return Err(error.to_string());
            }
        };
        if let Err(error) = snapshot::restore(&cwd, &before, &patch) {
            rollback_applied(&applied);
            return Err(error);
        }
        applied.push((cwd, patch));
        restored_ids.push(id);
    }
    Ok(restored_ids)
}

fn rollback_applied(applied: &[(PathBuf, String)]) {
    for (cwd, patch) in applied.iter().rev() {
        let _ = snapshot::apply_forward(cwd, patch);
    }
}

impl ThreadView {
    pub(super) fn record_user_timestamp(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(ordinal) = visible_user_ordinal(&self.thread.entries, row) else { return };
        self.user_timestamps.insert(ordinal, now_unix_secs());
        if self.timestamps_loaded {
            let record = self.record_id.clone();
            let snapshot = self.user_timestamps.clone();
            cx.background_executor().spawn(async move {
                save_timestamps(&record, &snapshot);
            }).detach();
        }
    }

    pub(super) fn record_user_notice(&mut self, row: usize, notice: UserNotice, cx: &mut Context<Self>) {
        let Some(ordinal) = visible_user_ordinal(&self.thread.entries, row) else { return };
        self.user_notices.insert(ordinal, notice);
        if self.notices_loaded {
            let record = self.record_id.clone();
            let snapshot = self.user_notices.clone();
            cx.background_executor().spawn(async move {
                save_notices(&record, &snapshot);
            }).detach();
        }
    }

    /// The next higher thinking level offered by the live `thought_level` option of the current session.
    fn next_higher_effort(&self) -> NextThinking {
        let Some(opt) = self.thread.option("thought_level") else {
            return NextThinking::Unsupported;
        };
        let non_default: Vec<&(String, String)> = opt
            .choices
            .iter()
            .filter(|(v, _)| !v.eq_ignore_ascii_case("default"))
            .collect();
        if non_default.is_empty() {
            return NextThinking::Unsupported;
        }
        let cur = non_default
            .iter()
            .position(|(v, _)| v == self.option_value(opt))
            .or_else(|| non_default.iter().position(|(v, _)| v.eq_ignore_ascii_case("medium")))
            .unwrap_or(1.min(non_default.len() - 1));
        let Some((value, label)) = non_default.get(cur + 1) else {
            return NextThinking::AtMax;
        };
        NextThinking::Higher(opt.id.clone(), value.clone(), label.clone())
    }

    fn latest_visible_user_row(&self) -> Option<usize> {
        self.thread.entries.iter().enumerate().rev().find_map(|(ix, entry)| is_visible_user_entry(entry).then_some(ix))
    }

    /// Whether `row` is the user message of the currently running turn.
    fn is_active_user_row(&self, row: usize) -> bool {
        if !self.working && self.preparing.is_none() {
            return false;
        }
        if self.active_turn_review.as_ref().is_some_and(|a| a.row == row) {
            return true;
        }
        self.latest_visible_user_row() == Some(row)
    }

    /// Unified gate matching `send_next` plus retry/capture/handoff guards.
    fn retry_block_reason(&self) -> Option<&'static str> {
        if self.retrying_turn || self.pending_retry.is_some() {
            Some("Restoring checkpoint and retrying…")
        } else if self.working || self.preparing.is_some() || !self.steering.is_empty() || self.has_background_work() || self.capturing_reviews > 0 {
            Some("Wait for the current turn to finish")
        } else if self.restoring_checkpoint() {
            Some("Wait for checkpoint restore to finish")
        } else if self.phase != Phase::Ready {
            Some("Session is not ready")
        } else if self.pending_model.is_some() {
            Some("Clear the pending model switch before retrying")
        } else if self.authentication_required
            || self.workspace_switching
            || self.permission_changing
            || self.read_only_unconfirmed
            || self.queue_held
            || self.is_waiting()
            || !self.options_in_flight.is_empty()
        {
            Some("Finish the pending session action before retrying")
        } else {
            None
        }
    }

    /// Checkpoints from `row` onward in reverse `row` order (newest-first) for atomic unwinding.
    fn retry_checkpoints_for(&self, row: usize) -> Vec<&review::TurnReview> {
        let mut turns: Vec<&review::TurnReview> = self
            .turn_reviews
            .iter()
            .filter(|turn| turn.row >= row && turn.checkpoint && turn.cwd == self.cwd)
            .collect();
        turns.sort_by_key(|turn| turn.row);
        turns.reverse();
        turns
    }

    /// Actions shown in the footer bar of the user message at `row`.
    pub(super) fn user_actions_for(&self, row: usize) -> Vec<UserAction> {
        let mut actions = Vec::new();
        let next = self.next_higher_effort();
        let (tooltip, enabled) = match (&next, self.retry_block_reason()) {
            (NextThinking::Unsupported, _) if self.phase != Phase::Ready || self.workspace_switching => {
                (self.retry_block_reason().unwrap_or("Session is not ready").to_string(), false)
            }
            (NextThinking::Unsupported, _) => return actions,
            (NextThinking::AtMax, _) => ("Already at max thinking".to_string(), false),
            (_, _) if self.is_active_user_row(row) => ("Wait for the current turn to finish".to_string(), false),
            (NextThinking::Higher(_, _, _), Some(reason)) => (reason.to_string(), false),
            (NextThinking::Higher(_, _, label), None) => {
                let checkpoints = self.retry_checkpoints_for(row);
                let has_own = checkpoints.iter().any(|t| t.row == row);
                let tip = if has_own {
                    format!("Restore checkpoint & retry with {label} thinking")
                } else if !checkpoints.is_empty() {
                    format!("Restore later checkpoints & retry with {label} thinking")
                } else {
                    format!("Retry with {label} thinking")
                };
                (tip, true)
            }
        };
        actions.push(UserAction {
            id: SharedString::from(format!("user-retry-higher-{row}")),
            icon: IconName::Brain,
            tooltip: SharedString::from(tooltip),
            on_click: enabled.then(|| {
                Box::new(move |v: &mut ThreadView, window: &mut Window, cx: &mut Context<ThreadView>| {
                    v.request_retry_turn(row, window, cx);
                }) as ActionHandler
            }),
        });
        actions
    }

    pub(super) fn request_retry_turn(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.retry_block_reason().is_some() {
            return;
        }
        let Some(Entry::User(prompt)) = self.thread.entries.get(row) else { return };
        let NextThinking::Higher(option_id, effort, effort_label) = self.next_higher_effort() else { return };
        let prompt = strip_retry_prefixes(prompt).to_string();
        let checkpoints = self.retry_checkpoints_for(row);
        if !checkpoints.is_empty() {
            let has_own_checkpoint = checkpoints.iter().any(|t| t.row == row);
            let later_checkpoints = checkpoints.iter().filter(|t| t.row > row).count();
            self.retry_confirm = Some(RetryConfirm {
                row,
                prompt,
                effort,
                effort_label,
                option_id,
                has_own_checkpoint,
                later_checkpoints,
            });
            window.focus(&self.restore_focus, cx);
            cx.notify();
        } else {
            self.begin_live_retry(row, prompt, option_id, effort, effort_label, Vec::new(), cx);
        }
    }

    pub(super) fn cancel_retry_turn(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.retry_confirm.take().is_some() {
            self.focus_composer(window, cx);
            cx.notify();
        }
    }

    pub(super) fn confirm_retry_turn(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.retry_confirm.take() else { return };
        self.focus_composer(window, cx);
        if let Some(reason) = self.retry_block_reason() {
            let at = self.thread.notice(format!("Could not retry: {reason}."), true);
            self.remeasure(at);
            cx.notify();
            return;
        }
        let turns: Vec<(Uuid, PathBuf, String, PathBuf)> = self
            .retry_checkpoints_for(pending.row)
            .into_iter()
            .map(|turn| (turn.id, turn.cwd.clone(), turn.before.clone(), review::patch_path(&self.record_id, turn.id)))
            .collect();
        if turns.is_empty() {
            self.begin_live_retry(pending.row, pending.prompt, pending.option_id, pending.effort, pending.effort_label, Vec::new(), cx);
            cx.notify();
            return;
        }
        self.retrying_turn = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { restore_checkpoints_atomic(turns) }).await;
            let _ = this.update(cx, |v, cx| {
                v.retrying_turn = false;
                match result {
                    Ok(restored_ids) => {
                        v.begin_live_retry(pending.row, pending.prompt, pending.option_id, pending.effort, pending.effort_label, restored_ids, cx);
                    }
                    Err(error) => {
                        let at = v.thread.notice(format!("Could not restore checkpoint to retry: {error}"), true);
                        v.remeasure(at);
                        v.send_next(cx);
                    }
                }
                cx.notify();
            });
        }).detach();
    }

    /// Keep the live session (preserving provider prompt caching and permission mode), bump `thought_level` in place,
    /// and wait for the harness to confirm the higher level before dispatching the retry at the front of the queue.
    #[allow(clippy::too_many_arguments)]
    fn begin_live_retry(
        &mut self,
        row: usize,
        prompt: String,
        option_id: String,
        effort: String,
        effort_label: String,
        restored_ids: Vec<Uuid>,
        cx: &mut Context<Self>,
    ) {
        let restored_files = !restored_ids.is_empty();
        if restored_files {
            let count = restored_ids.len();
            self.turn_reviews.retain(|turn| !restored_ids.contains(&turn.id));
            review::prune_turn_reviews_by_ids(&self.record_id, &restored_ids, cx.background_executor());
            let notice = if count == 1 {
                "Restored files to checkpoint".to_string()
            } else {
                format!("Restored files across {count} checkpoints")
            };
            let at = self.thread.notice(notice, false);
            self.remeasure(at);
            self.persist_entries();
        }
        let is_latest = self.latest_visible_user_row() == Some(row);
        let outgoing = if restored_files && is_latest {
            format!("{RETRY_RESTORED_LATEST_PREFIX}\n\n{prompt}")
        } else if restored_files {
            format!("{RETRY_RESTORED_EARLIER_PREFIX}\n\n{prompt}")
        } else if is_latest {
            prompt
        } else {
            format!("{RETRY_EARLIER_PREFIX}\n\n{prompt}")
        };
        let expected_permission = self.handoff_permission();
        let expected_mode = self.thread.mode.clone();
        self.pending_effort = Some(effort.clone());
        self.pending_retry = Some(PendingRetry {
            option_id: option_id.clone(),
            expected_effort: effort.clone(),
            effort_label,
            prompt: outgoing,
            expected_permission,
            expected_mode,
            permission_restored: false,
        });
        if self.connection.is_some() && self.session_id.is_some() {
            self.set_option(option_id, effort, cx);
        } else {
            // No live ACP connection (e.g. offline unit tests): enqueue at the front immediately.
            self.dispatch_pending_retry(cx);
        }
    }

    /// Called once all in-flight session options (or permission restoration) settle. Verifies that the requested higher
    /// thinking level took effect and that the session's permission mode was preserved before sending the retry.
    pub(super) fn complete_pending_retry(&mut self, cx: &mut Context<Self>) {
        let Some(retry) = self.pending_retry.as_ref() else { return };
        let applied = self
            .thread
            .config
            .iter()
            .find(|o| o.id == retry.option_id)
            .is_some_and(|o| o.current == retry.expected_effort);
        if !applied {
            self.cancel_pending_retry("Could not switch to higher thinking; retry cancelled.", cx);
            return;
        }
        let expected_permission = retry.expected_permission;
        let expected_mode = retry.expected_mode.clone();
        let permission_restored = retry.permission_restored;
        let mode_drifted = expected_permission.is_some_and(|wanted| {
            self.handoff_permission() != Some(wanted)
                || (expected_mode.is_some() && self.thread.mode != expected_mode)
        });
        if mode_drifted {
            let Some(wanted) = expected_permission else { return };
            if permission_restored || self.connection.is_none() || self.session_id.is_none() {
                self.cancel_pending_retry("Could not preserve permission mode; retry cancelled.", cx);
                return;
            }
            if let Some(pending) = self.pending_retry.as_mut() {
                pending.permission_restored = true;
            }
            self.set_permission(wanted, cx);
            if self.permission_changing {
                return;
            }
            if self.handoff_permission() != Some(wanted) {
                self.cancel_pending_retry("Could not preserve permission mode; retry cancelled.", cx);
                return;
            }
        }
        self.dispatch_pending_retry(cx);
    }

    pub(super) fn cancel_pending_retry(&mut self, message: &str, cx: &mut Context<Self>) {
        if self.pending_retry.take().is_none() {
            return;
        }
        let at = self.thread.notice(message, true);
        self.remeasure(at);
        if !self.queue.is_empty() {
            self.send_next(cx);
        }
    }

    fn dispatch_pending_retry(&mut self, cx: &mut Context<Self>) {
        let Some(retry) = self.pending_retry.take() else { return };
        let mut item = Queued::new(retry.prompt, true);
        item.ahead = true;
        item.order = queue::next_order();
        item.notice = Some(UserNotice::ResentHigherThinking { effort: retry.effort_label });
        self.queue.push_front(item);
        self.queue_changed();
        self.send_next(cx);
    }

    pub(super) fn render_user_header(&self, row: usize) -> Option<AnyElement> {
        let ordinal = visible_user_ordinal(&self.thread.entries, row)?;
        let notice = self.user_notices.get(&ordinal)?;
        Some(
            div()
                .w_full()
                .px(px(4.0))
                .flex()
                .items_center()
                .justify_end()
                .gap(px(4.0))
                .text_size(scale::px(11.0))
                .text_color(theme().subtle_foreground)
                .child(icon(notice.icon(), 11.5))
                .child(notice.text())
                .into_any_element(),
        )
    }

    pub(super) fn render_user_footer(&self, row: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ordinal = visible_user_ordinal(&self.thread.entries, row)?;
        let sent_ts = self.user_timestamps.get(&ordinal).copied();
        let actions = self.user_actions_for(row);
        if sent_ts.is_none() && actions.is_empty() {
            return None;
        }
        let has_actions = !actions.is_empty();
        let timestamp = sent_ts.and_then(|ts| {
            let short = format_sent_time(ts)?;
            let full = format_sent_full(ts).unwrap_or_else(|| short.clone());
            Some(
                div()
                    .id(SharedString::from(format!("user-sent-at-{row}")))
                    .text_size(scale::px(11.0))
                    .text_color(theme().subtle_foreground)
                    .tooltip(tooltip(full))
                    .child(short),
            )
        });
        let buttons = actions.into_iter().map(|action| {
            let id = action.id;
            let tip = action.tooltip;
            if let Some(handler) = action.on_click {
                div()
                    .id(id)
                    .size(px(20.0))
                    .rounded(px(5.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(theme().subtle_foreground)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme().secondary_hover).text_color(theme().foreground))
                    .tooltip(tooltip(tip))
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        handler(v, window, cx);
                    }))
                    .child(icon(action.icon, 12.5))
                    .into_any_element()
            } else {
                div()
                    .id(id)
                    .size(px(20.0))
                    .rounded(px(5.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(theme().subtle_foreground)
                    .opacity(0.5)
                    .cursor_default()
                    .tooltip(tooltip(tip))
                    .child(icon(action.icon, 12.5))
                    .into_any_element()
            }
        });
        Some(
            div()
                .w_full()
                .px(px(4.0))
                .flex()
                .items_center()
                .justify_end()
                .gap(px(8.0))
                .children(has_actions.then(|| div().flex().items_center().gap(px(4.0)).children(buttons)))
                .children(timestamp)
                .into_any_element(),
        )
    }

    pub(super) fn render_retry_confirm(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pending = self.retry_confirm.as_ref()?;
        let clear = cx.entity().downgrade();
        let later = pending.later_checkpoints;
        let plural = if later == 1 { "turn" } else { "turns" };
        let restores_files = pending.has_own_checkpoint || later > 0;
        let title = if restores_files { "Restore checkpoint & retry?" } else { "Retry with higher thinking?" };
        let detail = match (pending.has_own_checkpoint, later) {
            (true, 0) => format!(
                "This restores files changed in this turn back to before your message, then retries with {} thinking. Restore is blocked if those files changed since.",
                pending.effort_label,
            ),
            (true, n) => format!(
                "This restores files changed in this turn and {n} later {plural} back to before your message, then retries with {} thinking. Restore is blocked if those files changed since.",
                pending.effort_label,
            ),
            (false, 0) => format!(
                "This switches thinking to {} and resends your message. No file checkpoints will be reverted.",
                pending.effort_label,
            ),
            (false, n) => format!(
                "This restores files changed in {n} later {plural} back to before your message, then retries with {} thinking. Restore is blocked if those files changed since.",
                pending.effort_label,
            ),
        };
        let confirm_label = if restores_files {
            format!("Restore & retry ({})", pending.effort_label)
        } else {
            format!("Retry ({})", pending.effort_label)
        };
        let (confirm_bg, confirm_hover) = if restores_files {
            (theme().destructive, theme().destructive.opacity(0.8))
        } else {
            (theme().primary, theme().primary_hover)
        };
        Some(
            AlertDialog::new(cx)
                .focus_handle(self.restore_focus.clone())
                .on_open_change(move |open, _, window, cx| {
                    if !open {
                        let _ = clear.update(cx, |v, cx| v.cancel_retry_turn(window, cx));
                    }
                })
                .backdrop(div().absolute().inset_0().bg(theme().backdrop))
                .popup(
                    AlertDialogPopup::new()
                        .w(px(440.0))
                        .max_w_full()
                        .p(px(20.0))
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .rounded(px(10.0))
                        .bg(theme().background)
                        .border_1()
                        .border_color(theme().panel_border)
                        .child(
                            AlertDialogTitle::new()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_size(scale::px(16.0))
                                .child(title),
                        )
                        .child(AlertDialogDescription::new().text_color(theme().muted_foreground).child(detail))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.0))
                                .pt(px(6.0))
                                .child(
                                    btn::text("retry-cancel", "Cancel", false)
                                        .debug_selector(|| "retry-cancel".into())
                                        .on_click(cx.listener(|v, _: &ClickEvent, window, cx| v.cancel_retry_turn(window, cx))),
                                )
                                .child(
                                    btn::colored(
                                        "retry-confirm",
                                        None,
                                        confirm_label,
                                        confirm_bg,
                                        theme().background,
                                        confirm_hover,
                                        None,
                                        true,
                                    )
                                    .debug_selector(|| "retry-confirm".into())
                                    .on_click(cx.listener(|v, _: &ClickEvent, window, cx| v.confirm_retry_turn(window, cx))),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_formatting_handles_midnight_noon_and_padding() {
        assert_eq!(format_clock(0, 5), "12:05 AM");
        assert_eq!(format_clock(1, 7), "1:07 AM");
        assert_eq!(format_clock(12, 0), "12:00 PM");
        assert_eq!(format_clock(13, 45), "1:45 PM");
        assert_eq!(format_clock(23, 59), "11:59 PM");
    }

    #[test]
    fn timestamps_persist_and_clear() {
        let _dirs = crate::datadir::use_throwaway_folders();
        let mut map = BTreeMap::new();
        map.insert(0, 1_700_000_000);
        map.insert(1, 1_700_000_100);
        save_timestamps("user-ts-test", &map);
        assert_eq!(load_timestamps("user-ts-test"), map);

        clear_timestamps("user-ts-test");
        assert!(load_timestamps("user-ts-test").is_empty());
    }

    #[test]
    fn notices_persist_and_clear() {
        let _dirs = crate::datadir::use_throwaway_folders();
        let mut map = BTreeMap::new();
        map.insert(1, UserNotice::ResentHigherThinking { effort: "High".into() });
        save_notices("user-notice-test", &map);
        assert_eq!(load_notices("user-notice-test"), map);

        clear_notices("user-notice-test");
        assert!(load_notices("user-notice-test").is_empty());
    }

    #[test]
    fn split_attached_system_notes_extracts_retry_and_attachments() {
        // Plain user text has no attached notes.
        assert_eq!(split_attached_system_notes("Fix the bug"), (vec![], "Fix the bug"));

        // Retry prefix + user text + trailing local attachments -> both prepended above user text.
        let combined = format!(
            "{RETRY_RESTORED_LATEST_PREFIX}\n\nCheck this screenshot\n\n{}- \"/tmp/a.png\"\n- \"/tmp/b.png\"\n",
            crate::panels::attachments::ATTACHED_FILES_HEADER
        );
        let (notes, body) = split_attached_system_notes(&combined);
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].text, RETRY_RESTORED_LATEST_PREFIX);
        assert!(notes[1].text.starts_with("Attached local files"));
        assert!(notes[1].text.contains("/tmp/b.png"));
        assert_eq!(body, "Check this screenshot");

        // Attachments-only message leaves body empty.
        let only_attachments = format!("{}- \"/tmp/a.png\"\n", crate::panels::attachments::ATTACHED_FILES_HEADER);
        let (notes, body) = split_attached_system_notes(&only_attachments);
        assert_eq!(notes.len(), 1);
        assert_eq!(body, "");

        // Leading attachments before a slash command.
        let slash_with_attachments = format!("{}- \"/tmp/a.png\"\n\n/review", crate::panels::attachments::ATTACHED_FILES_HEADER);
        let (notes, body) = split_attached_system_notes(&slash_with_attachments);
        assert_eq!(notes.len(), 1);
        assert_eq!(body, "/review");

        // Stripping retry prefixes avoids stacking them when retrying a retried message.
        let retried = format!("{RETRY_EARLIER_PREFIX}\n\nOriginal request");
        assert_eq!(strip_retry_prefixes(&retried), "Original request");
    }

    #[test]
    fn visible_user_ordinal_ignores_notices_and_agent_entries() {
        let with_notices = vec![
            Entry::User("first".into()),
            Entry::Notice { text: "Compacting context…".into(), error: false },
            Entry::Agent("reply".into()),
            Entry::User("second".into()),
        ];
        let replayed_without_notices = vec![
            Entry::User("first".into()),
            Entry::Agent("reply".into()),
            Entry::User("second".into()),
        ];
        assert_eq!(visible_user_ordinal(&with_notices, 0), Some(0));
        assert_eq!(visible_user_ordinal(&with_notices, 3), Some(1));
        assert_eq!(visible_user_ordinal(&replayed_without_notices, 0), Some(0));
        assert_eq!(visible_user_ordinal(&replayed_without_notices, 2), Some(1));
    }

    #[test]
    fn atomic_multi_turn_restore_rolls_back_on_partial_failure() {
        let dir = tempfile::tempdir().unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "t@t"],
            &["config", "user.name", "t"],
            &["config", "core.autocrlf", "false"],
        ] {
            assert!(std::process::Command::new("git").args(args).current_dir(dir.path()).status().unwrap().success());
        }
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, "a0\n").unwrap();
        std::fs::write(&b, "b0\n").unwrap();
        assert!(std::process::Command::new("git").args(["add", "."]).current_dir(dir.path()).status().unwrap().success());
        assert!(std::process::Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(dir.path()).status().unwrap().success());

        // Turn 1 edits a.txt
        let before1 = snapshot::take(dir.path(), "s").unwrap();
        std::fs::write(&a, "a1\n").unwrap();
        let patch1 = snapshot::capture(dir.path(), &before1).unwrap();
        let patch1_path = dir.path().join("p1.patch");
        // Write patch files outside the repo so they don't dirty the working tree!
        let patch_dir = tempfile::tempdir().unwrap();
        let p1 = patch_dir.path().join("p1.patch");
        let p2 = patch_dir.path().join("p2.patch");
        std::fs::write(&p1, &patch1).unwrap();
        let _ = patch1_path;

        // Manual edit to a.txt between turn 1 and turn 2 breaks turn 1's restore check
        std::fs::write(&a, "a_manual\n").unwrap();

        // Turn 2 edits b.txt
        let before2 = snapshot::take(dir.path(), "s").unwrap();
        std::fs::write(&b, "b1\n").unwrap();
        let patch2 = snapshot::capture(dir.path(), &before2).unwrap();
        std::fs::write(&p2, &patch2).unwrap();

        // Attempting to restore turn 2 then turn 1 must fail on turn 1 AND re-apply turn 2 so b.txt stays "b1\n".
        let err = restore_checkpoints_atomic(vec![
            (Uuid::new_v4(), dir.path().into(), before2, p2),
            (Uuid::new_v4(), dir.path().into(), before1, p1),
        ]);
        assert!(err.is_err());
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "a_manual\n");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "b1\n", "turn 2 must be rolled back forward when turn 1 fails");
    }
}

//! Extensible per-message metadata and actions: sent timestamp and action buttons (copy, edit & rewind,
//! restore checkpoint, retry with higher thinking, retry with another model, and fork session).

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use gpui::{AnyElement, ClickEvent, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div, px};
use gpui_kit::{assets::IconName, base::{AlertDialog, AlertDialogDescription, AlertDialogPopup, AlertDialogTitle}};
use uuid::Uuid;

use super::{Phase, Queued, ThreadEvent, ThreadView, queue, review};
use crate::git::snapshot;
use crate::harness::{CatalogModel, thread::Entry};
use crate::panels::{picker, scale};
use crate::panels::theme::theme;
use crate::panels::ui::{button as btn, tooltip::tooltip};
use crate::panels::widgets::{Dropdown, Trigger, icon};

pub(super) enum TurnActionKind {
    RetryHigherThinking {
        prompt: String,
        effort: String,
        effort_label: String,
        option_id: String,
    },
    RetryWithModel {
        prompt: String,
        target: CatalogModel,
    },
    RestoreOnly,
}

/// A rewind staged in the composer (via the Rewind button or double-Escape on the latest message).
/// Sending the composer message restores any checkpoints from `row` onward, truncates the conversation
/// before `row`, and starts a fresh harness turn with the edited message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PendingRewind {
    pub row: usize,
    pub checkpoints: usize,
}

pub(super) struct RetryConfirm {
    pub row: usize,
    pub kind: TurnActionKind,
    pub has_own_checkpoint: bool,
    pub later_checkpoints: usize,
}

pub(super) struct RetryModelPicker {
    pub row: usize,
    pub at: Dropdown,
}

pub(super) const RETRY_RESTORED_LATEST_PREFIX: &str =
    "I restored the files to the checkpoint before my previous message. Please try again:";
const RETRY_RESTORED_EARLIER_PREFIX: &str =
    "I restored the files to the checkpoint before an earlier message (undoing later file changes). Please retry that request:";
const RETRY_EARLIER_PREFIX: &str = "Please revisit and retry this earlier request:";
const RETRY_LATEST_PREFIX: &str = "Please retry this request with higher thinking:";
const RETRY_MODEL_PREFIX: &str = "Please retry this request:";

const RETRY_PREFIXES: &[&str] = &[
    RETRY_RESTORED_LATEST_PREFIX,
    RETRY_RESTORED_EARLIER_PREFIX,
    RETRY_EARLIER_PREFIX,
    RETRY_LATEST_PREFIX,
    RETRY_MODEL_PREFIX,
];

/// Strip quoted or legacy retry context so repeated retries do not stack it.
fn strip_retry_prefixes(mut text: &str) -> &str {
    loop {
        let mut matched = false;
        for &prefix in RETRY_PREFIXES {
            let unquoted = text.strip_prefix("> ").unwrap_or(text);
            if let Some(rest) = unquoted.strip_prefix(prefix).and_then(|s| s.strip_prefix("\n\n")) {
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

/// Optional notice shown above a user message on the right side (icon + text).
/// Add new variants here as other per-message notices are introduced.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum UserNotice {
    ResentHigherThinking { effort: String },
    ResentWithModel { model: String },
    Rewound { #[serde(default)] restored_checkpoints: usize },
}

impl UserNotice {
    pub(super) fn text(&self) -> String {
        match self {
            Self::ResentHigherThinking { effort } => format!("Resent with {effort} thinking"),
            Self::ResentWithModel { model } => format!("Resent with {model}"),
            Self::Rewound { restored_checkpoints: 0 } => "Rewound".to_string(),
            Self::Rewound { restored_checkpoints: 1 } => "Rewound · restored 1 checkpoint".to_string(),
            Self::Rewound { restored_checkpoints } => format!("Rewound · restored {restored_checkpoints} checkpoints"),
        }
    }
}

/// A retry waiting for a live option change (`thought_level` or `model`) and permission preservation to be confirmed before dispatching.
pub(super) struct PendingRetry {
    pub option_id: String,
    pub expected_value: String,
    pub notice: UserNotice,
    pub failure_message: &'static str,
    pub prompt: String,
    pub expected_permission: Option<crate::harness::permission::Permission>,
    pub expected_mode: Option<String>,
    pub permission_restored: bool,
}

type ActionHandler = Box<dyn Fn(&mut ThreadView, &mut Window, &mut Context<ThreadView>) + 'static>;
type DropdownHandler = Box<dyn Fn(&mut ThreadView, Dropdown, &mut Window, &mut Context<ThreadView>) + 'static>;

/// One icon button in a user message's action bar. New actions only need an entry in `user_actions_for`.
/// `on_click: None` (and `on_dropdown: None`) renders the button disabled while still showing its hover tooltip.
pub(super) struct UserAction {
    pub id: SharedString,
    pub icon: IconName,
    pub tooltip: SharedString,
    pub on_click: Option<ActionHandler>,
    pub on_dropdown: Option<DropdownHandler>,
}

#[cfg(test)]
impl UserAction {
    pub(super) fn is_enabled(&self) -> bool {
        self.on_click.is_some() || self.on_dropdown.is_some()
    }
}

/// Whether `entry` is a visible message typed by the user (not a hidden prompt or subagent report).
pub(crate) fn is_visible_user_entry(entry: &Entry) -> bool {
    matches!(
        entry,
        Entry::User(text)
            if !crate::harness::thread::is_hidden_prompt(text)
                && crate::harness::thread::parse_subagent_report(text).is_none()
    )
}

/// Exclusive end index of the turn starting at visible user message `row` (up to the next visible user message, or `entries.len()`).
pub(super) fn turn_end_for_row(entries: &[Entry], row: usize) -> usize {
    entries
        .iter()
        .enumerate()
        .skip(row.saturating_add(1))
        .find_map(|(ix, entry)| is_visible_user_entry(entry).then_some(ix))
        .unwrap_or(entries.len())
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

pub(crate) fn truncate_timestamps(record: &str, keep_count: usize) {
    if record.is_empty() {
        return;
    }
    let mut timestamps = load_timestamps(record);
    timestamps.retain(|&ordinal, _| ordinal < keep_count);
    save_timestamps(record, &timestamps);
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

pub(crate) fn truncate_notices(record: &str, keep_count: usize) {
    if record.is_empty() {
        return;
    }
    let mut notices = load_notices(record);
    notices.retain(|&ordinal, _| ordinal < keep_count);
    save_notices(record, &notices);
}

pub(crate) fn record_rewound_notice(record: &str, ordinal: usize, restored_checkpoints: usize) {
    if record.is_empty() {
        return;
    }
    let mut notices = load_notices(record);
    notices.insert(ordinal, UserNotice::Rewound { restored_checkpoints });
    save_notices(record, &notices);
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
    #[cfg(windows)]
    if let Some(parts) = windows_local_parts(ts, None) {
        return Some(parts);
    }
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

#[cfg(windows)]
fn windows_local_parts(ts: i64, zone: Option<&windows_sys::Win32::System::Time::DYNAMIC_TIME_ZONE_INFORMATION>) -> Option<CalendarParts> {
    use windows_sys::Win32::{Foundation::SYSTEMTIME, System::Time::SystemTimeToTzSpecificLocalTimeEx};
    let utc = time::OffsetDateTime::from_unix_timestamp(ts).ok()?;
    let utc = SYSTEMTIME {
        wYear: utc.year().try_into().ok()?,
        wMonth: u8::from(utc.month()).into(),
        wDay: utc.day().into(),
        wHour: utc.hour().into(),
        wMinute: utc.minute().into(),
        wSecond: utc.second().into(),
        ..Default::default()
    };
    let mut local = SYSTEMTIME::default();
    // SAFETY: both times and the optional zone live for the call; null selects the system's dynamic time zone.
    let converted = unsafe {
        SystemTimeToTzSpecificLocalTimeEx(zone.map_or(std::ptr::null(), std::ptr::from_ref), &utc, &mut local)
    };
    (converted != 0).then_some(CalendarParts {
        year: local.wYear.into(), month: local.wMonth as u8, day: local.wDay as u8,
        hour: local.wHour as u8, minute: local.wMinute as u8,
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

/// Restore `turns` (`(id, cwd, before, patch_path)`) from newest to oldest. Reads all patches before modifying
/// the working tree, and if any step fails partway through, re-applies already-reversed patches forward.
fn restore_checkpoints_atomic(turns: Vec<(Uuid, PathBuf, String, PathBuf)>) -> Result<Vec<Uuid>, String> {
    let mut loaded = Vec::with_capacity(turns.len());
    for (id, cwd, before, path) in turns {
        let patch = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
        loaded.push((id, cwd, before, patch));
    }
    // Read every file any checkpoint touches before changing one, so a failure anywhere puts them all back.
    let journals = loaded.iter().map(|(_, cwd, _, patch)| snapshot::Journal::capture(cwd, patch)).collect::<Result<Vec<_>, _>>()?;
    let mut restored_ids = Vec::with_capacity(loaded.len());
    for (id, cwd, before, patch) in loaded {
        if let Err(error) = snapshot::restore(&cwd, &before, &patch) {
            for journal in journals.iter().rev() {
                journal.put_back();
            }
            return Err(error);
        }
        restored_ids.push(id);
    }
    Ok(restored_ids)
}

impl ThreadView {
    /// After a rewind whose new session failed to start: save this (the original) conversation and its per-message
    /// timestamps, notices and queue again, which the rewind had already cut on disk.
    pub(crate) fn persist_after_failed_rewind(&mut self) {
        self.persist_entries();
        self.queue_changed();
        if self.timestamps_loaded {
            save_timestamps(&self.record_id, &self.user_timestamps);
        }
        if self.notices_loaded {
            save_notices(&self.record_id, &self.user_notices);
        }
    }

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

    /// Shared gate for per-message actions that mutate files or restart the session.
    fn rewind_block_reason(&self) -> Option<&'static str> {
        if self.retrying_turn || self.pending_retry.is_some() {
            Some("Restoring checkpoint and retrying…")
        } else if self.working || self.preparing.is_some() || !self.steering.is_empty() || self.has_background_work() || self.capturing_reviews > 0 {
            Some("Wait for the current turn to finish")
        } else if self.restoring_checkpoint() {
            Some("Wait for checkpoint restore to finish")
        } else if self.phase != Phase::Ready {
            Some("Session is not ready")
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

    /// Unified gate matching `send_next` plus retry/capture/handoff guards.
    fn retry_block_reason(&self) -> Option<&'static str> {
        if let Some(reason) = self.rewind_block_reason() {
            Some(reason)
        } else if self.pending_model.is_some() {
            Some("Clear the pending model switch before retrying")
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

    /// Models other than the currently active one, across all enabled harnesses.
    pub(super) fn other_models_for_retry(&self) -> Vec<CatalogModel> {
        let current_model_id = self
            .thread
            .option("model")
            .map(|o| self.option_value(o))
            .filter(|v| !v.is_empty() && *v != "default")
            .or(self.initial_model.as_deref());
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for model in self.unified_models() {
            if model.harness == self.harness && current_model_id.is_some_and(|cur| cur == model.id) {
                continue;
            }
            if seen.insert((model.harness, model.id.clone())) {
                out.push(model);
            }
        }
        out
    }

    pub(super) fn copy_entry(&mut self, row: usize, cx: &mut Context<Self>) {
        let text = match self.thread.entries.get(row) {
            Some(Entry::User(text) | Entry::Agent(text)) if !text.is_empty() => text.clone(),
            _ => return,
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.copied_seq = self.copied_seq.wrapping_add(1);
        let seq = self.copied_seq;
        self.copied_row = Some((row, seq));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            let _ = this.update(cx, |v, cx| {
                if v.copied_row == Some((row, seq)) {
                    v.copied_row = None;
                    cx.notify();
                }
            });
        }).detach();
    }

    fn is_recently_copied(&self, row: usize) -> bool {
        self.copied_row.is_some_and(|(r, _)| r == row)
    }

    /// Actions shown in the footer bar of the user message at `row`.
    pub(super) fn user_actions_for(&self, row: usize) -> Vec<UserAction> {
        let mut actions = Vec::new();
        let block = self.retry_block_reason();
        let active_row = self.is_active_user_row(row);
        let checkpoints = self.retry_checkpoints_for(row);
        let has_own = checkpoints.iter().any(|t| t.row == row);

        // 1. Copy message (always enabled)
        let copied = self.is_recently_copied(row);
        actions.push(UserAction {
            id: SharedString::from(format!("user-copy-{row}")),
            icon: if copied { IconName::Check } else { IconName::Copy },
            tooltip: SharedString::from(if copied { "Copied" } else { "Copy message" }),
            on_click: Some(Box::new(move |v: &mut ThreadView, _: &mut Window, cx: &mut Context<ThreadView>| {
                v.copy_entry(row, cx);
            })),
            on_dropdown: None,
        });

        // 2. Rewind conversation (and file checkpoints) to this message
        let rewind_block = self.rewind_block_reason();
        let (rewind_tip, rewind_enabled) = if active_row {
            ("Wait for the current turn to finish".to_string(), false)
        } else if let Some(reason) = rewind_block {
            (reason.to_string(), false)
        } else if checkpoints.len() == 1 {
            ("Rewind to this message & restore checkpoint".to_string(), true)
        } else if !checkpoints.is_empty() {
            (format!("Rewind to this message & restore {} checkpoints", checkpoints.len()), true)
        } else {
            ("Rewind to this message".to_string(), true)
        };
        actions.push(UserAction {
            id: SharedString::from(format!("user-rewind-{row}")),
            icon: IconName::Undo2,
            tooltip: SharedString::from(rewind_tip),
            on_click: rewind_enabled.then(|| {
                Box::new(move |v: &mut ThreadView, window: &mut Window, cx: &mut Context<ThreadView>| {
                    v.stage_rewind(row, window, cx);
                }) as ActionHandler
            }),
            on_dropdown: None,
        });

        // 3. Restore files to before this message (no resend) — shown when checkpoints exist
        if !checkpoints.is_empty() {
            let (restore_tip, restore_enabled) = if active_row {
                ("Wait for the current turn to finish".to_string(), false)
            } else if let Some(reason) = block {
                (reason.to_string(), false)
            } else if checkpoints.len() == 1 {
                ("Restore files to before this message".to_string(), true)
            } else {
                (format!("Restore files across {} checkpoints", checkpoints.len()), true)
            };
            actions.push(UserAction {
                id: SharedString::from(format!("user-restore-only-{row}")),
                icon: IconName::ArchiveRestore,
                tooltip: SharedString::from(restore_tip),
                on_click: restore_enabled.then(|| {
                    Box::new(move |v: &mut ThreadView, window: &mut Window, cx: &mut Context<ThreadView>| {
                        v.request_restore_only(row, window, cx);
                    }) as ActionHandler
                }),
                on_dropdown: None,
            });
        }

        // 4. Retry with higher thinking
        let next = self.next_higher_effort();
        let higher_entry = match (&next, block) {
            (NextThinking::Unsupported, _) if self.phase != Phase::Ready || self.workspace_switching => {
                Some((block.unwrap_or("Session is not ready").to_string(), false))
            }
            (NextThinking::Unsupported, _) => None,
            (NextThinking::AtMax, _) => Some(("Already at max thinking".to_string(), false)),
            (_, _) if active_row => Some(("Wait for the current turn to finish".to_string(), false)),
            (NextThinking::Higher(_, _, _), Some(reason)) => Some((reason.to_string(), false)),
            (NextThinking::Higher(_, _, label), None) => {
                let tip = if has_own {
                    format!("Restore checkpoint & retry with {label} thinking")
                } else if !checkpoints.is_empty() {
                    format!("Restore later checkpoints & retry with {label} thinking")
                } else {
                    format!("Retry with {label} thinking")
                };
                Some((tip, true))
            }
        };
        if let Some((tooltip, enabled)) = higher_entry {
            actions.push(UserAction {
                id: SharedString::from(format!("user-retry-higher-{row}")),
                icon: IconName::Brain,
                tooltip: SharedString::from(tooltip),
                on_click: enabled.then(|| {
                    Box::new(move |v: &mut ThreadView, window: &mut Window, cx: &mut Context<ThreadView>| {
                        v.request_retry_turn(row, window, cx);
                    }) as ActionHandler
                }),
                on_dropdown: None,
            });
        }

        // 5. Retry with another model / harness (opens anchored dropdown)
        let other_models = self.other_models_for_retry();
        if !other_models.is_empty() {
            let (model_tip, model_enabled) = if active_row {
                ("Wait for the current turn to finish".to_string(), false)
            } else if let Some(reason) = block {
                (reason.to_string(), false)
            } else if !checkpoints.is_empty() {
                ("Restore checkpoint & retry with another model…".to_string(), true)
            } else {
                ("Retry with another model…".to_string(), true)
            };
            actions.push(UserAction {
                id: SharedString::from(format!("user-retry-model-{row}")),
                icon: IconName::RefreshCw,
                tooltip: SharedString::from(model_tip),
                on_click: None,
                on_dropdown: model_enabled.then(|| {
                    Box::new(move |v: &mut ThreadView, at: Dropdown, window: &mut Window, cx: &mut Context<ThreadView>| {
                        v.toggle_retry_model_picker(row, at, window, cx);
                    }) as DropdownHandler
                }),
            });
        }

        // 6. Fork session from here
        let (fork_tip, fork_enabled) = if active_row {
            ("Wait for the current turn to finish".to_string(), false)
        } else if self.phase != Phase::Ready || self.workspace_switching {
            (block.unwrap_or("Session is not ready").to_string(), false)
        } else {
            ("Fork session from here".to_string(), true)
        };
        actions.push(UserAction {
            id: SharedString::from(format!("user-fork-{row}")),
            icon: IconName::GitBranch,
            tooltip: SharedString::from(fork_tip),
            on_click: fork_enabled.then(|| {
                Box::new(move |v: &mut ThreadView, _: &mut Window, cx: &mut Context<ThreadView>| {
                    v.request_fork_from_row(row, cx);
                }) as ActionHandler
            }),
            on_dropdown: None,
        });

        actions
    }

    fn open_confirm_for_checkpoints(&mut self, row: usize, kind: TurnActionKind, window: &mut Window, cx: &mut Context<Self>) {
        let checkpoints = self.retry_checkpoints_for(row);
        let has_own_checkpoint = checkpoints.iter().any(|t| t.row == row);
        let later_checkpoints = checkpoints.iter().filter(|t| t.row > row).count();
        self.retry_confirm = Some(RetryConfirm {
            row,
            kind,
            has_own_checkpoint,
            later_checkpoints,
        });
        window.focus(&self.restore_focus, cx);
        cx.notify();
    }

    pub(super) fn request_retry_turn(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.retry_block_reason().is_some() {
            return;
        }
        let Some(Entry::User(prompt)) = self.thread.entries.get(row) else { return };
        let NextThinking::Higher(option_id, effort, effort_label) = self.next_higher_effort() else { return };
        let prompt = strip_retry_prefixes(prompt).to_string();
        if !self.retry_checkpoints_for(row).is_empty() {
            self.open_confirm_for_checkpoints(
                row,
                TurnActionKind::RetryHigherThinking { prompt, effort, effort_label, option_id },
                window,
                cx,
            );
        } else {
            self.begin_live_retry(row, prompt, option_id, effort, effort_label, Vec::new(), cx);
        }
    }

    /// Stage a true rewind to `row`: loads the message into the composer and shows a warning banner above it.
    /// Sending the message executes the rewind (restoring checkpoints and deleting everything from `row` onward).
    pub(super) fn stage_rewind(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.rewind_block_reason().is_some() || self.is_active_user_row(row) {
            return;
        }
        let Some(Entry::User(prompt)) = self.thread.entries.get(row) else { return };
        let prompt = strip_retry_prefixes(prompt).to_string();
        let checkpoints = self.retry_checkpoints_for(row).len();
        self.pending_rewind = Some(PendingRewind { row, checkpoints });
        self.history_at = None;
        self.set_draft(&prompt, window, cx);
        self.focus_composer(window, cx);
        cx.notify();
    }

    /// Stage a rewind to the latest visible user message (triggered by double-pressing Escape).
    pub(super) fn stage_rewind_latest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.latest_visible_user_row() else { return };
        self.stage_rewind(row, window, cx);
    }

    /// Execute a staged rewind upon sending: restore any checkpoints from `rewind.row` onward, then emit
    /// `ThreadEvent::Rewind` so the workspace truncates the thread and starts a fresh session up to `rewind.row`.
    pub(super) fn begin_rewind(
        &mut self,
        rewind: PendingRewind,
        message: String,
        target_model: Option<CatalogModel>,
        effort: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(reason) = self.rewind_block_reason() {
            self.pending_rewind = Some(rewind);
            self.pending_model = target_model;
            self.pending_effort = effort;
            self.pending_edit_draft = Some(message);
            let at = self.thread.notice(format!("Could not rewind: {reason}."), true);
            self.remeasure(at);
            cx.notify();
            return;
        }
        let turns: Vec<(Uuid, PathBuf, String, PathBuf)> = self
            .retry_checkpoints_for(rewind.row)
            .into_iter()
            .map(|turn| (turn.id, turn.cwd.clone(), turn.before.clone(), review::patch_path(&self.record_id, turn.id)))
            .collect();
        if turns.is_empty() {
            cx.emit(ThreadEvent::Rewind { row: rewind.row, message, target_model, effort, restored_checkpoints: 0 });
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
                        let restored_checkpoints = restored_ids.len();
                        v.turn_reviews.retain(|turn| !restored_ids.contains(&turn.id));
                        review::prune_turn_reviews_by_ids(&v.record_id, &restored_ids, cx.background_executor());
                        cx.emit(ThreadEvent::Rewind { row: rewind.row, message, target_model, effort, restored_checkpoints });
                    }
                    Err(error) => {
                        v.pending_rewind = Some(rewind);
                        v.pending_model = target_model;
                        v.pending_effort = effort;
                        v.pending_edit_draft = Some(message);
                        let at = v.thread.notice(format!("Could not restore checkpoint: {error}"), true);
                        v.remeasure(at);
                    }
                }
                cx.notify();
            });
        }).detach();
    }

    pub(super) fn request_restore_only(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.retry_block_reason().is_some() || self.is_active_user_row(row) {
            return;
        }
        if self.retry_checkpoints_for(row).is_empty() {
            return;
        }
        self.open_confirm_for_checkpoints(row, TurnActionKind::RestoreOnly, window, cx);
    }

    pub(super) fn toggle_retry_model_picker(&mut self, row: usize, at: Dropdown, window: &mut Window, cx: &mut Context<Self>) {
        if self.retry_model_picker.as_ref().is_some_and(|p| p.row == row) {
            self.close_retry_model_picker(window, cx);
            return;
        }
        if self.retry_block_reason().is_some() || self.is_active_user_row(row) {
            return;
        }
        if self.other_models_for_retry().is_empty() {
            return;
        }
        self.picker = None;
        self.retry_model_picker = Some(RetryModelPicker { row, at });
        self.picker_nav.reset();
        self.focus_model_search(window, cx);
        cx.notify();
    }

    pub(super) fn close_retry_model_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.retry_model_picker.take().is_some() {
            self.focus_composer(window, cx);
            cx.notify();
        }
    }

    pub(super) fn select_retry_model(&mut self, row: usize, target: CatalogModel, window: &mut Window, cx: &mut Context<Self>) {
        self.retry_model_picker = None;
        if self.retry_block_reason().is_some() || self.is_active_user_row(row) {
            self.focus_composer(window, cx);
            cx.notify();
            return;
        }
        let Some(Entry::User(prompt)) = self.thread.entries.get(row) else {
            self.focus_composer(window, cx);
            cx.notify();
            return;
        };
        let prompt = strip_retry_prefixes(prompt).to_string();
        if !self.retry_checkpoints_for(row).is_empty() {
            self.open_confirm_for_checkpoints(row, TurnActionKind::RetryWithModel { prompt, target }, window, cx);
        } else {
            self.focus_composer(window, cx);
            self.begin_model_retry(row, prompt, target, Vec::new(), cx);
            cx.notify();
        }
    }

    pub(super) fn request_fork_from_row(&mut self, row: usize, cx: &mut Context<Self>) {
        if self.phase != Phase::Ready || self.workspace_switching || self.is_active_user_row(row) {
            return;
        }
        if !self.thread.entries.get(row).is_some_and(is_visible_user_entry) {
            return;
        }
        let end = turn_end_for_row(&self.thread.entries, row);
        cx.emit(ThreadEvent::Fork(end));
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
            let at = self.thread.notice(format!("Could not complete action: {reason}."), true);
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
            self.finish_confirmed_action(pending.row, pending.kind, Vec::new(), cx);
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
                        v.finish_confirmed_action(pending.row, pending.kind, restored_ids, cx);
                    }
                    Err(error) => {
                        let at = v.thread.notice(format!("Could not restore checkpoint: {error}"), true);
                        v.remeasure(at);
                        v.send_next(cx);
                    }
                }
                cx.notify();
            });
        }).detach();
    }

    fn finish_confirmed_action(&mut self, row: usize, kind: TurnActionKind, restored_ids: Vec<Uuid>, cx: &mut Context<Self>) {
        match kind {
            TurnActionKind::RetryHigherThinking { prompt, effort, effort_label, option_id } => {
                self.begin_live_retry(row, prompt, option_id, effort, effort_label, restored_ids, cx);
            }
            TurnActionKind::RetryWithModel { prompt, target } => {
                self.begin_model_retry(row, prompt, target, restored_ids, cx);
            }
            TurnActionKind::RestoreOnly => {
                self.prune_restored_checkpoints(&restored_ids, cx);
            }
        }
    }

    fn prune_restored_checkpoints(&mut self, restored_ids: &[Uuid], cx: &mut Context<Self>) -> bool {
        if restored_ids.is_empty() {
            return false;
        }
        let count = restored_ids.len();
        self.turn_reviews.retain(|turn| !restored_ids.contains(&turn.id));
        review::prune_turn_reviews_by_ids(&self.record_id, restored_ids, cx.background_executor());
        let notice = if count == 1 {
            "Restored files to checkpoint".to_string()
        } else {
            format!("Restored files across {count} checkpoints")
        };
        let at = self.thread.notice(notice, false);
        self.remeasure(at);
        self.persist_entries();
        true
    }

    fn build_retry_outgoing(&self, row: usize, prompt: String, restored_files: bool, latest_prefix: &str) -> String {
        let is_latest = self.latest_visible_user_row() == Some(row);
        if restored_files && is_latest {
            crate::harness::thread::prepend_quote(RETRY_RESTORED_LATEST_PREFIX, &prompt)
        } else if restored_files {
            crate::harness::thread::prepend_quote(RETRY_RESTORED_EARLIER_PREFIX, &prompt)
        } else if is_latest && prompt.starts_with('/') {
            // A native command must still start the prompt to be recognized by the adapter.
            prompt
        } else if is_latest {
            crate::harness::thread::prepend_quote(latest_prefix, &prompt)
        } else {
            crate::harness::thread::prepend_quote(RETRY_EARLIER_PREFIX, &prompt)
        }
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
        let restored_files = self.prune_restored_checkpoints(&restored_ids, cx);
        let outgoing = self.build_retry_outgoing(row, prompt, restored_files, RETRY_LATEST_PREFIX);
        let expected_permission = self.handoff_permission();
        let expected_mode = self.thread.mode.clone();
        self.pending_effort = Some(effort.clone());
        self.pending_retry = Some(PendingRetry {
            option_id: option_id.clone(),
            expected_value: effort.clone(),
            notice: UserNotice::ResentHigherThinking { effort: effort_label },
            failure_message: "Could not switch to higher thinking; retry cancelled.",
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

    /// Retry a turn on another model: either switch the live `model` option on the current harness,
    /// or hand off in place to another harness via `ThreadEvent::Handoff`.
    fn begin_model_retry(
        &mut self,
        row: usize,
        prompt: String,
        target: CatalogModel,
        restored_ids: Vec<Uuid>,
        cx: &mut Context<Self>,
    ) {
        let restored_files = self.prune_restored_checkpoints(&restored_ids, cx);
        let outgoing = self.build_retry_outgoing(row, prompt, restored_files, RETRY_MODEL_PREFIX);
        if target.harness != self.harness {
            cx.emit(ThreadEvent::Handoff {
                to: target.harness,
                model: Some(target.id),
                message: outgoing,
                effort: None,
                by_agent: false,
            });
            return;
        }
        let Some(option_id) = self.thread.option("model").map(|o| o.id.clone()) else {
            let at = self.thread.notice("This session does not support switching models in place.", true);
            self.remeasure(at);
            return;
        };
        let expected_permission = self.handoff_permission();
        let expected_mode = self.thread.mode.clone();
        self.pending_retry = Some(PendingRetry {
            option_id: option_id.clone(),
            expected_value: target.id.clone(),
            notice: UserNotice::ResentWithModel { model: target.name },
            failure_message: "Could not switch model; retry cancelled.",
            prompt: outgoing,
            expected_permission,
            expected_mode,
            permission_restored: false,
        });
        if self.connection.is_some() && self.session_id.is_some() {
            self.set_option(option_id, target.id, cx);
        } else {
            self.dispatch_pending_retry(cx);
        }
    }

    /// Called once all in-flight session options (or permission restoration) settle. Verifies that the requested option
    /// took effect and that the session's permission mode was preserved before sending the retry.
    pub(super) fn complete_pending_retry(&mut self, cx: &mut Context<Self>) {
        let Some(retry) = self.pending_retry.as_ref() else { return };
        let applied = self
            .thread
            .config
            .iter()
            .find(|o| o.id == retry.option_id)
            .is_some_and(|o| o.current == retry.expected_value);
        if !applied {
            let msg = retry.failure_message;
            self.cancel_pending_retry(msg, cx);
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
        item.notice = Some(retry.notice);
        self.queue.push_front(item);
        self.queue_changed();
        self.send_next(cx);
    }

    pub(super) fn render_user_header(&self, row: usize) -> Option<AnyElement> {
        let ordinal = visible_user_ordinal(&self.thread.entries, row)?;
        let notice = self.user_notices.get(&ordinal)?;
        Some(crate::panels::ui::notice::notice(notice.text(), crate::panels::ui::alert::Tone::Default).into_any_element())
    }

    fn action_icon_button(id: SharedString, glyph: IconName, tip: SharedString, enabled: bool) -> gpui::Stateful<gpui::Div> {
        let base = div()
            .id(id)
            .size(px(20.0))
            .rounded(px(5.0))
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme().subtle_foreground)
            .tooltip(tooltip(tip))
            .child(icon(glyph, 12.5));
        if enabled {
            base.cursor_pointer()
                .hover(|s| s.bg(theme().secondary_hover).text_color(theme().foreground))
        } else {
            base.opacity(0.5).cursor_default()
        }
    }

    pub(super) fn render_agent_footer(&self, row: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Some(Entry::Agent(text)) = self.thread.entries.get(row) else { return None };
        if text.trim().is_empty() {
            return None;
        }
        let copied = self.is_recently_copied(row);
        let tip = SharedString::from(if copied { "Copied" } else { "Copy message" });
        let copy_btn = Self::action_icon_button(
            SharedString::from(format!("agent-copy-{row}")),
            if copied { IconName::Check } else { IconName::Copy },
            tip,
            true,
        )
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
            cx.stop_propagation();
            v.copy_entry(row, cx);
        }));
        Some(
            div()
                .w_full()
                .px(px(2.0))
                .flex()
                .items_center()
                .justify_end()
                .child(copy_btn)
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
            if let Some(handler) = action.on_dropdown {
                Self::action_icon_button(action.id, action.icon, action.tooltip, true)
                    .dropdown_right(cx, move |v, at, window, cx| handler(v, at, window, cx))
                    .into_any_element()
            } else if let Some(handler) = action.on_click {
                Self::action_icon_button(action.id, action.icon, action.tooltip, true)
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        handler(v, window, cx);
                    }))
                    .into_any_element()
            } else {
                Self::action_icon_button(action.id, action.icon, action.tooltip, false)
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
        let scope = match (pending.has_own_checkpoint, later) {
            (true, 0) => "files changed in this turn".to_string(),
            (true, n) => format!("files changed in this turn and {n} later {plural}"),
            (false, n) => format!("files changed in {n} later {plural}"),
        };
        let (title, detail, confirm_label) = match &pending.kind {
            TurnActionKind::RetryHigherThinking { effort_label, .. } => {
                let title = if restores_files { "Restore checkpoint & retry?" } else { "Retry with higher thinking?" };
                let detail = if restores_files {
                    format!("This restores {scope} back to before your message, then retries with {effort_label} thinking.")
                } else {
                    format!("This switches thinking to {effort_label} and resends your message. No file checkpoints will be reverted.")
                };
                let label = if restores_files { format!("Restore & retry ({effort_label})") } else { format!("Retry ({effort_label})") };
                (title, detail, label)
            }
            TurnActionKind::RetryWithModel { target, .. } => (
                "Restore checkpoint & retry with model?",
                format!("This restores {scope} back to before your message, then retries using {}.", target.name),
                format!("Restore & retry ({})", target.name),
            ),
            TurnActionKind::RestoreOnly => (
                "Restore files to checkpoint?",
                format!("This restores {scope} back to before your message without resending anything."),
                "Restore files".to_string(),
            ),
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

    pub(super) fn render_retry_model_picker(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picker_state = self.retry_model_picker.as_ref()?;
        let row = picker_state.row;
        let at = picker_state.at;
        let models = self.other_models_for_retry();
        let current_harness = self.harness;
        Some(picker::model_dropdown_overlay(
            ("retry-model-picker", "RETRY WITH"),
            "retry-model-list",
            "retry-model-opt",
            at,
            30,
            &self.model_search,
            &self.picker_nav,
            models,
            move |h| (h != current_harness).then_some("handoff"),
            move |v, target, window, cx| v.select_retry_model(row, target, window, cx),
            |v, window, cx| v.close_retry_model_picker(window, cx),
            window,
            cx,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_timestamps_use_the_time_zone_and_cross_date_boundaries() {
        use windows_sys::Win32::System::Time::DYNAMIC_TIME_ZONE_INFORMATION;
        let zone = |name: &str| {
            let mut zone = DYNAMIC_TIME_ZONE_INFORMATION::default();
            // The dynamic API looks up historical rules by the Windows time-zone key.
            for (slot, unit) in zone.TimeZoneKeyName.iter_mut().zip(name.encode_utf16()) { *slot = unit; }
            zone
        };
        let timestamp = time::Date::from_calendar_date(2026, time::Month::November, 27).unwrap()
            .with_hms(23, 30, 0).unwrap().assume_utc().unix_timestamp();
        assert_eq!(windows_local_parts(timestamp, Some(&zone("South Africa Standard Time"))), Some(CalendarParts {
            year: 2026, month: 11, day: 28, hour: 1, minute: 30,
        }));
        assert_eq!(windows_local_parts(timestamp, Some(&zone("SA Pacific Standard Time"))), Some(CalendarParts {
            year: 2026, month: 11, day: 27, hour: 18, minute: 30,
        }));
    }

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
    fn retry_context_is_quoted_and_does_not_stack() {
        let retried = crate::harness::thread::prepend_quote(RETRY_EARLIER_PREFIX, "Original request");
        assert_eq!(retried, "> Please revisit and retry this earlier request:\n\nOriginal request");
        assert_eq!(strip_retry_prefixes(&retried), "Original request");
        assert_eq!(strip_retry_prefixes(&format!("{RETRY_EARLIER_PREFIX}\n\nOriginal request")), "Original request");
    }

    #[test]
    fn turn_end_for_row_stops_at_next_visible_user_entry() {
        let entries = vec![
            Entry::User("first".into()),
            Entry::Agent("reply 1".into()),
            Entry::notice("Restored files to checkpoint", false),
            Entry::User("second".into()),
            Entry::Agent("reply 2".into()),
        ];
        assert_eq!(turn_end_for_row(&entries, 0), 3);
        assert_eq!(turn_end_for_row(&entries, 3), 5);
    }

    #[test]
    fn resent_with_model_notice_formats_and_round_trips() {
        let _dirs = crate::datadir::use_throwaway_folders();
        let mut map = BTreeMap::new();
        map.insert(0, UserNotice::ResentWithModel { model: "Opus 5.5".into() });
        assert_eq!(map[&0].text(), "Resent with Opus 5.5");
        save_notices("user-model-notice-test", &map);
        assert_eq!(load_notices("user-model-notice-test"), map);
        clear_notices("user-model-notice-test");
    }

    #[test]
    fn visible_user_ordinal_ignores_notices_and_agent_entries() {
        let with_notices = vec![
            Entry::User("first".into()),
            Entry::notice("Compacting context…", false),
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

        // Manual overlapping edit to a.txt between turn 1 and turn 2:
        std::fs::write(&a, "a_manual\n").unwrap();

        // Turn 2 edits b.txt
        let before2 = snapshot::take(dir.path(), "s").unwrap();
        std::fs::write(&b, "b1\n").unwrap();
        let patch2 = snapshot::capture(dir.path(), &before2).unwrap();
        std::fs::write(&p2, &patch2).unwrap();

        // If turn 1's snapshot commit is missing AND its patch conflicts, turn 1 fails AND turn 2 rolls back forward.
        let err = restore_checkpoints_atomic(vec![
            (Uuid::new_v4(), dir.path().into(), before2.clone(), p2.clone()),
            (Uuid::new_v4(), dir.path().into(), "0000000000000000000000000000000000000000".into(), p1.clone()),
        ]);
        assert!(err.is_err());
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "a_manual\n");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "b1\n", "turn 2 must be rolled back forward when turn 1 fails");

        // With valid before1 snapshot, restoring both turns succeeds even after the manual edit to a.txt.
        let ok = restore_checkpoints_atomic(vec![
            (Uuid::new_v4(), dir.path().into(), before2, p2),
            (Uuid::new_v4(), dir.path().into(), before1, p1),
        ]);
        assert!(ok.is_ok());
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "a0\n");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "b0\n");
    }
}

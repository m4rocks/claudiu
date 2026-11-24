//! Reviewing what the agent changed since the last message, hunk by hunk, as in a pull request: keep a hunk, put it
//! back as it was (`git/snapshot.rs`), or comment on one of its lines. The comments go back to the agent as one
//! message, each naming its line as `@path#L12`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui::{AnyElement, ClickEvent, Context, Div, Entity, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, ListAlignment, ListState, ParentElement, SharedString, Styled, Subscription, Window, div, list, prelude::FluentBuilder, px};
use gpui_kit::{assets::IconName, base::{AlertDialog, AlertDialogDescription, AlertDialogPopup, AlertDialogTitle}};
use gpui_kit::component::input::{InputEvent, InputState};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::{ThreadEvent, ThreadView};
use crate::git::snapshot::{self, FileChange, Hunk};
use crate::panels::comment_box::{COMMENT_SIZE, comment_box, comment_input, comment_trigger};
use crate::panels::scale;
use crate::panels::theme::{self, hsla};
use crate::panels::widgets::{button, button_colored, empty_panel, skeleton_rows, spinner};

pub(super) struct ActiveTurn {
    pub id: Uuid,
    pub row: usize,
    pub text: String,
    pub before: String,
    pub cwd: PathBuf,
}

#[derive(Deserialize, Serialize)]
pub(super) struct TurnReview {
    pub id: Uuid,
    pub row: usize,
    pub end: usize,
    pub text: String,
    pub before: String,
    pub cwd: PathBuf,
    pub file_count: usize,
    pub checkpoint: bool,
}

fn history_path(record: &str) -> PathBuf {
    crate::datadir::data_dir().join("turn-reviews").join(crate::datadir::safe_file_stem(record))
}

fn patch_path(record: &str, id: Uuid) -> PathBuf {
    history_path(record).join(format!("{id}.patch"))
}

pub(super) fn write_turn_patch(record: &str, id: Uuid, patch: &str) -> std::io::Result<()> {
    crate::datadir::write_atomic(&patch_path(record, id), patch)
}

pub(super) fn discard_turn_patch(record: &str, id: Uuid, cwd: &Path) {
    let _ = std::fs::remove_file(patch_path(record, id));
    snapshot::forget_checkpoint(cwd, record, id);
}

pub(crate) fn clear_turn_reviews(record: &str, executor: &gpui::BackgroundExecutor) {
    let turns = load_turn_reviews(record);
    let prior_record = record.to_string();
    executor.spawn(async move {
        for turn in turns {
            discard_turn_patch(&prior_record, turn.id, &turn.cwd);
        }
    }).detach();
    if !record.is_empty() && let Ok(dir) = std::fs::read_dir(history_path(record)) {
        for entry in dir.flatten().filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json")) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

pub(super) fn load_turn_reviews(record: &str) -> Vec<TurnReview> {
    if record.is_empty() {
        return Vec::new();
    }
    let Ok(dir) = std::fs::read_dir(history_path(record)) else { return Vec::new() };
    let mut turns: Vec<TurnReview> = dir.flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| serde_json::from_slice(&std::fs::read(entry.path()).ok()?).ok())
        .collect();
    turns.sort_by_key(|turn| turn.row);
    turns
}

enum ReviewRow {
    Error(String),
    FileHeader { file_ix: usize },
    NoDiff,
    HunkHeader { file_ix: usize, hunk_ix: usize, kept: bool },
    DiffLine { file_ix: usize, hunk_ix: usize, line_ix: usize, last_in_hunk: bool, kept: bool },
    Comment { text: String },
    CommentInput,
}

pub struct Review {
    /// A live review has a baseline and supports hunk edits. A completed turn has captured files.
    snapshot: Option<String>,
    id: Uuid,
    restore_confirm: bool,
    restoring: bool,
    /// None while the changes are read.
    files: Option<Vec<FileChange>>,
    /// Hunks the user kept, by their patch, which stays the same while other hunks are put back.
    kept: HashSet<String>,
    comments: Vec<Comment>,
    /// The line being commented on, the comment's input and the subscription to its Enter.
    commenting: Option<(Comment, Entity<InputState>, Subscription)>,
    error: Option<String>,
    /// Flattened rows rendered lazily by the virtual list so huge diffs stay fast.
    rows: Vec<ReviewRow>,
    list: ListState,
    glide: crate::panels::glide::SmoothList,
}

impl Review {
    fn hunk(&self, file_ix: usize, hunk_ix: usize) -> Option<&Hunk> {
        self.files.as_ref()?.get(file_ix)?.hunks.get(hunk_ix)
    }

    fn new(id: Uuid, snapshot: Option<String>) -> Self {
        Self {
            snapshot,
            id,
            restore_confirm: false,
            restoring: false,
            files: None,
            kept: HashSet::new(),
            comments: Vec::new(),
            commenting: None,
            error: None,
            rows: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(600.0)),
            glide: Default::default(),
        }
    }

    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        if let Some(why) = &self.error {
            rows.push(ReviewRow::Error(why.clone()));
        }
        if let Some(files) = &self.files {
            for (file_ix, file) in files.iter().enumerate() {
                rows.push(ReviewRow::FileHeader { file_ix });
                if file.hunks.is_empty() {
                    rows.push(ReviewRow::NoDiff);
                    continue;
                }
                for (hunk_ix, hunk) in file.hunks.iter().enumerate() {
                    let kept = self.kept.contains(&hunk.patch);
                    rows.push(ReviewRow::HunkHeader { file_ix, hunk_ix, kept });
                    if kept {
                        continue;
                    }
                    let last_line = hunk.lines.len().saturating_sub(1);
                    for (line_ix, line) in hunk.lines.iter().enumerate() {
                        rows.push(ReviewRow::DiffLine {
                            file_ix,
                            hunk_ix,
                            line_ix,
                            last_in_hunk: line_ix == last_line,
                            kept,
                        });
                        if self.snapshot.is_some()
                            && let Some(n) = line.new_line
                        {
                            for c in self.comments.iter().filter(|c| c.path == file.path && c.line == n) {
                                rows.push(ReviewRow::Comment { text: c.text.clone() });
                            }
                            if self.commenting.as_ref().is_some_and(|(c, _, _)| c.path == file.path && c.line == n) {
                                rows.push(ReviewRow::CommentInput);
                            }
                        }
                    }
                }
            }
        }
        let scroll = self.list.logical_scroll_top();
        self.rows = rows;
        self.list.reset(self.rows.len());
        if scroll.item_ix < self.rows.len() {
            self.list.scroll_to(scroll);
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Comment {
    pub path: PathBuf,
    pub line: u32,
    pub text: String,
}

/// The message that hands the comments to the agent.
pub fn comments_message(comments: &[Comment]) -> String {
    let mut message = String::from("Review comments on your changes:\n");
    for c in comments {
        message.push_str(&format!("\n@{}#L{}: {}", c.path.to_string_lossy().replace('\\', "/"), c.line, c.text.trim()));
    }
    message
}

impl ThreadView {
    pub(super) fn capture_turn_review(&mut self, result: Result<Value, String>, cx: &mut Context<Self>) {
        let Some(active) = self.active_turn_review.take() else {
            self.turn_ended(result, cx);
            return;
        };
        let end = self.thread.entries.len();
        let record = self.record_id.clone();
        let session = self.session_id.clone();
        let (cwd, before, patch_record, id) = (active.cwd.clone(), active.before.clone(), record.clone(), active.id);
        cx.spawn(async move |this, cx| {
            let captured = cx.background_executor().spawn(async move {
                let patch = snapshot::capture(&cwd, &before)?;
                let file_count = snapshot::parse(&patch).len();
                let checkpoint = if file_count > 0 {
                    write_turn_patch(&patch_record, id, &patch).map_err(|error| error.to_string())?;
                    snapshot::keep_checkpoint(&cwd, &patch_record, id, &before).is_ok()
                } else {
                    false
                };
                Ok::<_, String>((file_count, checkpoint))
            }).await;
            let saved = this.update(cx, |v, cx| {
                if !v.persist_on_drop || v.phase != super::Phase::Ready || v.session_id != session {
                    return false;
                }
                let saved = match &captured {
                    Ok((file_count, checkpoint)) if *file_count > 0 => v.remember_turn_review(TurnReview {
                        id, row: active.row, end, text: active.text, file_count: *file_count,
                        before: active.before, checkpoint: *checkpoint, cwd: active.cwd.clone(),
                    }, cx),
                    Err(error) => {
                        let at = v.thread.notice(format!("Could not capture turn changes: {error}"), true);
                        v.remeasure(at);
                        false
                    }
                    _ => false,
                };
                v.turn_ended(result, cx);
                saved
            }).unwrap_or(false);
            if !saved && captured.is_ok_and(|(count, _)| count > 0) {
                cx.background_executor().spawn(async move { discard_turn_patch(&record, id, &active.cwd); }).detach();
            }
        }).detach();
    }

    fn remember_turn_review(&mut self, turn: TurnReview, cx: &mut Context<Self>) -> bool {
        if !self.record_id.is_empty() && let Err(error) = serde_json::to_string(&turn)
            .map_err(std::io::Error::other)
            .and_then(|json| crate::datadir::write_atomic(&history_path(&self.record_id).join(format!("{}.json", turn.id)), &json))
        {
            let at = self.thread.notice(format!("Could not save turn review: {error}"), true);
            self.remeasure(at);
            cx.notify();
            return false;
        }
        self.turn_reviews.push(turn);
        cx.notify();
        true
    }

    #[cfg(test)]
    pub(crate) fn open_turn_review(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if !self.turn_reviews.iter().any(|turn| turn.id == id) {
            return;
        }
        self.review = Some(Review::new(id, None));
        let path = patch_path(&self.record_id, id);
        cx.spawn(async move |this, cx| {
            let read = cx.background_executor().spawn(async move {
                std::fs::read_to_string(path).map(|patch| snapshot::parse(&patch))
            }).await;
            let _ = this.update(cx, |v, cx| {
                let Some(review) = v.review.as_mut().filter(|review| review.id == id) else { return };
                match read {
                    Ok(files) => review.files = Some(files),
                    Err(error) => {
                        review.files = Some(Vec::new());
                        review.error = Some(format!("Could not open the turn review: {error}"));
                    }
                }
                review.rebuild_rows();
                cx.notify();
            });
        }).detach();
        cx.notify();
    }

    /// Whether the last turn left anything to review, and no turn is changing files right now.
    pub fn can_review(&self) -> bool {
        !self.working && self.changed == Some(true)
    }

    /// Whether the check for changes since the last snapshot has come back.
    #[cfg(test)]
    pub fn changes_known(&self) -> bool {
        !self.working && self.changed.is_some()
    }

    /// Read in the background whether the working tree differs from the last snapshot, so Review changes is off
    /// when there is nothing to show.
    pub(super) fn check_changes(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshots.last().cloned() else { return };
        self.changed = None;
        let cwd = self.cwd.clone();
        cx.spawn(async move |this, cx| {
            let source = snapshot.clone();
            let read = cx.background_executor().spawn(async move { snapshot::changes(&cwd, &source) }).await;
            let _ = this.update(cx, |v, cx| {
                if !v.working && v.snapshots.last() == Some(&snapshot) {
                    v.changed = Some(read.is_ok_and(|files| !files.is_empty()));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn open_review(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshots.last().cloned() else { return };
        self.review = Some(Review::new(Uuid::new_v4(), Some(snapshot)));
        self.reload_review(cx);
    }

    fn reload_review(&mut self, cx: &mut Context<Self>) {
        let Some(review) = &self.review else { return };
        let Some(snapshot) = review.snapshot.clone() else { return };
        let (cwd, id) = (self.cwd.clone(), review.id);
        cx.spawn(async move |this, cx| {
            let read = cx.background_executor().spawn(async move { snapshot::changes(&cwd, &snapshot) }).await;
            let _ = this.update(cx, |v, cx| {
                if let Some(review) = &mut v.review && review.id == id {
                    match read {
                        Ok(files) => review.files = Some(files),
                        Err(why) => {
                            review.files = Some(Vec::new());
                            review.error = Some(why);
                        }
                    }
                    review.rebuild_rows();
                }
                cx.notify();
            });
        })
        .detach();
    }

    #[cfg(test)]
    pub(crate) fn turn_review_ids(&self) -> Vec<Uuid> {
        self.turn_reviews.iter().map(|turn| turn.id).collect()
    }

    /// The changes the open review shows, once read.
    #[cfg(test)]
    pub fn review_files(&self) -> Option<&[FileChange]> {
        self.review.as_ref()?.files.as_deref()
    }

    /// Add a comment as if typed under `line` of `path`.
    #[cfg(test)]
    pub fn comment_on(&mut self, path: PathBuf, line: u32, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.start_comment(path, line, window, cx);
        self.finish_comment(text.to_string(), window, cx);
    }

    fn close_review(&mut self, cx: &mut Context<Self>) {
        self.review = None;
        // Hunks put back during the review may have left nothing to review.
        self.check_changes(cx);
        cx.notify();
    }

    fn keep_hunk(&mut self, patch: String, cx: &mut Context<Self>) {
        if let Some(review) = &mut self.review {
            review.kept.insert(patch);
            review.rebuild_rows();
        }
        cx.notify();
    }

    pub fn revert_hunk(&mut self, hunk: Hunk, cx: &mut Context<Self>) {
        let cwd = self.cwd.clone();
        cx.spawn(async move |this, cx| {
            let done = cx.background_executor().spawn(async move { snapshot::revert(&cwd, &hunk) }).await;
            let _ = this.update(cx, |v, cx| {
                if let (Err(why), Some(review)) = (done, &mut v.review) {
                    review.error = Some(format!("Couldn't put the hunk back, the file changed since: {why}"));
                    review.rebuild_rows();
                }
                v.reload_review(cx);
            });
        })
        .detach();
    }

    fn start_comment(&mut self, path: PathBuf, line: u32, window: &mut Window, cx: &mut Context<Self>) {
        let input = comment_input(&format!("line {line}"), window, cx);
        let sub = cx.subscribe_in(&input, window, |v: &mut Self, input, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { secondary, .. } = ev {
                let text = input.read(cx).value().trim().to_string();
                v.finish_comment(text, window, cx);
                if *secondary {
                    v.send_comments(window, cx);
                }
            }
        });
        if let Some(review) = &mut self.review {
            review.commenting = Some((Comment { path, line, text: String::new() }, input, sub));
            review.rebuild_rows();
        }
        cx.notify();
    }

    /// Keep the comment being written, unless it is empty, and hand the keyboard back to the composer.
    fn finish_comment(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(review) = &mut self.review else { return };
        if let Some((mut comment, _, _)) = review.commenting.take()
            && !text.is_empty()
        {
            comment.text = text;
            review.comments.push(comment);
        }
        review.rebuild_rows();
        self.composer.update(cx, |c, cx| c.focus(window, cx));
        cx.notify();
    }

    pub fn send_comments(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(review) = self.review.take() else { return };
        if !review.comments.is_empty() {
            self.send(comments_message(&review.comments), window, cx);
        }
        cx.notify();
    }

    pub(super) fn restoring_checkpoint(&self) -> bool {
        self.review.as_ref().is_some_and(|review| review.restoring)
    }

    fn can_restore(&self, turn: &TurnReview) -> bool {
        turn.checkpoint && turn.cwd == self.cwd && !self.is_working()
            && !self.has_background_work()
    }

    fn restore_turn(&mut self, id: Uuid, cx: &mut Context<Self>) {
        let Some(turn) = self.turn_reviews.iter().find(|turn| turn.id == id).filter(|turn| self.can_restore(turn)) else { return };
        let Some(review) = self.review.as_mut().filter(|review| review.id == id && !review.restoring && review.files.is_some() && review.error.is_none()) else { return };
        if !std::mem::take(&mut review.restore_confirm) {
            return;
        }
        review.restoring = true;
        review.error = None;
        let (cwd, before, path) = (turn.cwd.clone(), turn.before.clone(), patch_path(&self.record_id, id));
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move {
                let patch = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
                snapshot::restore(&cwd, &before, &patch)
            }).await;
            let _ = this.update(cx, |v, cx| {
                let Some(review) = v.review.as_mut().filter(|review| review.id == id) else { return };
                review.restoring = false;
                match result {
                    Ok(()) => v.close_review(cx),
                    Err(error) => {
                        review.error = Some(error);
                        review.rebuild_rows();
                    }
                }
                v.send_next(cx);
                cx.notify();
            });
        }).detach();
        cx.notify();
    }

    pub(super) fn render_review(&self, _window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let review = self.review.as_ref()?;
        let body: AnyElement = match &review.files {
            None => div()
                .flex_1()
                .min_h_0()
                .px(px(18.0))
                .pt(px(12.0))
                .flex()
                .flex_col()
                .gap(px(12.0))
                .child(div().text_color(hsla(theme::TEXT_DIM)).child("Reading changes"))
                .child(skeleton_rows(5))
                .into_any_element(),
            Some(files) if files.is_empty() && review.error.is_none() => div()
                .flex_1()
                .min_h_0()
                .px(px(18.0))
                .pt(px(12.0))
                .child(empty_panel(IconName::FileDiff, "No changes to review"))
                .into_any_element(),
            Some(_) => {
                let rows_list = list(review.list.clone(), cx.processor(|v: &mut Self, ix: usize, _w, cx| v.render_review_row(ix, cx)))
                    .size_full()
                    .pt(px(12.0))
                    .pb(px(12.0));
                div()
                    .id("thread-review")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(rows_list)
                    .child(review.glide.overlay(&review.list, px(24.0), !review.restore_confirm))
                    .child(gpui_kit::base::Scrollbar::vertical(&review.list).mode(gpui_kit::base::ScrollbarMode::Always))
                    .into_any_element()
            }
        };
        let count = review.comments.len();
        let turn = self.turn_reviews.iter().find(|turn| turn.id == review.id);
        let footer = div()
            .flex_none()
            .px(px(18.0))
            .py(px(10.0))
            .flex()
            .gap(px(8.0))
            .items_center()
            .border_t_1()
            .border_color(hsla(theme::PANEL_BORDER))
            .child(div().flex_1().text_color(hsla(theme::TEXT_DIM)).child(if review.snapshot.is_some() { "Changes since your last message" } else { "Changes captured at the end of this turn" }))
            .when_some(turn, |footer, turn| {
                let (id, end) = (turn.id, turn.end);
                footer.when(!review.restoring, |footer| footer.child(
                    button("review-fork", "Fork Thread", false)
                        .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| cx.emit(ThreadEvent::Fork(end)))),
                )).when(!review.restoring && review.files.is_some() && review.error.is_none() && self.can_restore(turn), |footer| {
                    footer.child(button("review-restore", "Restore checkpoint", false).debug_selector(|| "review-restore".into())
                        .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| {
                            if let Some(review) = v.review.as_mut().filter(|review| review.id == id && review.error.is_none()) {
                                review.restore_confirm = true;
                                window.focus(&v.restore_focus, cx);
                                cx.notify();
                            }
                        })))
                })
            })
            .when(review.restoring, |footer| footer.child(spinner("review-restoring", 12.0)))
            .when(!review.restoring, |footer| footer.child(
                button("review-close", "Close", false).on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.close_review(cx))),
            ))
            .when(count > 0, |d| {
                let label = if count == 1 { "Send 1 comment".to_string() } else { format!("Send {count} comments") };
                d.child(button("review-send", label, true).on_click(cx.listener(|v, _: &ClickEvent, window, cx| v.send_comments(window, cx))))
            });
        let alert = review.restore_confirm.then(|| {
            let id = review.id;
            let clear = cx.entity().downgrade();
            AlertDialog::new(cx)
                .focus_handle(self.restore_focus.clone())
                .on_open_change(move |open, _, _, cx| {
                    if !open {
                        let _ = clear.update(cx, |v, cx| v.cancel_restore(cx));
                    }
                })
                .backdrop(div().absolute().inset_0().bg(gpui::black().opacity(0.6)))
                .popup(AlertDialogPopup::new().w(px(420.0)).max_w_full().p(px(20.0))
                    .flex().flex_col().gap(px(10.0)).rounded(px(10.0))
                    .bg(hsla(theme::PANEL_BG)).border_1().border_color(hsla(theme::PANEL_BORDER))
                    .child(AlertDialogTitle::new().font_weight(FontWeight::SEMIBOLD).text_size(scale::px(16.0)).child("Restore this checkpoint?"))
                    .child(AlertDialogDescription::new().text_color(hsla(theme::TEXT_DIM))
                        .child("This puts files changed in this turn back to their earlier state. Restore is blocked if those files have changed since."))
                    .child(div().flex().justify_end().gap(px(8.0)).pt(px(6.0))
                        .child(button("restore-cancel", "Cancel", false).debug_selector(|| "restore-cancel".into())
                            .on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.cancel_restore(cx))))
                        .child(button_colored("restore-confirm", None, "Restore checkpoint", hsla(theme::DANGER),
                            hsla(theme::PANEL_BG), hsla(theme::DANGER).opacity(0.8), None, true)
                            .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                                v.restore_turn(id, cx);
                                v.cancel_restore(cx);
                            })))))
        });
        Some(div().flex_1().flex().flex_col().min_h_0().child(body).child(footer).children(alert))
    }

    fn cancel_restore(&mut self, cx: &mut Context<Self>) {
        if let Some(review) = &mut self.review {
            review.restore_confirm = false;
            cx.notify();
        }
    }

    fn render_review_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(review) = self.review.as_ref() else { return div().into_any_element() };
        let Some(row) = review.rows.get(ix) else { return div().into_any_element() };
        let files = review.files.as_deref().unwrap_or(&[]);
        let wrap = |el: AnyElement| div().w_full().px(px(18.0)).child(el).into_any_element();
        match row {
            ReviewRow::Error(why) => wrap(div().pb(px(8.0)).text_color(hsla(theme::DANGER)).child(why.clone()).into_any_element()),
            ReviewRow::FileHeader { file_ix } => {
                let Some(file) = files.get(*file_ix) else { return div().into_any_element() };
                wrap(
                    div()
                        .pt(px(if *file_ix == 0 { 0.0 } else { 12.0 }))
                        .pb(px(6.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(file.path.display().to_string())
                        .into_any_element(),
                )
            }
            ReviewRow::NoDiff => wrap(div().pb(px(6.0)).text_color(hsla(theme::TEXT_DIM)).child("No textual diff").into_any_element()),
            ReviewRow::HunkHeader { file_ix, hunk_ix, kept } => {
                let Some(hunk) = review.hunk(*file_ix, *hunk_ix) else { return div().into_any_element() };
                let (file_ix, hunk_ix) = (*file_ix, *hunk_ix);
                let border_color = hsla(if *kept { theme::OK } else { theme::PANEL_BORDER });
                let header = div()
                    .mt(px(if hunk_ix == 0 { 0.0 } else { 6.0 }))
                    .font_family(crate::platform::monospace_font())
                    .text_size(scale::px(12.5))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(8.0))
                    .py(px(3.0))
                    .bg(hsla(theme::APP_BG))
                    .border_t_1()
                    .border_l_1()
                    .border_r_1()
                    .when(*kept, |d| d.border_b_1().rounded(px(4.0)))
                    .when(!*kept, |d| d.rounded_t(px(4.0)))
                    .border_color(border_color)
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(hsla(theme::TEXT_FAINT)).child(hunk.header.clone()))
                    .when(review.snapshot.is_some(), |header| {
                        header
                            .child(if *kept {
                                div().text_color(hsla(theme::OK)).child("Kept").into_any_element()
                            } else {
                                button(SharedString::from(format!("review-keep-{file_ix}-{hunk_ix}")), "Keep", false)
                                    .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                                        if let Some(hunk) = v.review.as_ref().and_then(|r| r.hunk(file_ix, hunk_ix)) {
                                            v.keep_hunk(hunk.patch.clone(), cx);
                                        }
                                    }))
                                    .into_any_element()
                            })
                            .child(
                                button(SharedString::from(format!("review-revert-{file_ix}-{hunk_ix}")), "Revert", false)
                                    .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                                        if let Some(hunk) = v.review.as_ref().and_then(|r| r.hunk(file_ix, hunk_ix)) {
                                            v.revert_hunk(hunk.clone(), cx);
                                        }
                                    })),
                            )
                    });
                wrap(header.into_any_element())
            }
            ReviewRow::DiffLine { file_ix, hunk_ix, line_ix, last_in_hunk, kept } => {
                let Some(file) = files.get(*file_ix) else { return div().into_any_element() };
                let Some(line) = file.hunks.get(*hunk_ix).and_then(|h| h.lines.get(*line_ix)) else { return div().into_any_element() };
                let (bg, fg) = match line.origin {
                    '+' => (theme::DIFF_ADDED_BG, theme::OK),
                    '-' => (theme::DIFF_REMOVED_BG, theme::DANGER),
                    _ => (theme::PANEL_BG, theme::TEXT_DIM),
                };
                let number = line.new_line.map_or(String::new(), |n| n.to_string());
                let comment = review.snapshot.is_some().then(|| {
                    if let Some(n) = line.new_line {
                        let path = file.path.clone();
                        comment_trigger(format!("review-comment-{file_ix}-{hunk_ix}-{line_ix}"), false)
                            .opacity(0.65)
                            .group_hover("review-diff-line", |style| style.opacity(1.0))
                            .focus_visible(|style| style.opacity(1.0))
                            .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| v.start_comment(path.clone(), n, window, cx)))
                            .into_any_element()
                    } else {
                        div().size(px(COMMENT_SIZE)).into_any_element()
                    }
                });
                let border_color = hsla(if *kept { theme::OK } else { theme::PANEL_BORDER });
                let row = div()
                    .id(SharedString::from(format!("review-line-{file_ix}-{hunk_ix}-{line_ix}")))
                    .group("review-diff-line")
                    .font_family(crate::platform::monospace_font())
                    .text_size(scale::px(12.5))
                    .flex()
                    .bg(hsla(bg))
                    .text_color(hsla(fg))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .items_center()
                    .border_l_1()
                    .border_r_1()
                    .when(*last_in_hunk, |d| d.border_b_1().rounded_b(px(4.0)))
                    .border_color(border_color)
                    .children(comment)
                    .child(div().w(px(44.0)).flex_none().pr(px(8.0)).text_right().text_color(hsla(theme::TEXT_FAINT)).child(number))
                    .child(div().min_w_0().overflow_hidden().child(format!("{} {}", line.origin, line.text)));
                wrap(row.into_any_element())
            }
            ReviewRow::Comment { text } => wrap(
                div()
                    .border_l_1()
                    .border_r_1()
                    .border_color(hsla(theme::PANEL_BORDER))
                    .pl(px(44.0 + COMMENT_SIZE))
                    .pr(px(8.0))
                    .py(px(3.0))
                    .text_color(hsla(theme::ACCENT))
                    .child(text.clone())
                    .into_any_element(),
            ),
            ReviewRow::CommentInput => {
                let Some((_, input, _)) = review.commenting.as_ref() else { return div().into_any_element() };
                wrap(
                    div()
                        .border_l_1()
                        .border_r_1()
                        .border_color(hsla(theme::PANEL_BORDER))
                        .pl(px(44.0 + COMMENT_SIZE))
                        .p(px(4.0))
                        .on_key_down(cx.listener(|v, ev: &KeyDownEvent, window, cx| {
                            if ev.keystroke.key == "escape" {
                                v.finish_comment(String::new(), window, cx);
                                cx.stop_propagation();
                            }
                        }))
                        .child(comment_box(input, "keeps it", "sends all"))
                        .into_any_element(),
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_go_back_naming_their_lines() {
        let comments = [Comment { path: "src/a.rs".into(), line: 12, text: " use a match \n".into() }, Comment { path: "b.txt".into(), line: 3, text: "typo".into() }];
        assert_eq!(comments_message(&comments), "Review comments on your changes:\n\n@src/a.rs#L12: use a match\n@b.txt#L3: typo");
    }
}

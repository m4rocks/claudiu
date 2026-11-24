//! Reviewing what the agent changed since the last message, hunk by hunk, as in a pull request: keep a hunk, put it
//! back as it was (`git/snapshot.rs`), or comment on one of its lines. The comments go back to the agent as one
//! message, each naming its line as `@path#L12`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui::{Subscription, ClickEvent, Context, Div, Entity, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, SharedString, Styled, Window, div, prelude::FluentBuilder, px};
use gpui_kit::component::input::{InputEvent, InputState};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::{ThreadEvent, ThreadView};
use crate::git::snapshot::{self, FileChange, Hunk};
use crate::panels::comment_box::{COMMENT_SIZE, comment_box, comment_input, comment_trigger};
use crate::panels::scale;
use crate::panels::theme::{self, hsla};
use crate::panels::widgets::{button, spinner};

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

    pub(crate) fn open_turn_review(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if !self.turn_reviews.iter().any(|turn| turn.id == id) {
            return;
        }
        self.review = Some(Review {
            id, snapshot: None, files: None, restore_confirm: false, restoring: false, kept: HashSet::new(),
            comments: Vec::new(), commenting: None, error: None,
        });
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
        self.review = Some(Review { id: Uuid::new_v4(), restore_confirm: false, restoring: false, snapshot: Some(snapshot), files: None, kept: HashSet::new(), comments: Vec::new(), commenting: None, error: None });
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

    fn keep_hunk(&mut self, hunk: &Hunk, cx: &mut Context<Self>) {
        if let Some(review) = &mut self.review {
            review.kept.insert(hunk.patch.clone());
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
        let Some(review) = self.review.as_mut().filter(|review| review.id == id && !review.restoring && review.files.is_some()) else { return };
        if !review.restore_confirm {
            review.restore_confirm = true;
            cx.notify();
            return;
        }
        review.restore_confirm = false;
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
                    Err(error) => review.error = Some(error),
                }
                v.send_next(cx);
                cx.notify();
            });
        }).detach();
        cx.notify();
    }

    pub(super) fn render_review(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let review = self.review.as_ref()?;
        let mut body = self.review_scroll.wire(div().id("thread-review").flex_1(), window).px(px(18.0)).pt(px(12.0)).flex().flex_col().gap(px(12.0));
        if let Some(why) = &review.error {
            body = body.child(div().text_color(hsla(theme::DANGER)).child(why.clone()));
        }
        match &review.files {
            None => body = body.child(div().flex().gap(px(8.0)).items_center().text_color(hsla(theme::TEXT_DIM)).child(spinner("review-reading", 12.0)).child("Reading the changes…")),
            Some(files) if files.is_empty() && review.error.is_none() => body = body.child(div().text_color(hsla(theme::TEXT_DIM)).child("Nothing changed since your last message.")),
            Some(files) => {
                for (f, file) in files.iter().enumerate() {
                    body = body.child(self.render_file(f, file, review, cx));
                }
            }
        }
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
                )).when(!review.restoring && review.files.is_some() && self.can_restore(turn), |footer| {
                    let label = if review.restore_confirm { "Confirm restore" } else { "Restore Checkpoint" };
                    footer.child(button("review-restore", label, review.restore_confirm)
                        .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.restore_turn(id, cx))))
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
        Some(div().flex_1().flex().flex_col().min_h_0().child(body).child(footer))
    }

    fn render_file(&self, f: usize, file: &FileChange, review: &Review, cx: &mut Context<Self>) -> Div {
        let mut block = div().flex().flex_col().gap(px(6.0));
        block = block.child(div().font_weight(FontWeight::SEMIBOLD).child(file.path.display().to_string()));
        if file.hunks.is_empty() {
            block = block.child(div().text_color(hsla(theme::TEXT_DIM)).child("No textual diff"));
        }
        for (h, hunk) in file.hunks.iter().enumerate() {
            block = block.child(self.render_hunk(f, h, &file.path, hunk, review, cx));
        }
        block
    }

    fn render_hunk(&self, f: usize, h: usize, path: &Path, hunk: &Hunk, review: &Review, cx: &mut Context<Self>) -> Div {
        let kept = review.kept.contains(&hunk.patch);
        let (keep, revert) = (hunk.clone(), hunk.clone());
        let header = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(8.0))
            .py(px(3.0))
            .bg(hsla(theme::APP_BG))
            .child(div().flex_1().text_color(hsla(theme::TEXT_FAINT)).child(hunk.header.clone()))
            .when(review.snapshot.is_some(), |header| header.child(if kept {
                div().text_color(hsla(theme::OK)).child("Kept").into_any_element()
            } else {
                button(SharedString::from(format!("review-keep-{f}-{h}")), "Keep", false).on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.keep_hunk(&keep, cx))).into_any_element()
            })
            .child(button(SharedString::from(format!("review-revert-{f}-{h}")), "Revert", false).on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.revert_hunk(revert.clone(), cx)))));
        let mut block = div().font_family(crate::platform::monospace_font()).text_size(scale::px(12.5)).rounded(px(4.0)).border_1().border_color(hsla(if kept { theme::OK } else { theme::PANEL_BORDER })).flex().flex_col().overflow_hidden().child(header);
        // A kept hunk folds to its header.
        if kept {
            return block;
        }
        for (l, line) in hunk.lines.iter().enumerate() {
            let (bg, fg) = match line.origin {
                '+' => (theme::DIFF_ADDED_BG, theme::OK),
                '-' => (theme::DIFF_REMOVED_BG, theme::DANGER),
                _ => (theme::PANEL_BG, theme::TEXT_DIM),
            };
            let number = line.new_line.map_or(String::new(), |n| n.to_string());
            let comment = review.snapshot.is_some().then(|| {
                if let Some(n) = line.new_line {
                    let path = path.to_path_buf();
                    comment_trigger(format!("review-comment-{f}-{h}-{l}"))
                        .opacity(0.65)
                        .group_hover("review-diff-line", |style| style.opacity(1.0))
                        .focus_visible(|style| style.opacity(1.0))
                        .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| v.start_comment(path.clone(), n, window, cx)))
                        .into_any_element()
                } else {
                    div().size(px(COMMENT_SIZE)).into_any_element()
                }
            });
            let row = div()
                .id(SharedString::from(format!("review-line-{f}-{h}-{l}")))
                .group("review-diff-line")
                .flex()
                .bg(hsla(bg))
                .text_color(hsla(fg))
                .whitespace_nowrap()
                .items_center()
                .children(comment)
                .child(div().w(px(44.0)).flex_none().pr(px(8.0)).text_right().text_color(hsla(theme::TEXT_FAINT)).child(number))
                .child(div().child(format!("{} {}", line.origin, line.text)));
            block = block.child(row);
            if review.snapshot.is_some() && let Some(n) = line.new_line {
                for c in review.comments.iter().filter(|c| c.path == path && c.line == n) {
                    block = block.child(div().ml(px(44.0 + COMMENT_SIZE)).px(px(8.0)).py(px(3.0)).text_color(hsla(theme::ACCENT)).child(c.text.clone()));
                }
                if let Some((_, input, _)) = review.commenting.as_ref().filter(|(c, _, _)| c.path == path && c.line == n) {
                    block = block.child(
                        div()
                            .ml(px(44.0 + COMMENT_SIZE))
                            .p(px(4.0))
                            .on_key_down(cx.listener(|v, ev: &KeyDownEvent, window, cx| {
                                if ev.keystroke.key == "escape" {
                                    v.finish_comment(String::new(), window, cx);
                                    cx.stop_propagation();
                                }
                            }))
                            .child(comment_box(input, "keeps it", "sends all")),
                    );
                }
            }
        }
        block
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

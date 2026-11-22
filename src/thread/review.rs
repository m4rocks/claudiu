//! Reviewing what the agent changed since the last message, hunk by hunk, as in a pull request: keep a hunk, put it
//! back as it was (`git/snapshot.rs`), or comment on one of its lines. The comments go back to the agent as one
//! message, each naming its line as `@path#L12`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gpui::{Subscription, ClickEvent, Context, Div, Entity, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px};
use gpui_kit::component::input::{InputEvent, InputState};

use super::ThreadView;
use crate::git::snapshot::{self, FileChange, Hunk};
use crate::panels::comment_box::{comment_box, comment_input};
use crate::panels::scale;
use crate::panels::theme::{self, hsla};
use crate::panels::widgets::{button, spinner};

pub struct Review {
    /// The snapshot the changes are measured from: the one taken before the last message.
    snapshot: String,
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
            let read = cx.background_executor().spawn(async move { snapshot::changes(&cwd, &snapshot) }).await;
            let _ = this.update(cx, |v, cx| {
                v.changed = Some(read.is_ok_and(|files| !files.is_empty()));
                cx.notify();
            });
        })
        .detach();
    }

    pub fn open_review(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshots.last().cloned() else { return };
        self.review = Some(Review { snapshot, files: None, kept: HashSet::new(), comments: Vec::new(), commenting: None, error: None });
        self.reload_review(cx);
    }

    fn reload_review(&mut self, cx: &mut Context<Self>) {
        let Some(review) = &self.review else { return };
        let (cwd, snapshot) = (self.cwd.clone(), review.snapshot.clone());
        cx.spawn(async move |this, cx| {
            let read = cx.background_executor().spawn(async move { snapshot::changes(&cwd, &snapshot) }).await;
            let _ = this.update(cx, |v, cx| {
                if let Some(review) = &mut v.review {
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

    pub(super) fn render_review(&self, cx: &mut Context<Self>) -> Option<Div> {
        let review = self.review.as_ref()?;
        let mut body = div().id("thread-review").flex_1().overflow_y_scroll().px(px(18.0)).pt(px(12.0)).flex().flex_col().gap(px(12.0));
        if let Some(why) = &review.error {
            body = body.child(div().text_color(hsla(theme::DANGER)).child(why.clone()));
        }
        match &review.files {
            None => body = body.child(div().flex().gap(px(8.0)).items_center().text_color(hsla(theme::TEXT_DIM)).child(spinner("review-reading", 12.0)).child("Reading the changes…")),
            Some(files) if files.is_empty() => body = body.child(div().text_color(hsla(theme::TEXT_DIM)).child("Nothing changed since your last message.")),
            Some(files) => {
                for (f, file) in files.iter().enumerate() {
                    body = body.child(self.render_file(f, file, review, cx));
                }
            }
        }
        let count = review.comments.len();
        let footer = div()
            .flex_none()
            .px(px(18.0))
            .py(px(10.0))
            .flex()
            .gap(px(8.0))
            .items_center()
            .border_t_1()
            .border_color(hsla(theme::PANEL_BORDER))
            .child(div().flex_1().text_color(hsla(theme::TEXT_DIM)).child("Changes since your last message"))
            .child(button("review-close", "Close", false).on_click(cx.listener(|v, _: &ClickEvent, _, cx| v.close_review(cx))))
            .when(count > 0, |d| {
                let label = if count == 1 { "Send 1 comment".to_string() } else { format!("Send {count} comments") };
                d.child(button("review-send", label, true).on_click(cx.listener(|v, _: &ClickEvent, window, cx| v.send_comments(window, cx))))
            });
        Some(div().flex_1().flex().flex_col().min_h_0().child(body).child(footer))
    }

    fn render_file(&self, f: usize, file: &FileChange, review: &Review, cx: &mut Context<Self>) -> Div {
        let mut block = div().flex().flex_col().gap(px(6.0));
        block = block.child(div().font_weight(FontWeight::SEMIBOLD).child(file.path.display().to_string()));
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
            .child(if kept {
                div().text_color(hsla(theme::OK)).child("Kept").into_any_element()
            } else {
                button(SharedString::from(format!("review-keep-{f}-{h}")), "Keep", false).on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.keep_hunk(&keep, cx))).into_any_element()
            })
            .child(button(SharedString::from(format!("review-revert-{f}-{h}")), "Revert", false).on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.revert_hunk(revert.clone(), cx))));
        let mut block = div().font_family(crate::platform::monospace_font()).text_size(scale::px(12.5)).rounded(px(4.0)).border_1().border_color(hsla(if kept { theme::OK } else { theme::PANEL_BORDER })).flex().flex_col().overflow_hidden().child(header);
        // A kept hunk folds to its header.
        if kept {
            return block;
        }
        for (l, line) in hunk.lines.iter().enumerate() {
            let (bg, fg) = match line.origin {
                '+' => (0x16301e, theme::OK),
                '-' => (0x3a1a1d, theme::DANGER),
                _ => (theme::PANEL_BG, theme::TEXT_DIM),
            };
            let number = line.new_line.map_or(String::new(), |n| n.to_string());
            let mut row = div()
                .id(SharedString::from(format!("review-line-{f}-{h}-{l}")))
                .flex()
                .bg(hsla(bg))
                .text_color(hsla(fg))
                .whitespace_nowrap()
                .child(div().w(px(44.0)).flex_none().pr(px(8.0)).text_right().text_color(hsla(theme::TEXT_FAINT)).child(number))
                .child(div().child(format!("{} {}", line.origin, line.text)));
            // Comments are on lines of the file as it is now, so a removed line takes none.
            if let Some(n) = line.new_line {
                let path = path.to_path_buf();
                row = row.cursor_pointer().hover(|s| s.bg(hsla(theme::ROW_HOVER))).on_click(cx.listener(move |v, _: &ClickEvent, window, cx| v.start_comment(path.clone(), n, window, cx)));
            }
            block = block.child(row);
            if let Some(n) = line.new_line {
                for c in review.comments.iter().filter(|c| c.path == path && c.line == n) {
                    block = block.child(div().ml(px(44.0)).px(px(8.0)).py(px(3.0)).text_color(hsla(theme::ACCENT)).child(c.text.clone()));
                }
                if let Some((_, input, _)) = review.commenting.as_ref().filter(|(c, _, _)| c.path == path && c.line == n) {
                    block = block.child(
                        div()
                            .ml(px(44.0))
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

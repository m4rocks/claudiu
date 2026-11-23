use gpui::{AppContext, ClickEvent, Context, Div, Focusable, Keystroke, ParentElement, Styled, Subscription, Window, div, px};
use gpui_kit::component::input::{Input, InputEvent, InputState};

use super::ThreadView;
use crate::harness::thread::{Entry, ToolContent, parse_subagent_report};
use crate::panels::theme::{self, hsla};
use crate::panels::widgets::{button, button_disabled};

pub(super) struct Find {
    input: gpui::Entity<InputState>,
    _subscription: Subscription,
    matches: Vec<usize>,
    selected: usize,
    query: String,
}

fn matching_rows(entries: &[Entry], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return Vec::new();
    }
    let contains = |text: &str| text.to_lowercase().contains(query);
    entries.iter().enumerate().filter_map(|(row, entry)| {
        let matches = match entry {
            Entry::User(text) => parse_subagent_report(text).map_or_else(|| contains(text), |report| contains(report.body) || contains(report.title)),
            Entry::Agent(text) | Entry::Thought(text) | Entry::Notice { text, .. } => contains(text),
            Entry::Tool(tool) => contains(&tool.title) || tool.content.iter().any(|part| {
                matches!(part, ToolContent::Text(text) if contains(text))
            }),
        };
        matches.then_some(row)
    }).collect()
}

impl ThreadView {
    pub(crate) fn can_find(&self) -> bool {
        self.review.is_none() && self.screenshot.is_none()
    }

    pub(crate) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_find() {
            return;
        }
        if self.picker.is_some() {
            self.close_picker(window, cx);
        }
        if let Some(find) = &self.find {
            find.input.update(cx, |input, cx| input.focus(window, cx));
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in session"));
        let subscription = cx.subscribe_in(&input, window, |v: &mut Self, input, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) {
                if let Some(find) = &mut v.find {
                    find.query = input.read(cx).value().trim().to_lowercase();
                    find.matches.clear();
                }
                v.refresh_find(0);
                v.reveal_find(cx);
            }
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        self.find = Some(Find { input, _subscription: subscription, matches: Vec::new(), selected: 0, query: String::new() });
        cx.notify();
    }

    pub(super) fn intercept_find(&mut self, key: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.can_find() || self.picker.is_some() {
            return false;
        }
        let modifiers = key.modifiers;
        let focused = self.find.as_ref().is_some_and(|find| find.input.focus_handle(cx).is_focused(window));
        if key.key == "f" && modifiers.secondary() && !modifiers.shift && !modifiers.alt
            && (focused || self.composer.focus_handle(cx).is_focused(window))
        {
            self.open_find(window, cx);
            return true;
        }
        if !focused || modifiers.alt || modifiers.control || modifiers.platform {
            return false;
        }
        match key.key.as_str() {
            "escape" => self.close_find(window, cx),
            "enter" => self.step_find(modifiers.shift, cx),
            _ => return false,
        }
        true
    }

    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find = None;
        self.focus_composer(window, cx);
        cx.notify();
    }

    pub(super) fn refresh_find(&mut self, from: usize) {
        let Some(find) = &mut self.find else { return };
        let old_row = find.matches.get(find.selected).copied();
        find.matches.truncate(find.matches.partition_point(|row| *row < from));
        find.matches.extend(matching_rows(&self.thread.entries[from..], &find.query).into_iter().map(|row| from + row));
        find.selected = old_row.and_then(|row| find.matches.binary_search(&row).ok()).unwrap_or(0);
    }

    fn step_find(&mut self, previous: bool, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else { return };
        let len = find.matches.len();
        if len > 0 {
            find.selected = if previous { (find.selected + len - 1) % len } else { (find.selected + 1) % len };
            self.reveal_find(cx);
        }
    }

    fn reveal_find(&mut self, cx: &mut Context<Self>) {
        if let Some(row) = self.find.as_ref().and_then(|find| find.matches.get(find.selected).copied()) {
            self.chat_glide.cancel();
            self.list.pause_following_tail();
            self.reveal_timeline_row(row, cx);
            self.list.scroll_to(gpui::ListOffset { item_ix: row, offset_in_item: px(0.0) });
        }
        cx.notify();
    }

    pub(super) fn find_highlights(&self, row: usize) -> bool {
        self.find.as_ref().is_some_and(|find| find.matches.binary_search(&row).is_ok())
    }

    pub(super) fn render_find(&self, cx: &mut Context<Self>) -> Option<Div> {
        let find = self.find.as_ref()?;
        let count = find.matches.len();
        let label = if count == 0 { "0 matches".to_string() } else { format!("{} of {count}", find.selected + 1) };
        let arrows = [(true, "Previous"), (false, "Next")].into_iter().map(|(previous, label)| {
            if count == 0 {
                button_disabled(label, label)
            } else {
                button(label, label, false).on_click(cx.listener(move |v, _: &ClickEvent, _, cx| v.step_find(previous, cx)))
            }
        });
        Some(div().flex().items_center().gap(px(6.0)).p(px(8.0))
            .bg(hsla(theme::PANEL_BG)).border_b_1().border_color(hsla(theme::PANEL_BORDER))
            .child(div().flex_1().child(Input::new(&find.input)))
            .child(div().text_color(hsla(theme::TEXT_DIM)).child(label))
            .children(arrows)
            .child(button("thread-find-close", "Close", false)
                .on_click(cx.listener(|v, _: &ClickEvent, window, cx| v.close_find(window, cx)))))
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_matches_text_and_tool_results_case_insensitively() {
        let entries = [Entry::Agent("Done".into()), Entry::User("Find DONE here".into()), Entry::Thought("Proceed".into())];
        assert_eq!(matching_rows(&entries, "done"), [0, 1]);
        assert!(matching_rows(&entries, "").is_empty());
    }
}

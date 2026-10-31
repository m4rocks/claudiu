//! A pull request's changed files and their diffs.

use super::*;

impl GithubView {
    pub(super) fn render_files(&self, ds: &DetailState, cx: &mut Context<Self>) -> Div {
        if let Some(e) = &ds.files_error {
            return div().text_color(hsla(theme::DANGER)).child(e.clone());
        }
        let Some(files) = &ds.files else { return div().child(dim("Loading files…")) };
        let mut col = div().flex().flex_col().gap(px(10.0));
        col = col.child(dim(format!(
            "{} · +{} −{}",
            plural(files.len(), "file", "files"),
            files.iter().map(|f| f.additions).sum::<u32>(),
            files.iter().map(|f| f.deletions).sum::<u32>()
        )));
        for (n, f) in files.iter().enumerate() {
            let open = ds.expanded.contains(&f.path);
            let head = div()
                .id(SharedString::from(format!("gh-file-{n}")))
                .flex()
                .items_center()
                .gap(px(8.0))
                .h(px(32.0))
                .px(px(12.0))
                .bg(hsla(CARD_HEAD))
                .cursor_pointer()
                .hover(|s| s.bg(hsla(0x1c1c1d)))
                .child(div().text_color(hsla(theme::TEXT_FAINT)).child(icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 12.0)))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().font_family(monospace_font()).text_size(crate::panels::scale::px(12.0)).child(f.path.clone()))
                .children((f.status != "modified").then(|| faint(f.status.clone())))
                .child(div().text_size(crate::panels::scale::px(11.5)).text_color(hsla(GREEN)).child(format!("+{}", f.additions)))
                .child(div().text_size(crate::panels::scale::px(11.5)).text_color(hsla(RED)).child(format!("−{}", f.deletions)))
                .on_click(go(cx, GhAct::ToggleFile(f.path.clone())));
            let mut card = div().rounded(px(8.0)).border_1().border_color(hsla(CARD_BORDER)).overflow_hidden().child(head);
            if open {
                card = card.child(self.render_diff(f, ds.full.contains(&f.path), n, cx));
            }
            col = col.child(card);
        }
        col
    }

    pub(super) fn render_diff(&self, f: &FileDiff, full: bool, n: usize, cx: &mut Context<Self>) -> Div {
        const SHOWN: usize = 400;
        if f.no_patch {
            return div().p(px(12.0)).child(faint("No inline diff for this file (binary or too large). Open on GitHub to view it."));
        }
        let mut lines = div().flex().flex_col().font_family(monospace_font()).text_size(crate::panels::scale::px(11.5)).py(px(4.0));
        for l in f.lines.iter().take(if full { usize::MAX } else { SHOWN }) {
            let (bg, sign) = match l.kind {
                DiffKind::Add => (Some(GREEN), "+"),
                DiffKind::Del => (Some(RED), "-"),
                DiffKind::Hunk => (Some(0x58a6ff), ""),
                DiffKind::Ctx => (None, " "),
            };
            let num = |v: Option<u32>| div().w(px(40.0)).flex_none().text_align(TextAlign::Right).pr(px(6.0)).text_color(hsla(theme::TEXT_FAINT)).child(v.map(|v| v.to_string()).unwrap_or_default());
            lines = lines.child(
                div()
                    .flex()
                    .when_some(bg, |d, c| d.bg(hsla(c).opacity(if l.kind == DiffKind::Hunk { 0.08 } else { 0.14 })))
                    .child(num(l.old))
                    .child(num(l.new))
                    .child(div().w(px(12.0)).flex_none().text_color(hsla(theme::TEXT_DIM)).child(sign))
                    .child(div().whitespace_nowrap().child(if l.kind == DiffKind::Hunk { l.text.clone() } else if l.text.is_empty() { " ".to_string() } else { l.text.clone() })),
            );
        }
        let hidden = f.lines.len().saturating_sub(SHOWN);
        div()
            .child(div().id(SharedString::from(format!("gh-diff-{n}"))).overflow_x_scroll().child(lines))
            .when(!full && hidden > 0, |d| {
                d.child(div().p(px(8.0)).child(button(SharedString::from(format!("gh-diff-all-{n}")), format!("Show {hidden} more lines"), false).on_click(go(cx, GhAct::ShowAllLines(f.path.clone())))))
            })
    }

    // -------------------------------------------------------------------------------------------------- new item
}

//! A pull request's changed files and their diffs.

use super::*;

impl GithubView {
    pub(super) fn render_files(&self, ds: &DetailState) -> El {
        if let Some(e) = &ds.files_error {
            return label(e.clone(), 12.0, theme::DANGER).wrap();
        }
        let Some(files) = &ds.files else { return row().gap(8.0).center_y().child(spinner(12.0, theme::TEXT_DIM)).child(dim("Loading files…")) };
        let mut c = col().w_full().gap(10.0).child(dim(format!(
            "{} · +{} −{}",
            plural(files.len(), "file", "files"),
            files.iter().map(|f| f.additions).sum::<u32>(),
            files.iter().map(|f| f.deletions).sum::<u32>()
        )));
        for (n, f) in files.iter().enumerate() {
            let open = ds.expanded.contains(&f.path);
            let mut head = row()
                .key(format!("gh-file-{n}"))
                .w_full()
                .h(32.0)
                .px(12.0)
                .gap(8.0)
                .center_y()
                .fill(rgb(CARD_HEAD))
                .cursor(Cursor::Pointer)
                .role("button")
                .hover(Color::WHITE.alpha(0.03))
                .child(icon(&icons::CHEVRON_RIGHT, 12.0, theme::TEXT_FAINT).rotate(if open { 90.0 } else { 0.0 }).motion(motion::SNAP))
                .child(mono(f.path.clone(), 12.0, theme::TEXT).truncate_middle().grow(1.0).shrink());
            if f.status != "modified" {
                head = head.child(faint(f.status.clone()));
            }
            head = head
                .child(label(format!("+{}", f.additions), 11.5, GREEN))
                .child(label(format!("−{}", f.deletions), 11.5, RED))
                .on_press_tx(go(&self.id, GhAct::ToggleFile(f.path.clone())));
            let mut card = col().key(format!("gh-file-card-{n}")).w_full().radius(8.0).border(1.0, rgb(CARD_BORDER)).clip_rounded(8.0).motion(motion::REFLOW).child(head);
            if open {
                card = card.child(self.render_diff(f, ds.full.contains(&f.path), n));
            }
            c = c.child(card);
        }
        c
    }

    pub(super) fn render_diff(&self, f: &FileDiff, full: bool, n: usize) -> El {
        const SHOWN: usize = 400;
        if f.no_patch {
            return el().padding(12.0).child(faint("No inline diff for this file (binary or too large). Open on GitHub to view it.").wrap());
        }
        let mut lines = col().py(4.0);
        for l in f.lines.iter().take(if full { usize::MAX } else { SHOWN }) {
            let (bg, sign) = match l.kind {
                DiffKind::Add => (Some(GREEN), "+"),
                DiffKind::Del => (Some(RED), "-"),
                DiffKind::Hunk => (Some(0x58a6ff), ""),
                DiffKind::Ctx => (None, " "),
            };
            let num = |v: Option<u32>| row().w(40.0).justify_end().pr(6.0).child(mono(v.map(|v| v.to_string()).unwrap_or_default(), 11.5, theme::TEXT_FAINT));
            let body = if l.text.is_empty() { " ".to_string() } else { l.text.clone() };
            let mut r = row().child(num(l.old)).child(num(l.new)).child(row().w(12.0).child(mono(sign, 11.5, theme::TEXT_DIM))).child(mono(body, 11.5, theme::TEXT));
            if let Some(c) = bg {
                r = r.fill(rgb(c).alpha(if l.kind == DiffKind::Hunk { 0.08 } else { 0.14 }));
            }
            lines = lines.child(r);
        }
        let hidden = f.lines.len().saturating_sub(SHOWN);
        let mut c = col().w_full().child(row().key(format!("gh-diff-{n}")).w_full().scroll_x().child(lines));
        if !full && hidden > 0 {
            c = c.child(el().padding(8.0).child(button(format!("Show {hidden} more lines"), false).key(format!("gh-diff-all-{n}")).on_press_tx(go(&self.id, GhAct::ShowAllLines(f.path.clone())))));
        }
        c
    }
}

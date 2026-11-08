//! What draws above the workspace: the open menu, the modal dialog and the toast. Each pops in with a short spring
//! and fades away when it closes.

use rune::{Align, Color, Cursor, El, Side, Transition, Tx, col, el, row, text, text_input};

use crate::app::{Act, MenuAnchor, Modal, Workspace, modal_field};
use crate::panels::icons;
use crate::panels::theme::{self, rgb};
use crate::panels::widgets::{button, danger_button, font, icon, icon_button, label};

const MENU_W: f32 = 232.0;

fn act(ws: &mut Workspace, a: Act, tx: &mut Tx) {
    ws.act(a);
    ws.after_input(tx);
}

/// A text field of a dialog: field `n` of the open modal (see `modal_field`).
pub fn modal_input(n: usize, edit: &rune::TextEdit, placeholder: &str) -> El {
    let field = text_input(edit, font(13.0, 400.0))
        .key(format!("modal-field-{n}"))
        .single_line()
        .placeholder(placeholder.to_string(), rgb(theme::TEXT_FAINT))
        .color(rgb(theme::TEXT))
        .caret(rgb(theme::ACCENT), rgb(theme::SELECTION))
        .w_full()
        .on_edit(move |ws: &mut Workspace| match modal_field(&mut ws.modal, n) {
            Some(f) => f,
            // The modal closed between frames; a scratch field takes the edit.
            None => &mut ws.search,
        })
        .on_submit(|ws: &mut Workspace, tx: &mut Tx| {
            ws.submit_modal();
            ws.after_input(tx);
        });
    row()
        .key(format!("modal-box-{n}"))
        .h(30.0)
        .w_full()
        .px(9.0)
        .center_y()
        .radius(6.0)
        .fill(rgb(0x101012))
        .border(1.0, rgb(if edit.focused { theme::ACCENT } else { 0x2a2a2e }))
        .motion(rune::tok::motion::SNAP)
        .child(field)
}

impl Workspace {
    pub(crate) fn overlays(&self) -> Vec<El> {
        let mut out = vec![];
        if let Some(m) = self.render_modal() {
            out.push(m);
        }
        if let Some(m) = self.render_menu() {
            out.push(m);
        }
        if let Some(t) = self.render_toast() {
            out.push(t);
        }
        out
    }

    fn render_menu(&self) -> Option<El> {
        let menu = self.menu.as_ref()?;
        let mut card = col()
            .key(format!("menu-{}", menu.serial))
            .w(MENU_W.max(menu.min_w))
            .padding(4.0)
            .radius(9.0)
            .fill(rgb(0x18181a))
            .border(1.0, rgb(0x2c2c30))
            .shadow(10.0, 28.0, Color::BLACK.alpha(0.5))
            .role("menu")
            .transition(Transition::scale(0.96).combined(Transition::opacity()))
            .modal(true, |ws: &mut Workspace| ws.menu = None);
        for (i, item) in menu.items.iter().enumerate() {
            if item.separator_before {
                card = card.child(el().h(1.0).w_full().my(4.0).fill(rgb(theme::BORDER)));
            }
            let color = if item.disabled { theme::TEXT_FAINT } else if item.danger { theme::DANGER } else { theme::TEXT };
            let mut words = col().grow(1.0).shrink().child(label(item.label.clone(), 12.5, color).truncate());
            if let Some(d) = &item.detail {
                words = words.child(label(d.clone(), 11.0, theme::TEXT_FAINT).truncate());
            }
            let mut r = row()
                .key(format!("menu-{}-{i}", menu.serial))
                .h(if item.detail.is_some() { 42.0 } else { 28.0 })
                .w_full()
                .px(10.0)
                .gap(8.0)
                .center_y()
                .radius(5.0)
                .role("menuitem");
            if let Some(glyph) = item.icon {
                r = r.child(icon(glyph, 13.0, if item.disabled { theme::TEXT_FAINT } else { theme::TEXT_DIM }));
            }
            r = r.child(words);
            if let Some(h) = &item.hint {
                r = r.child(label(h.clone(), 11.0, theme::TEXT_FAINT));
            }
            if let Some(remove) = item.remove.clone() {
                r = r.hover_group().child(
                    icon_button(&icons::X)
                        .key(format!("menu-remove-{}-{i}", menu.serial))
                        .group_opacity(0.0, 1.0)
                        .on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| act(ws, remove.clone(), tx)),
                );
            }
            if !item.disabled {
                let a = item.act.clone();
                r = r.cursor(Cursor::Pointer).hover(Color::WHITE.alpha(0.06)).on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| act(ws, a.clone(), tx));
            }
            card = card.child(r);
        }
        Some(match &menu.anchor {
            MenuAnchor::Key(k) => card.anchored(k.clone(), Side::Bottom, Align::Start, 4.0),
            MenuAnchor::Point(x, y) => {
                // Kept inside the window: a menu opened near the right or bottom edge opens the other way.
                let w = MENU_W.max(menu.min_w);
                let h: f32 = menu.items.iter().map(|i| if i.detail.is_some() { 42.0 } else { 28.0 } + if i.separator_before { 9.0 } else { 0.0 }).sum::<f32>() + 10.0;
                let (ww, wh) = self.window_size;
                let left = if x + w + 8.0 > ww { (x - w).max(8.0) } else { *x };
                let top = if y + h + 8.0 > wh { (y - h).max(8.0) } else { *y };
                card.absolute(left, top)
            }
        })
    }

    fn render_modal(&self) -> Option<El> {
        let modal = self.modal.as_ref()?;
        let settings = matches!(modal, Modal::Settings { .. });
        let (title, body, confirm_label, danger): (String, El, String, bool) = match modal {
            Modal::Confirm { title, body, confirm, danger, .. } => (title.clone(), label(body.clone(), 13.0, theme::TEXT_DIM).wrap(), confirm.clone(), *danger),
            Modal::Settings { .. } => ("Settings".into(), self.settings_body(), "Save".into(), false),
            Modal::Advisor { input, .. } => (
                "Advisor model".into(),
                col()
                    .gap(8.0)
                    .child(label("A model alias or id, such as opus, sonnet or claude-opus-5-5, or “off”. Claude Code saves it as your default.", 12.0, theme::TEXT_DIM).wrap())
                    .child(modal_input(0, input, "opus, sonnet, off, or a model id")),
                "Set".into(),
                false,
            ),
            Modal::NewWorktree { input, base, .. } => {
                use crate::git::worktree::Base;
                let choice = |key: &'static str, l: &'static str, this: Base| {
                    button(l, *base == this).key(key).on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| act(ws, Act::PickBase(this), tx))
                };
                (
                    "New worktree".into(),
                    col()
                        .gap(10.0)
                        .child(label("The session moves into it and keeps its history. Claudiu makes a folder for it in its own data folder, outside your repository, with a new branch of this name.", 12.0, theme::TEXT_DIM).wrap())
                        .child(modal_input(0, input, "Leave empty for a generated name"))
                        .child(row().center_y().gap(6.0).child(label("Start from", 12.0, theme::TEXT_FAINT)).child(choice("base-main", "Main", Base::Main)).child(choice("base-current", "Current branch", Base::Current))),
                    "Create and switch".into(),
                    false,
                )
            }
            Modal::Rename { input, .. } => ("Rename session".into(), modal_input(0, input, ""), "Rename".into(), false),
            Modal::GoToLine { input, .. } => ("Go to line".into(), modal_input(0, input, "Line, or line:column"), "Go".into(), false),
            Modal::NewBranch { input, .. } => ("New branch".into(), modal_input(0, input, "Branch name"), "Create".into(), false),
        };
        let ok = if danger { danger_button(confirm_label) } else { button(confirm_label, true) };
        let mut card = col()
            .key("modal-card")
            .padding(20.0)
            .gap(12.0)
            .radius(12.0)
            .fill(rgb(0x151517))
            .border(1.0, rgb(0x2c2c30))
            .shadow(16.0, 48.0, Color::BLACK.alpha(0.55))
            .role("dialog")
            .transition(Transition::scale(0.96).combined(Transition::opacity()))
            .child(text(title).font(font(15.0, 600.0)).color(rgb(theme::TEXT)))
            .child(body)
            .child(
                row()
                    .w_full()
                    .justify_end()
                    .gap(8.0)
                    .mt(4.0)
                    .child(button("Cancel", false).key("modal-cancel").on_press_tx(|ws: &mut Workspace, tx: &mut Tx| act(ws, Act::Dismiss, tx)))
                    .child(ok.key("modal-ok").on_press_tx(|ws: &mut Workspace, tx: &mut Tx| {
                        ws.submit_modal();
                        ws.after_input(tx);
                    })),
            );
        card = if settings {
            // One size for every page, growing with Scale.
            card.w(crate::panels::scale::px(720.0)).h(crate::panels::scale::px(480.0))
        } else {
            card.w(420.0)
        };
        Some(
            row()
                .key("modal-backdrop")
                .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
                .center()
                .fill(Color::BLACK.alpha(0.62))
                .transition(Transition::opacity())
                .modal(true, |ws: &mut Workspace| {
                    ws.modal = None;
                    ws.focus_active();
                })
                .child(card),
        )
    }

    fn render_toast(&self) -> Option<El> {
        let t = self.toast.as_ref()?;
        let mut card = col()
            .key(format!("toast-{}", t.serial))
            .max_w(420.0)
            .px(14.0)
            .py(10.0)
            .radius(9.0)
            .fill(rgb(0x1a1a1c))
            .border(1.0, rgb(if t.error { 0x5a2327 } else { 0x2c2c30 }))
            .shadow(8.0, 24.0, Color::BLACK.alpha(0.45))
            .role("status")
            .transition(Transition::offset(0.0, 10.0).combined(Transition::opacity()))
            .child(
                row()
                    .gap(8.0)
                    .child(icon(if t.error { &icons::CIRCLE_X } else { &icons::CIRCLE_CHECK }, 14.0, if t.error { theme::DANGER } else { theme::OK }))
                    .child(label(t.text.clone(), 12.5, if t.error { theme::DANGER } else { theme::TEXT }).wrap().shrink()),
            );
        if let Some((l, a)) = t.action.clone() {
            card = card.child(
                row()
                    .mt(8.0)
                    .gap(6.0)
                    .child(button(l, true).key("toast-action").on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
                        ws.toast = None;
                        act(ws, a.clone(), tx)
                    }))
                    .child(button("Dismiss", false).key("toast-dismiss").on_press_tx(|ws: &mut Workspace, tx: &mut Tx| {
                        ws.toast = None;
                        ws.after_input(tx);
                    })),
            );
        }
        Some(row().key("toast-slot").pin(None, None, Some(18.0), Some(18.0)).child(card))
    }
}

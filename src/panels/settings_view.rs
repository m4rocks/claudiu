//! The Settings dialog's body: the category list on the left and the chosen page on the right.

use rune::tok::motion;
use rune::{Color, Cursor, El, ScrollSpec, Tx, col, el, row};

use crate::app::{Act, MenuAnchor, MenuItem, Modal, SettingsPage, Workspace};
use crate::claude::defaults::{Effort, PermissionMode};
use crate::claude::mcp::Feature;
use crate::claude::trim::Trim;
use crate::panels::overlays::modal_input;
use crate::panels::theme::{self, rgb};
use crate::panels::widgets::{button, danger_button, label, section_label, tick};

fn faint(t: impl Into<String>) -> El {
    label(t, 11.0, theme::TEXT_FAINT).wrap()
}

/// A labelled text field, field `n` of the modal.
fn field(n: usize, title: &str, edit: &rune::TextEdit, hint: &str) -> El {
    col().gap(4.0).w_full().child(section_label(title.to_string())).child(modal_input(n, edit, hint))
}

/// A row of the settings pages: dark, rounded, its label at the left.
fn setting_row(key: String) -> El {
    row().key(key).h(32.0).w_full().px(10.0).center_y().justify_between().radius(6.0).fill(rgb(0x111113)).border(1.0, rgb(0x242428))
}

/// An on/off switch whose knob slides across.
fn switch(on: bool) -> El {
    row()
        .w(28.0)
        .h(16.0)
        .padding(2.0)
        .radius(8.0)
        .fill(rgb(if on { theme::ACCENT } else { 0x2c2c30 }))
        .justify_start()
        .motion(motion::SNAP)
        .child(el().size(12.0, 12.0).radius(6.0).fill(rgb(if on { 0x17100d } else { 0x8a8a8a })).ml(if on { 12.0 } else { 0.0 }).motion(motion::SNAP))
}

/// An On/Off row that runs `flip` on the workspace when clicked, with an optional faint `tag` after the label.
fn switch_row(key: String, title: &str, tag: Option<&str>, on: bool, flip: impl Fn(&mut Workspace) + 'static) -> El {
    let mut left = row().center_y().gap(8.0).child(label(title.to_string(), 13.0, theme::TEXT));
    if let Some(t) = tag {
        left = left.child(label(t.to_string(), 11.0, theme::TEXT_FAINT));
    }
    setting_row(key)
        .cursor(Cursor::Pointer)
        .role("switch")
        .hover(Color::WHITE.alpha(0.02))
        .child(left)
        .child(switch(on))
        .on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
            flip(ws);
            ws.after_input(tx);
        })
}

/// An On/Off row in the Settings modal; `field` picks the flag it flips.
fn settings_toggle(key: &str, title: &str, on: bool, field: fn(&mut Modal) -> Option<&mut bool>) -> El {
    switch_row(key.to_string(), title, None, on, move |ws| {
        if let Some(flag) = ws.modal.as_mut().and_then(field) {
            *flag = !*flag;
        }
    })
}

/// A row that opens a menu of `options` under itself; the first entry, "Claude Code default", clears the choice.
fn picker_row<T: Copy + PartialEq + 'static>(key: &'static str, title: &str, current: Option<T>, options: &'static [T], name: fn(T) -> &'static str, pick: fn(Option<T>) -> Act) -> El {
    const DEFAULT: &str = "Claude Code default";
    setting_row(key.to_string())
        .cursor(Cursor::Pointer)
        .role("button")
        .hover(Color::WHITE.alpha(0.02))
        .child(label(title.to_string(), 13.0, theme::TEXT))
        .child(label(current.map_or(DEFAULT, name), 13.0, if current.is_some() { theme::ACCENT } else { theme::TEXT_FAINT }))
        .on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
            let items = std::iter::once(MenuItem::new(format!("{}{DEFAULT}", tick(current.is_none())), pick(None)))
                .chain(options.iter().map(|&o| MenuItem::new(format!("{}{}", tick(current == Some(o)), name(o)), pick(Some(o)))))
                .collect();
            ws.open_menu(MenuAnchor::Key(key.into()), 200.0, items);
            ws.after_input(tx);
        })
}

/// Adds `item` to `items`, or removes it when it is there.
fn flip<T: PartialEq>(items: &mut Vec<T>, item: T) {
    match items.iter().position(|i| *i == item) {
        Some(at) => {
            items.remove(at);
        }
        None => items.push(item),
    }
}

/// A − / + button that moves the scale by `by` percent, within the allowed range.
fn scale_step(key: &'static str, sign: &'static str, by: i32) -> El {
    row()
        .key(key)
        .size(22.0, 22.0)
        .center()
        .radius(4.0)
        .cursor(Cursor::Pointer)
        .role("button")
        .hover(Color::WHITE.alpha(0.08))
        .press_scale(0.9)
        .child(label(sign, 13.0, theme::TEXT))
        .on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
            if let Some(Modal::Settings { scale, .. }) = ws.modal.as_mut() {
                *scale = (*scale as i32 + by).clamp(crate::panels::scale::MIN as i32, crate::panels::scale::MAX as i32) as u32;
            }
            ws.after_input(tx);
        })
}

impl Workspace {
    pub(crate) fn settings_body(&self) -> El {
        let Some(Modal::Settings { shell, claude, model, effort, permission, updates, chrome, scale, trimmed, claudiu_off, skills_field, costly, hints }) = &self.modal else {
            return el();
        };
        let page = self.settings_page;
        let content = match page {
            SettingsPage::General => col()
                .gap(10.0)
                .child(field(0, "SHELL", shell, &hints[0]))
                .child(field(1, "CLAUDE CODE PATH", claude, &hints[1]))
                .child(row().child(button("Check setup", false).key("settings-check-setup").on_press_tx(|ws: &mut Workspace, tx: &mut Tx| {
                    ws.act(Act::OpenSetup);
                    ws.after_input(tx);
                })))
                .child(
                    setting_row("settings-scale".into()).child(label("Scale (text and terminal)", 13.0, theme::TEXT)).child(
                        row()
                            .center_y()
                            .gap(10.0)
                            .child(scale_step("scale-down", "−", -(crate::panels::scale::STEP as i32)))
                            .child(row().w(40.0).justify_center().child(label(format!("{scale}%"), 13.0, theme::TEXT)))
                            .child(scale_step("scale-up", "+", crate::panels::scale::STEP as i32)),
                    ),
                )
                .child(settings_toggle("settings-updates", "Check for updates", *updates, |m| match m {
                    Modal::Settings { updates, .. } => Some(updates),
                    _ => None,
                })),
            SettingsPage::Sessions => {
                let mut c = col()
                    .gap(10.0)
                    .child(field(2, "MODEL FOR NEW SESSIONS", model, &hints[2]))
                    .child(picker_row("settings-effort", "Effort", *effort, &Effort::ALL, Effort::name, Act::PickEffort))
                    .child(picker_row("settings-permission", "Permission mode", *permission, &PermissionMode::ALL, PermissionMode::name, Act::PickPermission));
                if *permission == Some(PermissionMode::BypassPermissions) {
                    c = c.child(faint("Every new session will run commands and edit files without asking."));
                }
                if *permission == Some(PermissionMode::Plan) && trimmed.contains(&Trim::PlanMode) {
                    c = c.child(faint("Plan mode is off under Claude Code tools, so new sessions start in Claude Code's default mode."));
                }
                c.child(settings_toggle("settings-chrome", "Claude in Chrome for new sessions", *chrome, |m| match m {
                    Modal::Settings { chrome, .. } => Some(chrome),
                    _ => None,
                }))
            }
            SettingsPage::ClaudeTools => {
                let mut list = col().gap(8.0);
                for found in costly {
                    list = list.child(label(format!("{} is set in {}. {}", found.name, found.source, found.effect), 11.0, theme::WARN).wrap());
                }
                list = list.child(faint(
                    "Applies to new sessions. Running sessions keep their tools. Resumed sessions get the new tools, and the old instructions stay until their next compaction. These switches edit Claude Code's own settings.json, for every account and for Claude Code outside Claudiu. Claudiu only turns things off; anything you turned off yourself stays off.",
                ));
                for (heading, items) in Trim::GROUPS {
                    list = list.child(section_label(heading.to_string()).mt(4.0));
                    for &item in items {
                        list = list.child(switch_row(format!("settings-trim-{}", item as usize), item.label(), Some(item.saving().label()), !trimmed.contains(&item), move |ws| {
                            if let Some(Modal::Settings { trimmed, .. }) = ws.modal.as_mut() {
                                flip(trimmed, item);
                            }
                        }));
                    }
                }
                if trimmed.contains(&Trim::PlanMode) {
                    list = list.child(faint("With Plan mode off, Claude can't leave plan mode itself; press Shift+Tab."));
                }
                list.child(section_label("SKILLS").mt(4.0))
                    .child(faint("Claude no longer sees these skills, but you can still type them. A setting you already have for a skill in Claude Code's settings.json wins."))
                    .child(field(3, "SKILLS TO TURN OFF, SEPARATED BY COMMAS", skills_field, &hints[3]))
            }
            SettingsPage::ClaudiuTools => {
                let mut list = col().gap(8.0).child(faint(
                    "Applies to new sessions. Running sessions keep their tools. Resumed sessions get the new tools, and the old instructions stay until their next compaction.",
                ));
                for feature in Feature::ALL {
                    list = list.child(switch_row(format!("settings-claudiu-{}", feature as usize), feature.label(), None, !claudiu_off.contains(&feature), move |ws| {
                        if let Some(Modal::Settings { claudiu_off, .. }) = ws.modal.as_mut() {
                            flip(claudiu_off, feature);
                        }
                    }));
                }
                list
            }
            SettingsPage::Worktrees => self.worktrees_page(),
            SettingsPage::About => col().child(faint(format!(
                "Claudiu {} · An independent project, not affiliated with, endorsed by or sponsored by Anthropic. Claude is a trademark of Anthropic.",
                env!("CARGO_PKG_VERSION")
            ))),
        };
        // The highlight under the chosen category slides to it.
        let item_h = crate::panels::scale::px(28.0);
        let at = SettingsPage::ALL.iter().position(|p| *p == page).unwrap_or(0) as f32;
        let mut categories = col()
            .key("settings-categories")
            .w(crate::panels::scale::px(160.0))
            .gap(2.0)
            .child(el().key("settings-highlight").absolute(0.0, at * (item_h + 2.0)).w_full().h(item_h).radius(6.0).fill(rgb(0x1f1f22)).motion(motion::SNAP));
        for p in SettingsPage::ALL {
            let selected = p == page;
            categories = categories.child(
                row()
                    .key(format!("settings-category-{}", p.label()))
                    .h(item_h)
                    .w_full()
                    .px(10.0)
                    .center_y()
                    .radius(6.0)
                    .cursor(Cursor::Pointer)
                    .role("tab")
                    .hover(Color::WHITE.alpha(0.03))
                    .child(label(p.label(), 13.0, if selected { theme::ACCENT } else { theme::TEXT_DIM }))
                    .on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
                        ws.set_settings_page(p);
                        // The category list has the keyboard again: Up and Down move through it.
                        ws.blur = true;
                        ws.after_input(tx);
                    }),
            );
        }
        row()
            .key("settings-body")
            .grow(1.0)
            .shrink()
            .min_h(0.0)
            .w_full()
            .gap(16.0)
            .child(categories)
            .child(col().key(format!("settings-page-{}", page.label())).grow(1.0).shrink().min_w(0.0).h_full().scroll(ScrollSpec { stick_bottom: false }).transition(rune::Transition::opacity()).child(content.w_full().pr(6.0)))
    }

    /// The Worktrees page: the worktree folders Claudiu made, by repository, each to show in the file manager or remove.
    fn worktrees_page(&self) -> El {
        let page = &self.worktrees_page;
        let reveal = if cfg!(target_os = "macos") { "Show in Finder" } else { "Show in File Explorer" };
        let mut list = col().gap(8.0).child(faint(
            "Worktrees Claudiu made live in its own data folder, outside your repositories. Removing one here asks the same question as the x in the top-bar menu.",
        ));
        let Some(stored) = &page.stored else { return list.child(faint("Looking…")) };
        if stored.is_empty() {
            return list.child(faint("No worktrees yet. Make one from the worktree button in the top bar."));
        }
        let mut group: Option<&std::path::Path> = None;
        for (i, item) in stored.iter().enumerate() {
            let repo_folder = item.folder.parent();
            if group != repo_folder {
                group = repo_folder;
                let heading = match &item.main_checkout {
                    Some(main) => main.to_string_lossy().into_owned(),
                    None => format!("{} (repository not found)", repo_folder.and_then(|p| p.file_name()).unwrap_or_default().to_string_lossy()),
                };
                list = list.child(section_label(heading).mt(4.0));
            }
            let name = item.folder.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let revealed = item.folder.clone();
            let ask = match item.main_checkout.clone() {
                Some(main_checkout) => Act::RemoveWorktree { main_checkout, folder: item.folder.clone() },
                None => Act::AskDeleteUnlisted(item.folder.clone()),
            };
            let mut r = col()
                .key(format!("worktree-{i}"))
                .gap(6.0)
                .px(10.0)
                .py(6.0)
                .radius(6.0)
                .fill(rgb(0x111113))
                .border(1.0, rgb(0x242428))
                .child(
                    row()
                        .w_full()
                        .center_y()
                        .justify_between()
                        .gap(10.0)
                        .child(col().grow(1.0).shrink().min_w(0.0).child(label(name, 13.0, theme::TEXT)).child(label(item.folder.to_string_lossy(), 11.0, theme::TEXT_FAINT).truncate_middle()))
                        .child(
                            row()
                                .gap(6.0)
                                .child(button(reveal, false).key(format!("worktree-reveal-{i}")).on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
                                    ws.act(Act::Reveal(revealed.clone()));
                                    ws.after_input(tx);
                                }))
                                .child(button("Remove", false).key(format!("worktree-remove-{i}")).on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
                                    ws.act(ask.clone());
                                    ws.after_input(tx);
                                })),
                        ),
                );
            if let Some(asked) = page.asking.as_ref().filter(|a| a.folder == item.folder) {
                let a = asked.act.clone();
                let confirm = if asked.danger { danger_button(asked.confirm.clone()) } else { button(asked.confirm.clone(), true) };
                r = r.child(faint(asked.body.clone())).child(
                    row()
                        .gap(6.0)
                        .child(button("Cancel", false).key(format!("worktree-cancel-{i}")).on_press_tx(|ws: &mut Workspace, tx: &mut Tx| {
                            ws.worktrees_page.asking = None;
                            ws.after_input(tx);
                        }))
                        .child(confirm.key(format!("worktree-confirm-{i}")).on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
                            ws.act(a.clone());
                            ws.after_input(tx);
                        })),
                );
            }
            list = list.child(r);
        }
        list
    }
}

//! The Welcome screen: the main area when no session is shown.

use gpui::{
    ClickEvent, Context, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    ParentElement, Styled, Window, div,
    prelude::FluentBuilder, px,
};

use crate::app::{
    Act, GitPickerTarget, ProjectPickerMode, Workspace, WorkspacePicker,
};
use gpui_kit::component::input::{Paste, Textarea};
use crate::harness::permission::Permission;
use crate::panels::{attachments, composer};
use crate::panels::theme::{self, hsla};
use gpui_kit::assets::IconName;
use crate::panels::widgets::{
    Trigger, icon, spinner,
};

impl Workspace {
    pub(super) fn render_home(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        use gpui::Focusable;
        let scaled = crate::panels::scale::px;
        let chosen_model = self.home_selected_model();
        let permission = self.home_selected_permission();

        let project = self.home_selected_project().cloned();
        let target_cwd = self.home_target_cwd();
        let git = project.as_ref().and_then(|p| p.git.clone());
        let branch = git.as_ref().and_then(|g| g.branch_at(&target_cwd)).map(str::to_string);
        let worktree_label = git.as_ref().and_then(|g| crate::git::worktree::label(g, &target_cwd));

        let project_open = matches!(self.picker_kind(), Some(WorkspacePicker::Project { mode: ProjectPickerMode::Home, .. }));
        let project_label = project.as_ref().map_or_else(|| "Project".to_string(), |p| p.name.clone());
        let project_chip = composer::chip("home-project", project_open)
            .text_color(hsla(if project.is_some() { theme::TEXT } else { theme::TEXT_DIM }))
            .child(icon(IconName::FolderOpen, 12.0).text_color(hsla(theme::TEXT_DIM)))
            .child(div().max_w(px(150.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(project_label))
            .child(icon(IconName::ChevronDown, 10.5).text_color(hsla(theme::TEXT_FAINT)))
            .dropdown(cx, |ws, at, window, cx| {
                ws.open_project_picker(ProjectPickerMode::Home, at, window, cx);
            });

        let creating = self.home.creating.as_ref().and_then(|f| f.file_name()).map(|n| n.to_string_lossy().into_owned());
        let git_chip = project.as_ref().and_then(|p| {
            let g = p.git.as_ref()?;
            // A worktree git is still creating shows by its name, with a spinner, before it exists.
            let primary = creating.clone().or_else(|| branch.clone()).or_else(|| worktree_label.clone()).unwrap_or_else(|| "main".into());
            let linked = if creating.is_some() { None } else { (!g.is_main(&target_cwd)).then(|| worktree_label.clone()).flatten() };
            let tail = creating.is_some().then(|| spinner("home-git-creating", 11.0).into_any_element());
            let pid = p.id.clone();
            let cwd = target_cwd.clone();
            let open = matches!(self.picker_kind(), Some(WorkspacePicker::Git(gp)) if gp.target == GitPickerTarget::Home);
            Some(
                composer::git_chip("home-git", open, primary, linked, tail)
                    .dropdown(cx, move |ws, at, window, cx| {
                        ws.open_git_picker(pid.clone(), cwd.clone(), GitPickerTarget::Home, at, window, cx);
                    })
                    .into_any_element(),
            )
        });

        let model_open = matches!(self.picker_kind(), Some(WorkspacePicker::HomeModel { .. }));
        let effort_badge = crate::thread::short_effort_label(self.home.effort.name());
        let model_anchor = self.home.model_anchor.clone();
        let key_model_anchor = model_anchor.clone();
        let model_chip = composer::model_chip("home-model", model_open, &model_anchor, chosen_model.harness, chosen_model.name.clone(), Some(effort_badge))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |ws, _: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    ws.open_home_model_picker(model_anchor.below(), window, cx);
                }),
            )
            .on_key_down(cx.listener(move |ws, ev: &gpui::KeyDownEvent, window, cx| {
                let plain = !ev.keystroke.modifiers.control && !ev.keystroke.modifiers.alt && !ev.keystroke.modifiers.platform && !ev.keystroke.modifiers.shift;
                if plain && matches!(ev.keystroke.key.as_str(), "enter" | "space") {
                    cx.stop_propagation();
                    ws.open_home_model_picker(key_model_anchor.below(), window, cx);
                }
            }))
            .into_any_element();

        let perm_open = matches!(self.picker_kind(), Some(WorkspacePicker::HomePermission));
        let perm_chip = composer::permission_chip("home-mode", perm_open, permission.map_or("Harness default", Permission::label))
            .dropdown(cx, |ws, at, window, cx| {
                ws.open_home_permission_picker(at, window, cx);
            })
            .into_any_element();

        let can_send = self.home.attachments.has_files() || composer::can_send(self.home.prompt.read(cx).value().as_ref());
        let start_btn = composer::send_button("home-launch", can_send)
            .when(can_send, |d| d.on_click(cx.listener(|ws, _: &ClickEvent, window, cx| ws.act(Act::HomeLaunch, window, cx))))
            .into_any_element();

        let focused = self.home.prompt.focus_handle(cx).is_focused(window);
        let input = composer::input_area()
            .capture_action(cx.listener(|ws, _: &Paste, _, cx| {
                if ws.home.attachments.paste(cx) {
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .children(self.home.attachments.render(|ws: &mut Self| &mut ws.home.attachments, cx))
            .child(div()
                .min_h(px(54.0))
                .on_mouse_down(MouseButton::Left, cx.listener(|ws, _, window, cx| ws.home.prompt.update(cx, |p, cx| p.focus(window, cx))))
                .child(Textarea::new(&self.home.prompt).appearance(false)))
            .into_any_element();
        let composer = composer::render(composer::Composer {
            lit: focused,
            top: None,
            input,
            model: model_chip,
            permission: Some(perm_chip),
            project: Some(project_chip.into_any_element()),
            git: git_chip,
            extra_chips: vec![],
            extra_actions: vec![attachments::button(&self.home.prompt, |ws: &mut Self| &mut ws.home.attachments, true, cx).into_any_element()],
            send: start_btn,
        })
        .max_w(px(680.0))
        .on_drop(cx.listener(|ws, paths: &gpui::ExternalPaths, window, cx| {
            ws.home.attachments.add_paths(paths.paths());
            ws.home.prompt.update(cx, |p, cx| p.focus(window, cx));
            cx.notify();
        }));

        div()
            .id("home-root")
            .relative()
            .size_full()
            .px(px(28.0))
            .pb(px(96.0))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(20.0))
            .child(div().text_size(scaled(24.0)).font_weight(FontWeight::MEDIUM).text_color(hsla(theme::TEXT)).child("What are we working on?"))
            .child(composer)
    }
}

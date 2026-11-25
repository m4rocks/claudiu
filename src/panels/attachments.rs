use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    App, ClickEvent, ClipboardEntry, Context, Div, Entity, ExternalPaths, Image, InputEvent,
    InteractiveElement, IntoElement, ParentElement, PathPromptOptions, StatefulInteractiveElement,
    Styled, StyledImage, Window, div, img,
    prelude::FluentBuilder, px,
};
use gpui_kit::{
    assets::IconName,
    base::Button,
    component::{input::TextareaState, tooltip::Tooltip},
};

use crate::panels::theme::{self, hsla};
use crate::panels::widgets::{focusable, icon, icon_button};

#[derive(Default)]
pub struct Attachments {
    files: Vec<Attachment>,
    error: Option<String>,
}

struct Attachment {
    path: PathBuf,
    pasted: Option<Arc<Image>>,
}

impl Attachments {
    pub fn has_files(&self) -> bool {
        !self.files.is_empty()
    }

    pub fn add_paths(&mut self, paths: &[PathBuf]) {
        self.error = None;
        for path in paths {
            match std::path::absolute(path) {
                Ok(path) if path.is_file() => {
                    if !self.files.iter().any(|file| file.path == path) {
                        self.files.push(Attachment { path, pasted: None });
                    }
                }
                _ => self.error = Some(format!("Could not attach {}", path.display())),
            }
        }
    }

    /// Return true for a file/image paste so the textarea doesn't also insert its text representation.
    pub fn paste(&mut self, cx: &App) -> bool {
        let item = cx.read_from_clipboard();
        #[cfg(target_os = "linux")]
        let may_contain_files = item
            .as_ref()
            .and_then(|item| item.text())
            .is_none_or(|text| {
                text.lines()
                    .any(|line| line.starts_with('/') || line.starts_with("file://"))
            });
        let entries = item.map_or_else(Vec::new, |item| item.entries);
        let mut files = entries
            .iter()
            .filter_map(|entry| match entry {
                ClipboardEntry::ExternalPaths(paths) => Some(paths.paths()),
                _ => None,
            })
            .peekable();
        if files.peek().is_some() {
            self.add_paths(&files.flatten().cloned().collect::<Vec<_>>());
            return true;
        }
        let mut handled = false;
        for entry in entries {
            if let ClipboardEntry::Image(image) = entry {
                handled = true;
                self.error = None;
                if self
                    .files
                    .iter()
                    .any(|file| file.pasted.as_deref() == Some(&image))
                {
                    continue;
                }
                let name = format!("{}.{}", uuid::Uuid::new_v4(), image.format.extension());
                let path = crate::datadir::data_dir().join("attachments").join(name);
                self.files.push(Attachment {
                    path,
                    pasted: Some(Arc::new(image)),
                });
            }
        }
        #[cfg(target_os = "linux")]
        if !cfg!(test)
            && !handled
            && may_contain_files
            && let Ok(mut clipboard) = arboard::Clipboard::new()
            && let Ok(paths) = clipboard.get().file_list()
            && !paths.is_empty()
        {
            self.add_paths(&paths);
            return true;
        }
        handled
    }

    /// Use the existing text prompt path so attachments survive queuing, steering, and hand-offs.
    pub fn message(&mut self, text: &str) -> Option<String> {
        match self.build_message(text) {
            Ok(message) => {
                self.error = None;
                Some(message)
            }
            Err(error) => {
                self.error = Some(error.to_string());
                None
            }
        }
    }

    fn build_message(&self, text: &str) -> std::io::Result<String> {
        if self.files.is_empty() {
            return Ok(text.to_owned());
        }
        let mut message = String::new();
        // Start ordinary messages with the user's text so Home can use it as the session title.
        let text_first = !text.is_empty() && !text.starts_with('/');
        if text_first {
            let _ = write!(message, "{text}\n\n");
        }
        message.push_str("Attached local files (open these paths to inspect them):\n");
        for file in &self.files {
            if let Some(image) = &file.pasted {
                std::fs::create_dir_all(file.path.parent().expect("attachment directory"))?;
                std::fs::write(&file.path, &image.bytes)?;
            } else if !file.path.is_file() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("Attachment is missing: {}", file.path.display()),
                ));
            }
            let path = serde_json::to_string(&file.path.to_string_lossy())?;
            let _ = writeln!(message, "- {path}");
        }
        if !text_first && !text.is_empty() {
            message.push('\n');
            message.push_str(text);
        }
        Ok(message)
    }

    pub fn render<V: 'static>(
        &self,
        select: fn(&mut V) -> &mut Self,
        cx: &mut Context<V>,
    ) -> Option<Div> {
        if self.files.is_empty() && self.error.is_none() {
            return None;
        }
        let cards = self
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| {
                let name = if file.pasted.is_some() {
                    "Pasted image".into()
                } else {
                    file.path
                        .file_name()
                        .unwrap_or(file.path.as_os_str())
                        .to_string_lossy()
                        .into_owned()
                };
                let label = format!("Remove {name}");
                let preview = file
                    .pasted
                    .as_ref()
                    .map(|image| img(image.clone()))
                    .or_else(|| {
                        let is_image = file
                            .path
                            .extension()
                            .and_then(|ext| ext.to_str())
                            .is_some_and(|ext| {
                                matches!(
                                    ext.to_ascii_lowercase().as_str(),
                                    "png"
                                        | "jpg"
                                        | "jpeg"
                                        | "webp"
                                        | "gif"
                                        | "svg"
                                        | "bmp"
                                        | "tif"
                                        | "tiff"
                                        | "ico"
                                        | "pbm"
                                        | "ppm"
                                        | "pgm"
                                )
                            });
                        is_image.then(|| img(file.path.clone()))
                    });
                let content = if let Some(preview) = preview {
                    preview
                        .w(px(64.0))
                        .h(px(48.0))
                        .object_fit(gpui::ObjectFit::Contain)
                        .into_any_element()
                } else {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(5.0))
                        .child(icon(IconName::File, 14.0))
                        .child(
                            div()
                                .max_w(px(132.0))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(name.clone()),
                        )
                        .into_any_element()
                };
                div()
                    .id(("attachment", index))
                    .flex()
                    .items_center()
                    .gap(px(3.0))
                    .px(px(5.0))
                    .py(px(3.0))
                    .rounded(px(6.0))
                    .bg(hsla(theme::PANEL_BG))
                    .text_size(px(12.0))
                    .text_color(hsla(theme::TEXT_DIM))
                    .tooltip(move |window, cx| Tooltip::new(name.clone()).build(window, cx))
                    .child(content)
                    .child(
                        icon_button("remove", IconName::X)
                            .accessibility_label(label)
                            .on_click(cx.listener(move |v, _: &ClickEvent, _, cx| {
                                let attachments = select(v);
                                attachments.files.remove(index);
                                attachments.error = None;
                                cx.notify();
                            })),
                    )
            })
            .collect::<Vec<_>>();
        Some(div()
            .flex()
            .flex_col()
            .gap(px(5.0))
            .pb(px(8.0))
            .when(!cards.is_empty(), |d| {
                d.child(
                    div()
                        .id("composer-attachments")
                        .max_h(px(108.0))
                        .overflow_y_scroll()
                        .child(div().flex().flex_wrap().gap(px(6.0)).children(cards)),
                )
            })
            .when_some(self.error.clone(), |d, error| {
                d.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(hsla(theme::DANGER))
                        .child(error),
                )
            }))
    }
}

pub fn drop_target(
    frame: Div,
    drop: impl Fn(&ExternalPaths, &mut Window, &mut App) + 'static,
) -> Div {
    frame
        .debug_selector(|| "composer-file-drop".into())
        .on_drag_move::<ExternalPaths>(|drag, window, cx| {
            // GPUI converts native file drags to mouse events after updating input modality.
            // Restore mouse hit testing if the previous interaction was a keystroke.
            if window.last_input_was_keyboard() && drag.bounds.contains(&drag.event.position) {
                let event = drag.event.clone();
                window.defer(cx, move |window, cx| {
                    window.dispatch_event(event.to_platform_input(), cx);
                });
            }
        })
        .drag_over::<ExternalPaths>(|style, _, _, _| style.border_color(hsla(theme::FOCUS_BORDER)))
        .on_drop(drop)
}

pub fn button<V: 'static>(
    textarea: &Entity<TextareaState>,
    select: fn(&mut V) -> &mut Attachments,
    cx: &mut Context<V>,
) -> Button {
    let textarea = textarea.downgrade();
    focusable("composer-attach")
        .accessibility_label("Attach files")
        .size(px(26.0))
        .flex_none()
        .rounded(px(5.0))
        .text_color(hsla(theme::TEXT_DIM))
        .hover(|s| s.text_color(hsla(theme::TEXT)))
        .cursor_pointer()
        .debug_selector(|| "composer-attach".into())
        .child(icon(IconName::Paperclip, 16.0))
        .on_click(cx.listener(move |_, _: &ClickEvent, window, cx| {
            let textarea = textarea.clone();
            let picker = cx.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: true,
                prompt: Some("Attach files".into()),
            });
            cx.spawn_in(window, async move |this, cx| {
                let selection = picker.await;
                let _ = this.update_in(cx, |v, window, cx| {
                    if let Ok(Ok(Some(paths))) = selection {
                        select(v).add_paths(&paths);
                        cx.notify();
                    }
                    let _ = textarea.update(cx, |input, cx| input.focus(window, cx));
                });
            })
            .detach();
        }))
}

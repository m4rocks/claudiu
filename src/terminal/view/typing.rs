//! Keyboard, paste and platform text input: everything that turns what the user types into bytes for the PTY.

use super::*;

impl TerminalView {
    pub(super) fn send(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        // Claude Code is not listening yet: keys edit the draft instead (`draft_key`).
        if !self.held() {
            self.write_input(bytes, cx);
        }
    }

    pub(super) fn write_input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        let mut term = self.terminal.term.lock();
        term.selection = None;
        if term.grid().display_offset() != 0 {
            term.scroll_display(Scroll::Bottom);
        }
        drop(term);
        (self.frac, self.glide) = (0.0, None);
        self.terminal.write(bytes);
        cx.notify();
    }

    pub fn paste_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.held() {
            self.draft.insert(text);
            cx.notify();
            return;
        }
        let bytes = self.paste_bytes(text);
        self.send(bytes, cx);
    }

    pub(super) fn paste_bytes(&self, text: &str) -> Vec<u8> {
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if self.mode().contains(TermMode::BRACKETED_PASTE) {
            // Strip embedded end-marker so pasted text can't terminate the paste early.
            let clean = text.replace("\x1b[201~", "");
            format!("\x1b[200~{clean}\x1b[201~").into_bytes()
        } else {
            text.into_bytes()
        }
    }

    pub fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        if let Some(text) = item.text().filter(|t| !t.is_empty()) {
            self.paste_text(&text, cx);
            return;
        }
        // While Claude Code starts, images are kept as files and typed in as paths when it is ready (`skeleton.rs`).
        if self.loading {
            for entry in item.entries() {
                match entry {
                    gpui::ClipboardEntry::Image(image) => {
                        if let Some(path) = save_pasted(image) {
                            self.draft.add_image(path);
                        }
                    }
                    gpui::ClipboardEntry::ExternalPaths(p) => self.paste_paths(p.paths(), cx),
                    _ => {}
                }
            }
            cx.notify();
            return;
        }
        // An image on the clipboard: Claude Code reads it itself on its image-paste key (Alt+V on Windows,
        // Ctrl+V elsewhere).
        if item.entries().iter().any(|e| matches!(e, gpui::ClipboardEntry::Image(_))) {
            let key: &[u8] = if cfg!(target_os = "windows") { b"\x1bv" } else { b"\x16" };
            self.send(key.to_vec(), cx);
        } else if let Some(paths) = item.entries().iter().find_map(|e| match e {
            gpui::ClipboardEntry::ExternalPaths(p) => Some(p.paths().to_vec()),
            _ => None,
        }) {
            self.paste_paths(&paths, cx);
        }
    }

    /// Dropped or copied files: paste their paths (quoted when needed); Claude Code attaches them.
    pub fn paste_paths(&mut self, paths: &[std::path::PathBuf], cx: &mut Context<Self>) {
        if self.loading {
            for p in paths {
                if is_image_file(p) {
                    self.draft.add_image(p.clone());
                } else {
                    self.draft.insert(&format!("{} ", skeleton::quote_path(p)));
                }
            }
            cx.notify();
            return;
        }
        let text = paths.iter().map(|p| skeleton::quote_path(p)).collect::<Vec<_>>().join(" ");
        if !text.is_empty() {
            self.paste_text(&format!("{text} "), cx);
        }
    }

    /// The last `max_lines` lines of the screen and scrollback as plain text, trailing blank lines dropped.
    pub fn tail_text(&self, max_lines: usize) -> String {
        let term = self.terminal.term.lock();
        let (top, bottom, last) = (term.topmost_line(), term.bottommost_line(), term.last_column());
        let start = Line((bottom.0 - max_lines.max(1) as i32 + 1).max(top.0));
        term.bounds_to_string(GridPoint::new(start, Column(0)), GridPoint::new(bottom, last)).trim_end().to_string()
    }

    /// Copies the selection; returns whether there was one.
    pub fn copy(&mut self, cx: &mut Context<Self>) -> bool {
        let mut term = self.terminal.term.lock();
        let text = term.selection_to_string().filter(|t| !t.is_empty());
        if text.is_some() {
            term.selection = None;
        }
        drop(term);
        match text {
            Some(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                cx.notify();
                true
            }
            None => false,
        }
    }

    pub(super) fn select_all(&mut self, cx: &mut Context<Self>) {
        let mut term = self.terminal.term.lock();
        let (top, bottom, last) = (term.topmost_line(), term.bottommost_line(), term.last_column());
        let mut sel = Selection::new(SelectionType::Simple, GridPoint::new(top, Column(0)), Side::Left);
        sel.update(GridPoint::new(bottom, last), Side::Right);
        term.selection = Some(sel);
        drop(term);
        cx.notify();
    }

    pub(super) fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let m = &ks.modifiers;
        let key = ks.key.as_str();

        // Claudiu terminal shortcuts. Everything else goes to the program untouched.
        let copy_chord = (m.control && m.shift && key == "c")
            || (m.control && !m.shift && !m.alt && key == "insert");
        let paste_chord = (m.control && m.shift && key == "v")
            || (m.shift && !m.control && !m.alt && key == "insert");
        // Ctrl+V pastes on Windows/Linux (as in Windows Terminal); on macOS Cmd+C / Cmd+V are the copy/paste keys.
        let ctrl_v = !cfg!(target_os = "macos") && m.control && !m.shift && !m.alt && key == "v";
        let mac_copy = m.platform && !m.shift && !m.control && !m.alt && key == "c";
        let mac_paste = m.platform && !m.shift && !m.control && !m.alt && key == "v";
        // Every key handled here stops propagation, so the platform doesn't also deliver it as text.
        if mac_copy || copy_chord {
            self.copy(cx);
            cx.stop_propagation();
            return;
        }
        if mac_paste || paste_chord || ctrl_v {
            self.paste(cx);
            cx.stop_propagation();
            return;
        }
        // Ctrl+C with a selection copies (like Windows Terminal); otherwise it is SIGINT as usual.
        if m.control && !m.shift && !m.alt && key == "c" && self.copy(cx) {
            cx.stop_propagation();
            return;
        }
        if m.shift && !m.control && !m.alt {
            let scroll = match key {
                "pageup" => Some(Scroll::PageUp),
                "pagedown" => Some(Scroll::PageDown),
                "home" => Some(Scroll::Top),
                "end" => Some(Scroll::Bottom),
                _ => None,
            };
            if let Some(scroll) = scroll {
                (self.frac, self.glide) = (0.0, None);
                self.terminal.term.lock().scroll_display(scroll);
                cx.notify();
                cx.stop_propagation();
                return;
            }
        }
        // Plain text arrives through `EntityInputHandler::replace_text_in_range` (IME, dead keys).
        if self.exit_code.is_some() || keys::is_text(ks) {
            return;
        }
        if self.held() {
            self.draft.key(ks);
            cx.notify();
            cx.stop_propagation();
            return;
        }
        if let Some(bytes) = keys::encode(ks, self.mode()) {
            self.send(bytes, cx);
            cx.stop_propagation();
        }
    }

    /// Type a message into Claude Code: a line as keystrokes, several as one paste so their breaks stay breaks.
    pub(super) fn type_in(&mut self, text: &str, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }
        // A long line is better pasted than typed.
        let bytes = if text.contains('\n') || text.chars().count() > 200 { self.paste_bytes(text) } else { text.as_bytes().to_vec() };
        self.write_input(bytes, cx);
    }

    /// Put an image file on the clipboard and press Claude Code's image-paste key, which reads it from there.
    pub(super) fn paste_image_file(&mut self, path: &std::path::Path, cx: &mut Context<Self>) -> bool {
        use gpui::ImageFormat::*;
        let format = match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
            Some("png") => Png,
            Some("jpg" | "jpeg") => Jpeg,
            Some("gif") => Gif,
            Some("webp") => Webp,
            Some("bmp") => Bmp,
            _ => return false,
        };
        let Ok(bytes) = std::fs::read(path) else { return false };
        #[cfg(target_os = "windows")]
        if !crate::platform::set_clipboard_bitmap(&bytes) {
            return false;
        }
        #[cfg(not(target_os = "windows"))]
        cx.write_to_clipboard(ClipboardItem::new_image(&gpui::Image::from_bytes(format, bytes)));
        #[cfg(target_os = "windows")]
        let _ = format;
        let key: &[u8] = if cfg!(target_os = "windows") { b"\x1bv" } else { b"\x16" };
        self.write_input(key.to_vec(), cx);
        true
    }
}

/// Text input from the platform: typed characters, dead-key results and IME commits. A terminal has no editable
/// document, so ranges are ignored; only in-progress composition (`marked`) is tracked, to draw it at the cursor.
impl EntityInputHandler for TerminalView {
    fn text_for_range(&mut self, _: Range<usize>, _: &mut Option<Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        None
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: 0..0, reversed: false })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|t| 0..t.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(&mut self, _: Option<Range<usize>>, text: &str, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        if self.held() {
            self.draft.insert(text);
            cx.notify();
        } else if !text.is_empty() && self.is_running() {
            self.send(text.as_bytes().to_vec(), cx);
        } else {
            cx.notify();
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = (!text.is_empty()).then(|| text.to_string());
        cx.notify();
    }

    /// The cursor cell, so the IME candidate window opens next to it.
    fn bounds_for_range(&mut self, _: Range<usize>, _: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let m = self.metrics?;
        let term = self.terminal.term.lock();
        let cursor = term.grid().cursor.point;
        let row = (cursor.line.0 + term.grid().display_offset() as i32).max(0);
        Some(Bounds::new(
            point(m.origin.x + m.cell_w * cursor.column.0 as f32, m.origin.y + m.shift + m.line_h * row as f32),
            size(m.cell_w, m.line_h),
        ))
    }

    fn character_index_for_point(&mut self, _: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        None
    }
}

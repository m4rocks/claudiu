//! Keyboard, paste and platform text input: everything that turns what the user types into bytes for the PTY.

use super::*;

impl TerminalView {
    pub(super) fn send(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        self.write_input(bytes, cx);
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
        // An image on the clipboard: a program that takes images (Claude Code run in the shell) reads it itself on its
        // image-paste key (Alt+V on Windows, Ctrl+V elsewhere).
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

    /// Dropped or copied files: paste their paths, quoted when they have spaces.
    pub fn paste_paths(&mut self, paths: &[std::path::PathBuf], cx: &mut Context<Self>) {
        let text = paths.iter().map(|p| quote_path(p)).collect::<Vec<_>>().join(" ");
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

    /// Returns the active terminal selection when non-empty, otherwise up to `max_lines` recent
    /// non-empty lines from the visible screen and scrollback buffer.
    pub fn selection_or_recent_output(&self, max_lines: usize) -> Option<String> {
        let term = self.terminal.term.lock();
        if let Some(selected) = term.selection_to_string() {
            let trimmed = selected.trim_matches(|c| c == '\r' || c == '\n');
            if !trimmed.trim().is_empty() {
                return Some(trimmed.replace("\r\n", "\n"));
            }
        }
        let (top, bottom, last) = (term.topmost_line(), term.bottommost_line(), term.last_column());
        let window_lines = (max_lines.saturating_mul(8)).max(200) as i32;
        let start = Line((bottom.0 - window_lines + 1).max(top.0));
        let raw = term.bounds_to_string(GridPoint::new(start, Column(0)), GridPoint::new(bottom, last));
        extract_recent_output(&raw, max_lines)
    }

    /// Copies the selection; returns whether there was one.
    pub fn copy(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
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
            // Text selected in a card: Ctrl+C copies it rather than interrupting Claude Code.
            None if TextSelection::has_selection(window, cx) => {
                let text = TextSelection::selected_text(window, cx);
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                TextSelection::clear(window, cx);
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

    pub(super) fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let m = &ks.modifiers;
        let key = ks.key.as_str();

        // m4code terminal shortcuts. Everything else goes to the program untouched.
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
            self.copy(window, cx);
            cx.stop_propagation();
            return;
        }
        if mac_paste || paste_chord || ctrl_v {
            self.paste(cx);
            cx.stop_propagation();
            return;
        }
        // Ctrl+C with a selection copies (like Windows Terminal); otherwise it is SIGINT as usual.
        if m.control && !m.shift && !m.alt && key == "c" && self.copy(window, cx) {
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
        if let Some(bytes) = keys::encode(ks, self.mode()) {
            self.send(bytes, cx);
            cx.stop_propagation();
        }
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
        if !text.is_empty() && self.is_running() {
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

/// Default maximum number of non-empty lines grabbed when sending recent shell output to chat.
pub const SHELL_RECENT_LINES: usize = 60;

/// Extract up to `max_non_empty_lines` trailing non-empty lines from raw terminal text,
/// preserving internal blank lines between them and trimming trailing whitespace on each line.
pub fn extract_recent_output(raw: &str, max_non_empty_lines: usize) -> Option<String> {
    if max_non_empty_lines == 0 {
        return None;
    }
    let normalized = raw.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.lines().map(str::trim_end).collect();
    let non_empty: Vec<usize> = lines.iter().enumerate().filter_map(|(i, l)| (!l.is_empty()).then_some(i)).collect();
    let &last_ix = non_empty.last()?;
    let first_non_empty = non_empty.len().saturating_sub(max_non_empty_lines);
    let start_ix = non_empty[first_non_empty];
    Some(lines[start_ix..=last_ix].join("\n"))
}

/// Format shell output as a Markdown fenced quote block, picking a backtick fence longer than
/// any backtick run in `output` so embedded fences never break out.
pub fn format_shell_quote_block(output: &str) -> String {
    let normalized = output.replace("\r\n", "\n");
    let max_ticks = longest_backtick_run(&normalized);
    let fence = "`".repeat(max_ticks.max(2) + 1);
    let mut out = format!("> {fence}\n");
    for line in normalized.lines() {
        if line.is_empty() {
            out.push_str(">\n");
        } else {
            out.push_str("> ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str("> ");
    out.push_str(&fence);
    out
}

fn longest_backtick_run(text: &str) -> usize {
    let (mut best, mut run) = (0usize, 0usize);
    for ch in text.chars() {
        if ch == '`' {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    best
}

/// A path as a shell takes it when pasted: quoted if it has spaces.
fn quote_path(p: &std::path::Path) -> String {
    let s = p.to_string_lossy();
    if s.contains(' ') { format!("\"{s}\"") } else { s.into_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_recent_output_drops_blanks_and_limits_non_empty_lines() {
        assert_eq!(extract_recent_output("   \n\n  \r\n", 60), None);
        assert_eq!(extract_recent_output("hello", 0), None);

        let raw = "\n  \nline 1  \nline 2\n\nline 3\nline 4   \n\n   ";
        assert_eq!(extract_recent_output(raw, 60).as_deref(), Some("line 1\nline 2\n\nline 3\nline 4"));
        assert_eq!(extract_recent_output(raw, 2).as_deref(), Some("line 3\nline 4"));
        assert_eq!(extract_recent_output(raw, 3).as_deref(), Some("line 2\n\nline 3\nline 4"));
    }

    #[test]
    fn format_shell_quote_block_uses_safe_fence_and_preserves_blank_lines() {
        assert_eq!(
            format_shell_quote_block("cargo test\n\nfailed"),
            "> ```\n> cargo test\n>\n> failed\n> ```"
        );
        assert_eq!(
            format_shell_quote_block("echo ```nested```"),
            "> ````\n> echo ```nested```\n> ````"
        );
    }

    #[test]
    fn selection_takes_precedence_over_recent_output() {
        let cx = gpui::TestAppContext::single();
        let (terminal, rx, _, _) = Terminal::build(&SpawnSpec::default(), 40, 6);
        {
            let mut term = terminal.term.lock();
            let mut parser = alacritty_terminal::vte::ansi::Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new();
            parser.advance(&mut *term, b"first line\r\nsecond line\r\nthird line");
        }
        let view = cx.update(|app| app.new(|cx| TerminalView::new(terminal, rx, cx)));
        let recent = cx.update(|app| view.read(app).selection_or_recent_output(2));
        assert_eq!(recent.as_deref(), Some("second line\nthird line"));

        cx.update(|app| {
            let mut term = view.read(app).terminal.term.lock();
            let mut sel = Selection::new(SelectionType::Simple, GridPoint::new(Line(0), Column(0)), Side::Left);
            sel.update(GridPoint::new(Line(0), Column(4)), Side::Right);
            term.selection = Some(sel);
        });
        let selected = cx.update(|app| view.read(app).selection_or_recent_output(60));
        assert_eq!(selected.as_deref(), Some("first"));
    }
}

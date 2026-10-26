//! Keyboard, paste and platform text input: everything that turns what the user types into bytes for the PTY.

use super::*;

impl TerminalView {
    pub(super) fn send(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        // Claude Code is not listening yet: keys edit the draft instead (`draft_key`).
        if !self.held() {
            self.mark_typed();
            self.write_input(bytes, cx);
        }
    }

    fn mark_typed(&mut self) {
        if let Some(r) = &mut self.reference {
            r.typed = true;
        }
    }

    /// Paste `text` into Claude's input as `text: `, so the user types their comment after it (`reference_line`). Not while
    /// Claude Code is starting, or while an earlier one is still there. Only into Claude's own input box: on a menu (a
    /// permission question, the folder trust check) the line break could pick an answer, and without bracketed paste it
    /// would send the message. Returns whether it was inserted.
    pub fn insert_reference(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        if self.held() || self.reference.is_some() || !self.mode().contains(TermMode::BRACKETED_PASTE) {
            return false;
        }
        let Some(column) = self.prompt_column() else { return false };
        let line = reference_line(text, column);
        let bytes = self.paste_bytes(&line);
        self.write_input(bytes, cx);
        self.reference = Some(Inserted { chars: line.chars().count(), typed: false });
        true
    }

    /// The cursor's column when it is in Claude Code's input box: the `❯` line under a full-width rule, with another
    /// rule below.
    fn prompt_column(&self) -> Option<usize> {
        let (rows, line, column) = self.screen_rows();
        cursor_in_prompt(&rows, line).then_some(column)
    }

    /// Whether Claude Code's input box holds `text`, such as a reference the user kept on an earlier visit and hasn't
    /// sent yet.
    pub fn input_holds(&self, text: &str) -> bool {
        let (rows, line, _) = self.screen_rows();
        let squeezed = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        // Spaces aside, since a long input wraps onto more rows.
        prompt_text(&rows, line).is_some_and(|input| squeezed(&input).contains(&squeezed(text)))
    }

    /// The screen's rows as text, and the cursor's line and column.
    fn screen_rows(&self) -> (Vec<String>, usize, usize) {
        let term = self.terminal.term.lock();
        let grid = term.grid();
        let rows: Vec<String> = (0..grid.screen_lines() as i32)
            .map(|l| {
                let row = &grid[Line(l)];
                (0..grid.columns()).map(|c| row[Column(c)].c).collect()
            })
            .collect();
        let cursor = grid.cursor.point;
        (rows, cursor.line.0.max(0) as usize, cursor.column.0)
    }

    /// Erase the inserted line again unless the user typed after it.
    pub fn withdraw_reference(&mut self, cx: &mut Context<Self>) {
        if let Some(r) = self.reference.take().filter(|r| !r.typed) {
            self.write_input(vec![0x7f; r.chars], cx);
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
        self.mark_typed();
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

/// Where Claude Code's input text starts on each of its lines: after `❯ `, and indented as far below it.
const INPUT_COLUMN: usize = 2;

/// The reference as pasted with the cursor at `column` of Claude's input: `@path#L12: `, so the user's comment follows
/// on the same line, and a space before it when the cursor follows text. No line breaks: the user starts a new line
/// for the next comment themselves.
fn reference_line(text: &str, column: usize) -> String {
    if column > INPUT_COLUMN { format!(" {text}: ") } else { format!("{text}: ") }
}

/// A full-width rule, possibly with a label in it: Claude Code names the input box's top border after a named
/// (for instance resumed) session, as in `────── name ──`.
fn is_rule(row: &str) -> bool {
    let row = row.trim_end();
    let dashes = row.chars().filter(|&c| c == '─').count();
    row.starts_with("──") && dashes * 2 >= row.chars().count()
}

/// The rows of Claude Code's input box, joined, when the cursor on `cursor` is inside it.
fn prompt_text(rows: &[String], cursor: usize) -> Option<String> {
    if !cursor_in_prompt(rows, cursor) {
        return None;
    }
    let top = rows[..=cursor].iter().rposition(|r| is_rule(r))?;
    let bottom = cursor + 1 + rows[cursor + 1..].iter().position(|r| is_rule(r))?;
    Some(rows[top + 1..bottom].iter().map(|r| r.trim_end()).collect())
}

/// `rows` of the screen, with the cursor on `cursor`: is it inside Claude Code's input box (see `at_prompt`)?
fn cursor_in_prompt(rows: &[String], cursor: usize) -> bool {
    let Some(top) = rows.get(..=cursor).and_then(|above| above.iter().rposition(|r| is_rule(r))) else { return false };
    let below = rows.get(cursor + 1..).is_some_and(|below| below.iter().any(|r| is_rule(r)));
    // A numbered menu right under a rule also starts with the pointer: `❯ 1. Yes`.
    let numbered = |line: &str| line.trim_start().split_once(". ").is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
    below && top < cursor && rows[top + 1].strip_prefix('❯').is_some_and(|line| !numbered(line))
}

#[cfg(test)]
mod tests {
    use super::{cursor_in_prompt, prompt_text, reference_line};

    #[test]
    fn a_reference_labels_the_users_comment_on_the_same_line() {
        assert_eq!(reference_line("@a.rs#L1", 2), "@a.rs#L1: ", "an empty input, or the start of a line in it");
        assert_eq!(reference_line("@a.rs#L1", 9), " @a.rs#L1: ", "after the user's text, with no line break");
    }

    fn screen(text: &str) -> Vec<String> {
        text.lines().map(String::from).collect()
    }

    #[test]
    fn the_input_box_text_is_its_rows_between_the_rules() {
        let rows = screen("● Done.
────────
❯ explain @a.rs#L1-
  2: why
────────
  ? for shortcuts");
        assert_eq!(prompt_text(&rows, 3).as_deref(), Some("❯ explain @a.rs#L1-  2: why"));
        assert_eq!(prompt_text(&rows, 0), None, "the cursor is outside the box");
    }

    #[test]
    fn only_claude_codes_input_box_counts_as_its_prompt() {
        let prompt = screen("Claude Code

──────────
❯ fix the bug
  and this
──────────
  ⏵⏵ auto mode on");
        assert!(cursor_in_prompt(&prompt, 3));
        assert!(cursor_in_prompt(&prompt, 4), "on the input's second line");
        assert!(!cursor_in_prompt(&prompt, 0), "above the box");
        assert!(!cursor_in_prompt(&prompt, 6), "below the box");
        let trust = screen("──────────
 Do you trust the files in this folder?

 ❯ No, exit
   Yes, I trust this folder

 Enter to confirm");
        assert!(!cursor_in_prompt(&trust, 3), "a menu");
        let question = screen("──────────
Do you want to proceed?
❯ 1. Yes
  2. No
──────────");
        assert!(!cursor_in_prompt(&question, 2), "a numbered question");
        let menu = screen("──────────
❯ 1. Yes
  2. No
──────────");
        assert!(!cursor_in_prompt(&menu, 1), "a numbered menu right under a rule");
    }

    #[test]
    fn a_named_session_labels_the_top_rule_of_its_input_box() {
        let named = screen("● Done.
────────────────────────── fix the freeze ──
❯
──────────────────────────────────────────
  ? for shortcuts");
        assert!(cursor_in_prompt(&named, 2));
    }
}

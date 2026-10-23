//! The GPUI element that lays out, measures and paints the terminal grid.

use super::*;

pub(super) struct TerminalElement {
    pub(super) view: Entity<TerminalView>,
}

impl IntoElement for TerminalElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

#[derive(Clone, PartialEq)]
struct CellStyle {
    fg: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

struct Batch {
    row: usize,
    col: usize,
    next_col: usize,
    text: String,
    style: CellStyle,
}

pub(super) struct Prepaint {
    bounds: Bounds<Pixels>,
    cell_w: Pixels,
    line_h: Pixels,
    backgrounds: Vec<(Bounds<Pixels>, Hsla)>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    glyph_quads: Vec<(Bounds<Pixels>, Hsla)>,
    cursor: Option<(Bounds<Pixels>, CursorShape, bool)>,
    /// IME composition text, painted over the grid at the cursor.
    preedit: Option<(Point<Pixels>, ShapedLine)>,
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let (family, font_size, focused, marked) = {
            let v = self.view.read(cx);
            (v.font_family, crate::panels::scale::px(f32::from(v.font_size)), v.focus.is_focused(window), v.marked.clone())
        };
        let base_font: Font = gpui::font(family);
        let font_id = window.text_system().resolve_font(&base_font);
        let cell_w = window
            .text_system()
            .advance(font_id, font_size, 'm')
            .map(|s| s.width)
            .unwrap_or(px(8.0));
        let line_h = (font_size * 1.35).round();

        let cols = ((bounds.size.width / cell_w).floor() as u16).max(2);
        let rows = ((bounds.size.height / line_h).floor() as u16).max(1);

        let terminal = &self.view.read(cx).terminal;
        terminal.resize(cols, rows, f32::from(cell_w), f32::from(line_h));
        let frac = self.view.read(cx).frac;
        self.view.update(cx, |v, _| {
            v.metrics = Some(Metrics {
                cell_w,
                line_h,
                origin: bounds.origin,
                shift: line_h * frac,
            });
        });
        // Where row 0 of the cells below is drawn (one row higher while the line above is peeking in).
        let top_y;

        let mut backgrounds: Vec<(Bounds<Pixels>, Hsla)> = Vec::new();
        let mut batches: Vec<Batch> = Vec::new();
        let mut glyph_quads: Vec<(Bounds<Pixels>, Hsla)> = Vec::new();
        let mut cursor_out = None;
        let mut preedit_at = None;

        {
            let terminal = &self.view.read(cx).terminal;
            let term = terminal.term.lock();
            let content = term.renderable_content();
            let offset = content.display_offset as i32;
            let colors = content.colors;
            let cursor: RenderableCursor = content.cursor;
            let selection = content.selection;
            let block_cursor = focused && cursor.shape == CursorShape::Block;
            let default_bg = theme::default_bg();

            // Between two lines the view is shifted down by `frac` of a line and the line above shows in the gap.
            let extra = (frac > 0.0 && term.grid().display_offset() < term.history_size()).then(|| Line(-offset - 1));
            let top = extra.is_some() as usize;
            top_y = bounds.origin.y + line_h * frac - line_h * top as f32;
            let cell_origin = |row: usize, col: usize| point(bounds.origin.x + cell_w * col as f32, top_y + line_h * row as f32);
            let above = extra.into_iter().flat_map(|l| (0..cols as usize).map(move |c| (l, c))).map(|(l, c)| (&term.grid()[l][Column(c)], GridPoint::new(l, Column(c))));

            // Open background span: (row, start_col, end_col_exclusive, color)
            let mut open_bg: Option<(usize, usize, usize, Hsla)> = None;
            let flush_bg = |span: &mut Option<(usize, usize, usize, Hsla)>,
                            out: &mut Vec<(Bounds<Pixels>, Hsla)>| {
                if let Some((row, c0, c1, color)) = span.take() {
                    out.push((
                        Bounds::new(
                            cell_origin(row, c0),
                            size(cell_w * (c1 - c0) as f32, line_h),
                        ),
                        color,
                    ));
                }
            };

            for (cell, p) in content.display_iter.map(|i| (i.cell, i.point)).chain(above) {
                let row = p.line.0 + offset;
                if row < -(top as i32) || row >= rows as i32 || p.column.0 >= cols as usize {
                    continue;
                }
                let (row, col) = ((row + top as i32) as usize, p.column.0);
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                let width = if cell.flags.contains(Flags::WIDE_CHAR) {
                    2
                } else {
                    1
                };

                let mut fg = theme::resolve(cell.fg, colors);
                let mut bg = theme::resolve(cell.bg, colors);
                let mut bg_is_default = theme::is_default_bg(&cell.bg);
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                    bg_is_default = false;
                }
                if cell.flags.contains(Flags::DIM) {
                    fg = theme::dimmed(fg);
                }
                if block_cursor && p == cursor.point {
                    std::mem::swap(&mut fg, &mut bg);
                    bg_is_default = false;
                }
                let selected = selection.is_some_and(|s| s.contains(p));
                let bg_color = if selected {
                    bg_is_default = false;
                    theme::hsla(theme::SELECTION)
                } else {
                    theme::rgb_to_hsla(bg)
                };
                let _ = default_bg;

                // Background spans (merged per row)
                if bg_is_default {
                    flush_bg(&mut open_bg, &mut backgrounds);
                } else {
                    match &mut open_bg {
                        Some((r, _, end, color))
                            if *r == row && *end == col && *color == bg_color =>
                        {
                            *end = col + width
                        }
                        _ => {
                            flush_bg(&mut open_bg, &mut backgrounds);
                            open_bg = Some((row, col, col + width, bg_color));
                        }
                    }
                }

                // Text
                let underline =
                    cell.flags.intersects(Flags::ALL_UNDERLINES) || cell.hyperlink().is_some();
                let ch = cell.c;
                if cell.flags.contains(Flags::HIDDEN) || (ch == ' ' && !underline) || ch == '\0' {
                    continue;
                }
                // Block elements / box drawing: exact rectangles instead of font glyphs, so they tile seamlessly.
                let (ox, oy) = (f32::from(bounds.origin.x), f32::from(top_y));
                let (cw, lh) = (f32::from(cell_w), f32::from(line_h));
                let x0 = (ox + cw * col as f32).round();
                let x1 = (ox + cw * (col + width) as f32).round();
                let y0 = (oy + lh * row as f32).round();
                if let Some(rects) = glyphs::rects(ch, x1 - x0, lh) {
                    let color = theme::rgb_to_hsla(fg);
                    for rc in rects {
                        glyph_quads.push((
                            Bounds::new(
                                point(px(x0 + rc.x), px(y0 + rc.y)),
                                size(px(rc.w), px(rc.h)),
                            ),
                            color.opacity(rc.alpha),
                        ));
                    }
                    continue;
                }
                let style = CellStyle {
                    fg: theme::rgb_to_hsla(fg),
                    bold: cell.flags.contains(Flags::BOLD),
                    italic: cell.flags.contains(Flags::ITALIC),
                    underline,
                    strike: cell.flags.contains(Flags::STRIKEOUT),
                };
                // Only plain single-width chars batch; wide chars are placed individually.
                let batchable = width == 1;
                match batches.last_mut() {
                    Some(b)
                        if batchable && b.row == row && b.next_col == col && b.style == style =>
                    {
                        b.text.push(ch);
                        b.next_col += 1;
                    }
                    _ => batches.push(Batch {
                        row,
                        col,
                        next_col: col + width,
                        text: ch.to_string(),
                        style,
                    }),
                }
                if !batchable {
                    // Terminate so the next cell starts a fresh batch.
                    if let Some(b) = batches.last_mut() {
                        b.next_col = usize::MAX;
                    }
                }
            }
            flush_bg(&mut open_bg, &mut backgrounds);

            // Cursor (non-block shapes, or hollow when unfocused)
            let cur_row = cursor.point.line.0 + offset;
            if cur_row >= 0 && cur_row < rows as i32 {
                preedit_at = Some(cell_origin(cur_row as usize + top, cursor.point.column.0));
            }
            if cur_row >= 0
                && cur_row < rows as i32
                && cursor.point.column.0 < cols as usize
                && cursor.shape != CursorShape::Hidden
            {
                let shape = if !focused {
                    CursorShape::HollowBlock
                } else {
                    cursor.shape
                };
                if shape != CursorShape::Block {
                    let wide = term.grid()[cursor.point].flags.contains(Flags::WIDE_CHAR);
                    let origin = cell_origin(cur_row as usize + top, cursor.point.column.0);
                    let bounds =
                        Bounds::new(origin, size(cell_w * if wide { 2.0 } else { 1.0 }, line_h));
                    cursor_out = Some((bounds, shape, focused));
                }
            }
        }

        let mut lines = Vec::with_capacity(batches.len());
        for b in batches {
            let mut font = base_font.clone();
            if b.style.bold {
                font.weight = FontWeight::BOLD;
            }
            if b.style.italic {
                font.style = FontStyle::Italic;
            }
            let run = TextRun {
                len: b.text.len(),
                font,
                color: b.style.fg,
                background_color: None,
                underline: b.style.underline.then_some(UnderlineStyle {
                    color: Some(b.style.fg),
                    thickness: px(1.0),
                    wavy: false,
                }),
                strikethrough: b.style.strike.then_some(StrikethroughStyle {
                    color: Some(b.style.fg),
                    thickness: px(1.0),
                }),
            };
            let line =
                window
                    .text_system()
                    .shape_line(b.text.into(), font_size, &[run], Some(cell_w));
            let origin = point(
                bounds.origin.x + cell_w * b.col as f32,
                top_y + line_h * b.row as f32,
            );
            lines.push((origin, line));
        }

        let preedit = marked.zip(preedit_at).map(|(text, origin)| {
            let fg = theme::hsla(theme::TEXT);
            let run = TextRun {
                len: text.len(),
                font: base_font.clone(),
                color: fg,
                background_color: None,
                underline: Some(UnderlineStyle { color: Some(fg), thickness: px(1.0), wavy: false }),
                strikethrough: None,
            };
            (origin, window.text_system().shape_line(text.into(), font_size, &[run], None))
        });

        Prepaint {
            bounds,
            preedit,
            cell_w,
            line_h,
            backgrounds,
            lines,
            glyph_quads,
            cursor: cursor_out,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        pre: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.paint_quad(fill(pre.bounds, theme::hsla(theme::TERMINAL_BG)));
        // A shifted view draws a row past the edges; the mask cuts it off.
        window.with_content_mask(Some(ContentMask { bounds: pre.bounds }), |window| {
        for (bounds, color) in pre.backgrounds.drain(..) {
            window.paint_quad(fill(bounds, color));
        }
        for (bounds, color) in pre.glyph_quads.drain(..) {
            window.paint_quad(fill(bounds, color));
        }
        for (origin, line) in pre.lines.drain(..) {
            let _ = line.paint(origin, pre.line_h, gpui::TextAlign::Left, None, window, cx);
        }
        if let Some((b, shape, _)) = pre.cursor.take() {
            let color = theme::hsla(theme::ACCENT);
            match shape {
                CursorShape::Beam => window.paint_quad(fill(
                    Bounds::new(b.origin, size(px(2.0), b.size.height)),
                    color,
                )),
                CursorShape::Underline => window.paint_quad(fill(
                    Bounds::new(
                        point(b.origin.x, b.origin.y + b.size.height - px(2.0)),
                        size(b.size.width, px(2.0)),
                    ),
                    color,
                )),
                _ => window.paint_quad(outline(b, color, gpui::BorderStyle::Solid)),
            }
        }
        if let Some((origin, line)) = pre.preedit.take() {
            window.paint_quad(fill(Bounds::new(origin, size(line.width, pre.line_h)), theme::hsla(theme::TERMINAL_BG)));
            let _ = line.paint(origin, pre.line_h, gpui::TextAlign::Left, None, window, cx);
        }
        });
        // Receive typed text / IME input while this terminal has focus.
        let focus = self.view.read(cx).focus.clone();
        window.handle_input(&focus, ElementInputHandler::new(pre.bounds, self.view.clone()), cx);
        let _ = pre.cell_w;
    }
}

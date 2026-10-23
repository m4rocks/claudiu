//! Pixel-exact drawing for block elements (U+2580..259F) and box drawing (U+2500..257F).
//! Fonts leave gaps between rows/cells for these; terminals draw them as rectangles instead so
//! logos, borders and rules join seamlessly. Pure geometry, no GPUI types.

/// A rectangle inside a cell, in cell-local pixels, with an opacity multiplier for shades.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub alpha: f32,
}

fn r(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h, alpha: 1.0 }
}

/// Rectangles to fill for `ch` in a cell of `w` x `h` pixels, or `None` to fall back to the font.
pub fn rects(ch: char, w: f32, h: f32) -> Option<Vec<Rect>> {
    match ch as u32 {
        0x2580..=0x259F => block(ch as u32, w, h),
        0x2500..=0x257F => lines(ch as u32, w, h),
        _ => None,
    }
}

fn block(c: u32, w: f32, h: f32) -> Option<Vec<Rect>> {
    let (hw, hh) = ((w / 2.0).round(), (h / 2.0).round());
    // quadrant bits: UL=1, UR=2, LL=4, LR=8
    let quad = |bits: u8| {
        let mut v = Vec::new();
        if bits & 1 != 0 {
            v.push(r(0.0, 0.0, hw, hh));
        }
        if bits & 2 != 0 {
            v.push(r(hw, 0.0, w - hw, hh));
        }
        if bits & 4 != 0 {
            v.push(r(0.0, hh, hw, h - hh));
        }
        if bits & 8 != 0 {
            v.push(r(hw, hh, w - hw, h - hh));
        }
        Some(v)
    };
    match c {
        0x2580 => Some(vec![r(0.0, 0.0, w, hh)]),
        0x2581..=0x2588 => {
            let n = (c - 0x2580) as f32;
            let bh = (h * n / 8.0).round();
            Some(vec![r(0.0, h - bh, w, bh)])
        }
        0x2589..=0x258F => {
            let n = (0x2590 - c) as f32; // 7..1 eighths from the left
            Some(vec![r(0.0, 0.0, (w * n / 8.0).round(), h)])
        }
        0x2590 => Some(vec![r(hw, 0.0, w - hw, h)]),
        0x2591 => Some(vec![Rect { alpha: 0.25, ..r(0.0, 0.0, w, h) }]),
        0x2592 => Some(vec![Rect { alpha: 0.5, ..r(0.0, 0.0, w, h) }]),
        0x2593 => Some(vec![Rect { alpha: 0.75, ..r(0.0, 0.0, w, h) }]),
        0x2594 => Some(vec![r(0.0, 0.0, w, (h / 8.0).round().max(1.0))]),
        0x2595 => {
            let bw = (w / 8.0).round().max(1.0);
            Some(vec![r(w - bw, 0.0, bw, h)])
        }
        0x2596 => quad(4),
        0x2597 => quad(8),
        0x2598 => quad(1),
        0x2599 => quad(1 | 4 | 8),
        0x259A => quad(1 | 8),
        0x259B => quad(1 | 2 | 4),
        0x259C => quad(1 | 2 | 8),
        0x259D => quad(2),
        0x259E => quad(2 | 4),
        0x259F => quad(2 | 4 | 8),
        _ => None,
    }
}

/// (left, right, up, down) stroke weights: 0 none, 1 light, 2 heavy/double.
fn strokes(c: u32) -> Option<(u8, u8, u8, u8)> {
    Some(match c {
        0x2500 | 0x2504 | 0x2508 | 0x254C => (1, 1, 0, 0),
        0x2501 | 0x2505 | 0x2509 | 0x254D | 0x2550 => (2, 2, 0, 0),
        0x2502 | 0x2506 | 0x250A | 0x254E => (0, 0, 1, 1),
        0x2503 | 0x2507 | 0x250B | 0x254F | 0x2551 => (0, 0, 2, 2),
        0x250C | 0x256D => (0, 1, 0, 1),
        0x250D | 0x2552 => (0, 2, 0, 1),
        0x250E | 0x2553 => (0, 1, 0, 2),
        0x250F | 0x2554 => (0, 2, 0, 2),
        0x2510 | 0x256E => (1, 0, 0, 1),
        0x2511 | 0x2555 => (2, 0, 0, 1),
        0x2512 | 0x2556 => (1, 0, 0, 2),
        0x2513 | 0x2557 => (2, 0, 0, 2),
        0x2514 | 0x2570 => (0, 1, 1, 0),
        0x2515 | 0x2558 => (0, 2, 1, 0),
        0x2516 | 0x2559 => (0, 1, 2, 0),
        0x2517 | 0x255A => (0, 2, 2, 0),
        0x2518 | 0x256F => (1, 0, 1, 0),
        0x2519 | 0x255B => (2, 0, 1, 0),
        0x251A | 0x255C => (1, 0, 2, 0),
        0x251B | 0x255D => (2, 0, 2, 0),
        0x251C => (0, 1, 1, 1),
        0x251D | 0x255E => (0, 2, 1, 1),
        0x251E => (0, 1, 2, 1),
        0x251F => (0, 1, 1, 2),
        0x2520 | 0x255F => (0, 1, 2, 2),
        0x2521 => (0, 2, 2, 1),
        0x2522 => (0, 2, 1, 2),
        0x2523 | 0x2560 => (0, 2, 2, 2),
        0x2524 => (1, 0, 1, 1),
        0x2525 | 0x2561 => (2, 0, 1, 1),
        0x2526 => (1, 0, 2, 1),
        0x2527 => (1, 0, 1, 2),
        0x2528 | 0x2562 => (1, 0, 2, 2),
        0x2529 => (2, 0, 2, 1),
        0x252A => (2, 0, 1, 2),
        0x252B | 0x2563 => (2, 0, 2, 2),
        0x252C => (1, 1, 0, 1),
        0x252D => (2, 1, 0, 1),
        0x252E => (1, 2, 0, 1),
        0x252F | 0x2564 => (2, 2, 0, 1),
        0x2530 | 0x2565 => (1, 1, 0, 2),
        0x2531 => (2, 1, 0, 2),
        0x2532 => (1, 2, 0, 2),
        0x2533 | 0x2566 => (2, 2, 0, 2),
        0x2534 => (1, 1, 1, 0),
        0x2535 => (2, 1, 1, 0),
        0x2536 => (1, 2, 1, 0),
        0x2537 | 0x2567 => (2, 2, 1, 0),
        0x2538 | 0x2568 => (1, 1, 2, 0),
        0x2539 => (2, 1, 2, 0),
        0x253A => (1, 2, 2, 0),
        0x253B | 0x2569 => (2, 2, 2, 0),
        0x253C => (1, 1, 1, 1),
        0x253D => (2, 1, 1, 1),
        0x253E => (1, 2, 1, 1),
        0x253F | 0x256A => (2, 2, 1, 1),
        0x2540 => (1, 1, 2, 1),
        0x2541 => (1, 1, 1, 2),
        0x2542 | 0x256B => (1, 1, 2, 2),
        0x2543 => (2, 1, 2, 1),
        0x2544 => (1, 2, 2, 1),
        0x2545 => (2, 1, 1, 2),
        0x2546 => (1, 2, 1, 2),
        0x2547 => (2, 2, 2, 1),
        0x2548 => (2, 2, 1, 2),
        0x2549 => (2, 1, 2, 2),
        0x254A => (1, 2, 2, 2),
        0x254B | 0x256C => (2, 2, 2, 2),
        0x2574 => (1, 0, 0, 0),
        0x2575 => (0, 0, 1, 0),
        0x2576 => (0, 1, 0, 0),
        0x2577 => (0, 0, 0, 1),
        0x2578 => (2, 0, 0, 0),
        0x2579 => (0, 0, 2, 0),
        0x257A => (0, 2, 0, 0),
        0x257B => (0, 0, 0, 2),
        0x257C => (1, 2, 0, 0),
        0x257D => (0, 0, 1, 2),
        0x257E => (2, 1, 0, 0),
        0x257F => (0, 0, 2, 1),
        _ => return None, // diagonals etc.: let the font draw them
    })
}

fn lines(c: u32, w: f32, h: f32) -> Option<Vec<Rect>> {
    let (l, rt, u, d) = strokes(c)?;
    let light = (w / 8.0).round().max(1.0);
    let thick = |weight: u8| if weight >= 2 { light * 2.0 } else { light };
    let cx = (w / 2.0).floor();
    let cy = (h / 2.0).floor();
    // Horizontal strokes are centered on cy, vertical ones on cx; each reaches the cell edge.
    let h_t = thick(l.max(rt));
    let v_t = thick(u.max(d));
    let y0 = (cy - (h_t / 2.0).floor()).max(0.0);
    let x0 = (cx - (v_t / 2.0).floor()).max(0.0);
    let mut out = Vec::new();
    // Arms meet through the junction so corners have no notch.
    if l > 0 {
        out.push(r(0.0, cy - (thick(l) / 2.0).floor(), x0 + v_t.max(thick(l).min(v_t)), thick(l)));
    }
    if rt > 0 {
        out.push(r(x0, cy - (thick(rt) / 2.0).floor(), w - x0, thick(rt)));
    }
    if u > 0 {
        out.push(r(cx - (thick(u) / 2.0).floor(), 0.0, thick(u), y0 + h_t.max(thick(u).min(h_t))));
    }
    if d > 0 {
        out.push(r(cx - (thick(d) / 2.0).floor(), y0, thick(d), h - y0));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_and_half_blocks() {
        assert_eq!(rects('█', 8.0, 18.0).unwrap(), vec![r(0.0, 0.0, 8.0, 18.0)]);
        assert_eq!(rects('▀', 8.0, 18.0).unwrap(), vec![r(0.0, 0.0, 8.0, 9.0)]);
        assert_eq!(rects('▄', 8.0, 18.0).unwrap(), vec![r(0.0, 9.0, 8.0, 9.0)]);
        assert_eq!(rects('▌', 8.0, 18.0).unwrap(), vec![r(0.0, 0.0, 4.0, 18.0)]);
        assert_eq!(rects('▐', 8.0, 18.0).unwrap(), vec![r(4.0, 0.0, 4.0, 18.0)]);
    }

    #[test]
    fn quadrants_cover_expected_cells() {
        // ▛ = upper-left + upper-right + lower-left (the Clawd logo uses these)
        let q = rects('▛', 8.0, 18.0).unwrap();
        assert_eq!(q.len(), 3);
        let area: f32 = q.iter().map(|r| r.w * r.h).sum();
        assert_eq!(area, 8.0 * 18.0 * 0.75);
    }

    #[test]
    fn shades_are_translucent_fills() {
        assert_eq!(rects('▒', 8.0, 16.0).unwrap()[0].alpha, 0.5);
    }

    #[test]
    fn horizontal_rule_spans_the_whole_cell_so_neighbours_join() {
        let v = rects('─', 8.0, 18.0).unwrap();
        assert_eq!(v.len(), 2); // left arm + right arm
        let min_x = v.iter().map(|r| r.x).fold(f32::MAX, f32::min);
        let max_x = v.iter().map(|r| r.x + r.w).fold(0.0, f32::max);
        assert_eq!((min_x, max_x), (0.0, 8.0));
    }

    #[test]
    fn corner_has_two_arms_and_junction_is_covered() {
        let v = rects('┌', 8.0, 18.0).unwrap();
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn unhandled_chars_fall_back_to_font() {
        assert!(rects('a', 8.0, 18.0).is_none());
        assert!(rects('╱', 8.0, 18.0).is_none());
    }
}

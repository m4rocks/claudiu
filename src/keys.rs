//! GPUI keystroke -> VT bytes. Pure function so it can be unit-tested.
//! Everything not handled here is forwarded to the program unchanged, so Claude Code
//! shortcuts (Alt+P, Shift+Tab, ...) reach it exactly as they would in any terminal.

use alacritty_terminal::term::TermMode;
use gpui::Keystroke;

/// xterm modifier parameter: 1 + shift + 2*alt + 4*ctrl.
fn mod_param(ks: &Keystroke) -> u8 {
    let m = &ks.modifiers;
    1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.control as u8
}

fn csi(final_byte: char, mods: u8) -> Vec<u8> {
    if mods == 1 {
        format!("\x1b[{final_byte}").into_bytes()
    } else {
        format!("\x1b[1;{mods}{final_byte}").into_bytes()
    }
}

fn ss3_or_csi(final_byte: char, mods: u8, app_cursor: bool) -> Vec<u8> {
    if mods == 1 && app_cursor {
        format!("\x1bO{final_byte}").into_bytes()
    } else {
        csi(final_byte, mods)
    }
}

fn tilde(n: u8, mods: u8) -> Vec<u8> {
    if mods == 1 {
        format!("\x1b[{n}~").into_bytes()
    } else {
        format!("\x1b[{n};{mods}~").into_bytes()
    }
}

pub fn encode(ks: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    let m = &ks.modifiers;
    if m.platform {
        return None;
    }
    let mods = mod_param(ks);
    let app = mode.contains(TermMode::APP_CURSOR);

    // AltGr arrives as ctrl+alt with a produced character: it's plain text.
    if m.control && m.alt
        && let Some(text) = ks.key_char.as_ref().filter(|t| !t.is_empty()) {
            return Some(text.clone().into_bytes());
        }

    let alt_prefix = |mut bytes: Vec<u8>| {
        if m.alt {
            bytes.insert(0, 0x1b);
        }
        bytes
    };

    let named = match ks.key.as_str() {
        "enter" => Some(if m.control {
            b"\n".to_vec()
        } else if m.alt || m.shift {
            // ESC CR: what Claude Code documents as the newline binding for terminals without kitty keys.
            b"\x1b\r".to_vec()
        } else {
            b"\r".to_vec()
        }),
        "tab" => Some(if m.shift { b"\x1b[Z".to_vec() } else { alt_prefix(b"\t".to_vec()) }),
        "escape" => Some(alt_prefix(vec![0x1b])),
        "backspace" => Some(if m.control { vec![0x08] } else { alt_prefix(vec![0x7f]) }),
        "space" => Some(if m.control { vec![0] } else { alt_prefix(b" ".to_vec()) }),
        "up" => Some(ss3_or_csi('A', mods, app)),
        "down" => Some(ss3_or_csi('B', mods, app)),
        "right" => Some(ss3_or_csi('C', mods, app)),
        "left" => Some(ss3_or_csi('D', mods, app)),
        "home" => Some(ss3_or_csi('H', mods, app)),
        "end" => Some(ss3_or_csi('F', mods, app)),
        "insert" => Some(tilde(2, mods)),
        "delete" => Some(tilde(3, mods)),
        "pageup" => Some(tilde(5, mods)),
        "pagedown" => Some(tilde(6, mods)),
        "f1" => Some(ss3_or_csi('P', mods, true)),
        "f2" => Some(ss3_or_csi('Q', mods, true)),
        "f3" => Some(ss3_or_csi('R', mods, true)),
        "f4" => Some(ss3_or_csi('S', mods, true)),
        "f5" => Some(tilde(15, mods)),
        "f6" => Some(tilde(17, mods)),
        "f7" => Some(tilde(18, mods)),
        "f8" => Some(tilde(19, mods)),
        "f9" => Some(tilde(20, mods)),
        "f10" => Some(tilde(21, mods)),
        "f11" => Some(tilde(23, mods)),
        "f12" => Some(tilde(24, mods)),
        _ => None,
    };
    if named.is_some() {
        return named;
    }

    let mut chars = ks.key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        // Multi-char key name we don't know (e.g. "capslock"): fall back to any produced text.
        return ks.key_char.as_ref().filter(|t| !t.is_empty()).map(|t| t.clone().into_bytes());
    };

    if m.control {
        let byte = match c.to_ascii_lowercase() {
            l @ 'a'..='z' => l as u8 - b'a' + 1,
            '[' => 0x1b,
            '\\' => 0x1c,
            ']' => 0x1d,
            '^' | '6' => 0x1e,
            '_' | '-' | '/' => 0x1f,
            '@' | '2' | ' ' => 0,
            '?' => 0x7f,
            _ => return None,
        };
        return Some(alt_prefix(vec![byte]));
    }

    if m.alt {
        let text = match &ks.key_char {
            Some(t) if !t.is_empty() && t.is_ascii() => t.clone(),
            _ if m.shift => c.to_uppercase().collect(),
            _ => c.to_string(),
        };
        return Some(alt_prefix(text.into_bytes()));
    }

    ks.key_char.as_ref().filter(|t| !t.is_empty()).map(|t| t.clone().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    fn ks(key: &str, ch: Option<&str>, f: impl FnOnce(&mut Modifiers)) -> Keystroke {
        let mut modifiers = Modifiers::default();
        f(&mut modifiers);
        Keystroke { modifiers, key: key.into(), key_char: ch.map(Into::into) }
    }
    fn enc(k: Keystroke) -> Vec<u8> {
        encode(&k, TermMode::NONE).unwrap()
    }

    #[test]
    fn alt_p_is_esc_p() {
        assert_eq!(enc(ks("p", None, |m| m.alt = true)), b"\x1bp");
        assert_eq!(enc(ks("p", Some("p"), |m| m.alt = true)), b"\x1bp");
    }

    #[test]
    fn shift_tab_is_backtab() {
        assert_eq!(enc(ks("tab", None, |m| m.shift = true)), b"\x1b[Z");
    }

    #[test]
    fn control_letters() {
        assert_eq!(enc(ks("c", None, |m| m.control = true)), [3]);
        assert_eq!(enc(ks("d", None, |m| m.control = true)), [4]);
        assert_eq!(enc(ks("z", None, |m| m.control = true)), [26]);
    }

    #[test]
    fn arrows_respect_app_cursor_and_modifiers() {
        let up = ks("up", None, |_| {});
        assert_eq!(encode(&up, TermMode::NONE).unwrap(), b"\x1b[A");
        assert_eq!(encode(&up, TermMode::APP_CURSOR).unwrap(), b"\x1bOA");
        assert_eq!(enc(ks("left", None, |m| m.control = true)), b"\x1b[1;5D");
        assert_eq!(enc(ks("right", None, |m| m.shift = true)), b"\x1b[1;2C");
        assert_eq!(enc(ks("home", None, |_| {})), b"\x1b[H");
    }

    #[test]
    fn text_and_altgr() {
        assert_eq!(enc(ks("a", Some("a"), |_| {})), b"a");
        assert_eq!(enc(ks("a", Some("A"), |m| m.shift = true)), b"A");
        assert_eq!(enc(ks("q", Some("@"), |m| { m.control = true; m.alt = true })), b"@");
        assert_eq!(enc(ks("é", Some("é"), |_| {})), "é".as_bytes());
    }

    #[test]
    fn editing_keys() {
        assert_eq!(enc(ks("enter", None, |_| {})), b"\r");
        assert_eq!(enc(ks("enter", None, |m| m.shift = true)), b"\x1b\r");
        assert_eq!(enc(ks("backspace", None, |_| {})), [0x7f]);
        assert_eq!(enc(ks("delete", None, |_| {})), b"\x1b[3~");
        assert_eq!(enc(ks("f5", None, |_| {})), b"\x1b[15~");
        assert_eq!(enc(ks("escape", None, |_| {})), [0x1b]);
    }

    #[test]
    fn platform_key_is_not_forwarded() {
        assert!(encode(&ks("c", None, |m| m.platform = true), TermMode::NONE).is_none());
    }
}

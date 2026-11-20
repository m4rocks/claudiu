//! Computer use for sessions on Windows: screenshots, clicks, typing and window focus, served by Claudiu's MCP
//! server (`claude/mcp.rs`). Claude Code ships its own computer-use server only on macOS, so without these a session
//! on Windows has no way to look at or drive an app.
//!
//! Elsewhere the tools are not listed, and a call that names one anyway gets "only on Windows".

// Off Windows only the tests use the parsing and window picking; they stay compiled there so the tests run on every OS.
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::Path;

use serde_json::{Value, json};

pub const TOOLS: [&str; 6] = ["list_windows", "focus_window", "screenshot", "click", "type_text", "press_keys"];

pub const SUPPORTED: bool = cfg!(windows);

/// What makes the model reach for the tools instead of saying it can't see the app.
pub const INSTRUCTIONS: &str = " Computer use: list_windows, focus_window, screenshot, click, type_text and press_keys \
take screenshots of windows or the screen and drive the mouse and keyboard on this Windows desktop. Use them to look at \
and check apps you build or run; screenshot saves a PNG that you open with Read. They act at once on the user's real \
desktop, so only click and type in windows the task is about. Load them first with ToolSearch \
(select:mcp__claudiu__screenshot,mcp__claudiu__click) if needed.";

/// The tools' schemas, or none where they don't work.
pub fn tools() -> Vec<Value> {
    if !SUPPORTED {
        return vec![];
    }
    let window = || {
        json!({
            "window_id": { "type": "integer", "description": "The id from list_windows" },
            "title": { "type": "string", "description": "Part of the window's title, any case" },
            "process": { "type": "string", "description": "The program's file name, like claudiu or claudiu.exe" },
        })
    };
    let with_window = |mut properties: Value| {
        for (k, v) in window().as_object().unwrap() {
            properties[k] = v.clone();
        }
        properties
    };
    let tool = |name: &str, description: &str, properties: Value, required: &[&str]| {
        json!({ "name": name, "description": description, "inputSchema": { "type": "object", "properties": properties, "required": required } })
    };
    const PICK: &str = " When several windows match, the frontmost is used.";
    vec![
        tool(
            "list_windows",
            "List the visible top-level windows on this desktop, frontmost first: id, process, title and position in screen pixels.",
            json!({ "query": { "type": "string", "description": "Only windows whose title or process contains this, any case (optional)" } }),
            &[],
        ),
        tool("focus_window", &format!("Bring a window to the front and give it the keyboard, restoring it if minimized.{PICK}"), window(), &[]),
        tool(
            "screenshot",
            &format!(
                "Save a PNG screenshot of a window (brought to the front first), or of the whole screen when no window is named, and return its path; open it with Read. The image is in physical pixels, so x and y read off it are what click takes.{PICK}"
            ),
            window(),
            &[],
        ),
        tool(
            "click",
            &format!(
                "Click the mouse at x, y. With a window named, x and y count from its top-left corner, as in its screenshot, and the window is brought to the front first; without one, they are screen pixels, as in a full-screen screenshot.{PICK}"
            ),
            with_window(json!({
                "x": { "type": "integer" },
                "y": { "type": "integer" },
                "button": { "type": "string", "enum": ["left", "right", "middle"], "description": "Default left" },
                "double": { "type": "boolean", "description": "Double-click (default false)" },
            })),
            &["x", "y"],
        ),
        tool(
            "type_text",
            &format!("Type text into the window that has the keyboard, or into the named window after bringing it to the front. Newlines press Enter.{PICK}"),
            with_window(json!({ "text": { "type": "string" } })),
            &["text"],
        ),
        tool(
            "press_keys",
            &format!(
                "Press keys or shortcuts in order, like [\"ctrl+s\"] or [\"escape\", \"alt+f4\"]. Keys: ctrl, shift, alt, win, enter, tab, escape, backspace, delete, insert, space, up, down, left, right, home, end, pageup, pagedown, f1-f24, a-z and 0-9. Type other characters with type_text.{PICK}"
            ),
            with_window(json!({ "keys": { "type": "array", "items": { "type": "string" } } })),
            &["keys"],
        ),
    ]
}

/// Run one of `TOOLS`. Screenshots go into `screenshots`. Arguments are checked before anything on the desktop moves.
pub fn call(tool: &str, args: &Value, screenshots: &Path) -> Result<String, String> {
    if !SUPPORTED {
        return Err("Computer use works only on Windows. On macOS, Claude Code has its own computer use.".into());
    }
    let request = Request::parse(tool, args)?;
    #[cfg(windows)]
    return win::run(request, screenshots);
    #[cfg(not(windows))]
    {
        let _ = (request, screenshots);
        unreachable!("SUPPORTED is false off Windows")
    }
}

/// A tool call whose arguments were checked.
#[derive(Debug, PartialEq)]
enum Request {
    List { query: Option<String> },
    Focus(Selector),
    Screenshot(Option<Selector>),
    Click { window: Option<Selector>, x: i32, y: i32, button: Button, double: bool },
    Type { window: Option<Selector>, text: String },
    Keys { window: Option<Selector>, combos: Vec<Vec<Key>> },
}

impl Request {
    fn parse(tool: &str, args: &Value) -> Result<Request, String> {
        let window = Selector::parse(args)?;
        Ok(match tool {
            "list_windows" => Request::List { query: non_empty(&args["query"]) },
            "focus_window" => Request::Focus(window.ok_or("name the window with window_id, title or process")?),
            "screenshot" => Request::Screenshot(window),
            "click" => {
                let coordinate = |v: &Value| v.as_i64().and_then(|n| i32::try_from(n).ok());
                let (Some(x), Some(y)) = (coordinate(&args["x"]), coordinate(&args["y"])) else {
                    return Err("x and y must be whole numbers of pixels".into());
                };
                let button = match args["button"].as_str().unwrap_or("left") {
                    "left" => Button::Left,
                    "right" => Button::Right,
                    "middle" => Button::Middle,
                    _ => return Err("button must be left, right or middle".into()),
                };
                Request::Click { window, x, y, button, double: args["double"].as_bool().unwrap_or(false) }
            }
            "type_text" => match args["text"].as_str() {
                Some(text) if !text.is_empty() => Request::Type { window, text: text.to_string() },
                _ => return Err("text must not be empty".into()),
            },
            "press_keys" => {
                let combos = args["keys"].as_array().ok_or("keys must be a list of keys, like [\"ctrl+s\"]")?;
                if combos.is_empty() {
                    return Err("keys must list at least one key".into());
                }
                let combos = combos.iter().map(|c| c.as_str().ok_or_else(|| format!("{c} is not a key")).and_then(parse_combo)).collect::<Result<_, _>>()?;
                Request::Keys { window, combos }
            }
            _ => return Err("unknown tool".into()),
        })
    }
}

fn non_empty(v: &Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Which window a call means. Every part given must match.
#[derive(Debug, PartialEq)]
struct Selector {
    id: Option<isize>,
    title: Option<String>,
    process: Option<String>,
}

impl Selector {
    /// `None` when the call names no window.
    fn parse(args: &Value) -> Result<Option<Selector>, String> {
        let id = match &args["window_id"] {
            Value::Null => None,
            v => Some(v.as_i64().and_then(|n| isize::try_from(n).ok()).ok_or("window_id must be an id from list_windows")?),
        };
        let (title, process) = (non_empty(&args["title"]), non_empty(&args["process"]));
        Ok((id.is_some() || title.is_some() || process.is_some()).then_some(Selector { id, title, process }))
    }

    fn matches(&self, w: &Window) -> bool {
        self.id.is_none_or(|id| id == w.id)
            && self.title.as_ref().is_none_or(|t| w.title.to_lowercase().contains(&t.to_lowercase()))
            && self.process.as_ref().is_none_or(|p| same_program(&w.process, p))
    }
}

/// `claudiu`, `claudiu.exe` and `Claudiu.EXE` all name `claudiu.exe`.
fn same_program(process: &str, wanted: &str) -> bool {
    let strip = |s: &str| {
        let s = s.to_lowercase();
        s.strip_suffix(".exe").map(str::to_string).unwrap_or(s)
    };
    strip(process) == strip(wanted)
}

/// A visible top-level window. `left`/`top`/`right`/`bottom` are physical screen pixels of what is drawn, without
/// the invisible resize border.
#[derive(Clone, Debug, PartialEq)]
struct Window {
    id: isize,
    title: String,
    process: String,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    minimized: bool,
    /// Belongs to the Claudiu running this session (see `HOST`).
    host: bool,
}

impl Window {
    fn line(&self) -> String {
        let state = if self.minimized { " (minimized)" } else { "" };
        let host = if self.host { " (the Claudiu running this session: screenshots only)" } else { "" };
        format!("{} · {} · \"{}\" · {}x{} at {},{}{state}{host}", self.id, self.process, self.title, self.right - self.left, self.bottom - self.top, self.left, self.top)
    }
}

/// Clicking or typing into the Claudiu that runs this session would type into the session's own terminal, which
/// Claudiu does only for selection references (CLAUDE.md, Boundaries).
const HOST: &str = "that is the Claudiu running this session, and computer use never clicks or types there";

/// The frontmost of `windows` (listed frontmost first) that `selector` means; for `input`, never the host's.
fn pick<'a>(windows: &'a [Window], selector: &Selector, input: bool) -> Result<&'a Window, String> {
    let mut matching = windows.iter().filter(|w| selector.matches(w));
    let first = matching.clone().next().ok_or("no visible window matches; list_windows shows what is open")?;
    if !input {
        return Ok(first);
    }
    matching.find(|w| !w.host).ok_or_else(|| HOST.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Button {
    Left,
    Right,
    Middle,
}

/// A Windows virtual-key code, and whether it is one of the extended keys (arrows, Home, Delete and the like) that
/// apps tell apart from their number-pad twins by a flag.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Key {
    code: u16,
    extended: bool,
}

/// `"ctrl+shift+t"` into its keys, held in order and let go in reverse.
fn parse_combo(combo: &str) -> Result<Vec<Key>, String> {
    let keys: Vec<Key> = combo
        .split('+')
        .map(|name| key(&name.trim().to_lowercase()).ok_or_else(|| format!("unknown key \"{}\" in \"{combo}\"", name.trim())))
        .collect::<Result<_, _>>()?;
    Ok(keys)
}

fn key(name: &str) -> Option<Key> {
    let plain = |code| Some(Key { code, extended: false });
    let extended = |code| Some(Key { code, extended: true });
    match name {
        "ctrl" | "control" => plain(0x11),
        "shift" => plain(0x10),
        "alt" => plain(0x12),
        "win" | "windows" | "meta" => extended(0x5B),
        "enter" | "return" => plain(0x0D),
        "tab" => plain(0x09),
        "escape" | "esc" => plain(0x1B),
        "backspace" => plain(0x08),
        "space" => plain(0x20),
        "pageup" => extended(0x21),
        "pagedown" => extended(0x22),
        "end" => extended(0x23),
        "home" => extended(0x24),
        "left" => extended(0x25),
        "up" => extended(0x26),
        "right" => extended(0x27),
        "down" => extended(0x28),
        "insert" => extended(0x2D),
        "delete" | "del" => extended(0x2E),
        _ => {
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                // Letters and digits: their virtual-key codes are the upper-case ASCII codes.
                (Some(c), None) if c.is_ascii_alphanumeric() => plain(c.to_ascii_uppercase() as u16),
                _ => match name.strip_prefix('f').and_then(|n| n.parse::<u16>().ok()) {
                    Some(n @ 1..=24) => plain(0x70 + n - 1),
                    _ => None,
                },
            }
        }
    }
}

#[cfg(windows)]
mod win {
    use std::path::Path;

    use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, POINT, RECT};
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SRCCOPY,
        SelectObject,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
    use windows_sys::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
        MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEINPUT, SendInput,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GA_ROOT, GetAncestor, GetForegroundWindow, GetSystemMetrics, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, SM_CXVIRTUALSCREEN,
        SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_RESTORE, SetCursorPos, SetForegroundWindow, ShowWindow, WindowFromPoint,
    };

    use super::{Button, HOST, Key, Request, Selector, Window, pick};

    pub fn run(request: Request, screenshots: &Path) -> Result<String, String> {
        // Coordinates, window bounds and captures all in physical pixels, whatever each monitor's scale. Failing
        // means it was already set, which is just as good.
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        match request {
            Request::List { query } => {
                let query = query.map(|q| q.to_lowercase());
                let lines: Vec<String> =
                    windows().iter().filter(|w| query.as_ref().is_none_or(|q| w.title.to_lowercase().contains(q) || w.process.to_lowercase().contains(q))).map(Window::line).collect();
                Ok(if lines.is_empty() { "No visible window matches.".into() } else { lines.join("\n") })
            }
            Request::Focus(selector) => {
                let w = front(&selector, true)?;
                Ok(format!("Focused {}", w.line()))
            }
            Request::Screenshot(selector) => {
                let (left, top, width, height, what) = match selector {
                    Some(selector) => {
                        let w = front(&selector, false)?;
                        (w.left, w.top, w.right - w.left, w.bottom - w.top, format!("window {}", w.line()))
                    }
                    None => unsafe {
                        (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN), GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN), "the screen".to_string())
                    },
                };
                let image = capture(left, top, width, height)?;
                let path = screenshots.join(format!("{}.png", crate::store::new_id()));
                std::fs::create_dir_all(screenshots).map_err(|e| format!("could not save the screenshot: {e}"))?;
                image.save(&path).map_err(|e| format!("could not save the screenshot: {e}"))?;
                Ok(format!("Saved a {width}x{height} screenshot of {what} to {}", path.display()))
            }
            Request::Click { window, x, y, button, double } => {
                let (dx, dy) = match window {
                    Some(selector) => {
                        let w = front(&selector, true)?;
                        (w.left, w.top)
                    }
                    None => (0, 0),
                };
                let (sx, sy) = (dx + x, dy + y);
                if is_host(unsafe { GetAncestor(WindowFromPoint(POINT { x: sx, y: sy }), GA_ROOT) }) {
                    return Err(HOST.into());
                }
                if unsafe { SetCursorPos(sx, sy) } == 0 {
                    return Err(format!("could not move the mouse to {sx},{sy}"));
                }
                let (down, up) = match button {
                    Button::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
                    Button::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
                    Button::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
                };
                let clicks = if double { 2 } else { 1 };
                let inputs: Vec<INPUT> = (0..clicks).flat_map(|_| [mouse(down), mouse(up)]).collect();
                send(&inputs)?;
                Ok(format!("Clicked at {sx},{sy} on the screen."))
            }
            Request::Type { window, text } => {
                keyboard_target(window)?;
                let mut inputs = vec![];
                for unit in text.replace("\r\n", "\n").encode_utf16() {
                    // Many apps ignore a typed newline or tab character, but all of them take the key.
                    let key = match unit {
                        0x0A => Some(Key { code: 0x0D, extended: false }),
                        0x09 => Some(Key { code: 0x09, extended: false }),
                        _ => None,
                    };
                    match key {
                        Some(key) => inputs.extend([keyboard(key, false), keyboard(key, true)]),
                        None => inputs.extend([unicode(unit, false), unicode(unit, true)]),
                    }
                }
                send(&inputs)?;
                Ok(format!("Typed {} characters.", text.chars().count()))
            }
            Request::Keys { window, combos } => {
                keyboard_target(window)?;
                for combo in &combos {
                    let inputs: Vec<INPUT> = combo.iter().map(|&k| keyboard(k, false)).chain(combo.iter().rev().map(|&k| keyboard(k, true))).collect();
                    send(&inputs)?;
                }
                Ok(format!("Pressed {}.", combos.len()))
            }
        }
    }

    /// Bring the named window to the front for typing, or check that the one already there may take it.
    fn keyboard_target(selector: Option<Selector>) -> Result<(), String> {
        match selector {
            Some(selector) => front(&selector, true).map(|_| ()),
            None if is_host(unsafe { GetForegroundWindow() }) => Err(HOST.into()),
            None => Ok(()),
        }
    }

    /// The process id `HOST_PID_ENV` names; none when the server runs outside Claudiu.
    fn host_pid() -> Option<u32> {
        std::env::var(crate::claude::mcp::HOST_PID_ENV).ok()?.parse().ok()
    }

    fn is_host(hwnd: HWND) -> bool {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        host_pid().is_some_and(|host| host == pid)
    }

    /// Bring the window `selector` means to the front, and return it as it is once there. For `input`, never the
    /// host's window.
    fn front(selector: &Selector, input: bool) -> Result<Window, String> {
        let w = pick(&windows(), selector, input)?.clone();
        let hwnd = w.id as HWND;
        unsafe {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            if GetForegroundWindow() != hwnd {
                // Windows hands the foreground only to a process that sent the latest input, and this server, in
                // the background under Claude Code, never has. An input that moves nothing makes it the latest
                // sender (what PowerToys does to switch windows).
                send(&[mouse(0)])?;
                SetForegroundWindow(hwnd);
            }
        }
        // Give the window a moment to come to the front and repaint before anything reads or clicks it.
        std::thread::sleep(std::time::Duration::from_millis(150));
        if unsafe { GetForegroundWindow() } != hwnd {
            return Err(format!("Windows did not let {} come to the front; click it once by hand and try again", w.line()));
        }
        // Restoring a minimized window moves it, so read its bounds again.
        windows().into_iter().find(|n| n.id == w.id).ok_or_else(|| "the window closed".to_string())
    }

    /// Visible, uncloaked top-level windows with a title, frontmost first.
    fn windows() -> Vec<Window> {
        unsafe extern "system" fn each(hwnd: HWND, found: LPARAM) -> windows_sys::core::BOOL {
            let found = unsafe { &mut *(found as *mut Vec<HWND>) };
            found.push(hwnd);
            1
        }
        let mut handles: Vec<HWND> = vec![];
        unsafe { EnumWindows(Some(each), &mut handles as *mut Vec<HWND> as LPARAM) };
        handles.into_iter().filter_map(describe).collect()
    }

    fn describe(hwnd: HWND) -> Option<Window> {
        unsafe {
            if IsWindowVisible(hwnd) == 0 {
                return None;
            }
            // Suspended Store apps and windows on other virtual desktops are "visible" but cloaked: not on screen.
            let mut cloaked = 0u32;
            DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED as u32, &mut cloaked as *mut u32 as *mut _, 4);
            if cloaked != 0 {
                return None;
            }
            let len = GetWindowTextLengthW(hwnd);
            if len <= 0 {
                return None;
            }
            let mut title = vec![0u16; len as usize + 1];
            let n = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
            let title = String::from_utf16_lossy(&title[..n.max(0) as usize]);
            let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            if DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS as u32, &mut r as *mut RECT as *mut _, std::mem::size_of::<RECT>() as u32) != 0 {
                return None;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            let host = host_pid() == Some(pid);
            Some(Window { id: hwnd as isize, title, process: process_name(pid), left: r.left, top: r.top, right: r.right, bottom: r.bottom, minimized: IsIconic(hwnd) != 0, host })
        }
    }

    /// The program's file name, or `?` when Windows won't say (an elevated process, for one).
    fn process_name(pid: u32) -> String {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return "?".into();
            }
            let mut buf = vec![0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) != 0;
            CloseHandle(handle);
            if !ok {
                return "?".into();
            }
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            path.rsplit('\\').next().unwrap_or(&path).to_string()
        }
    }

    /// Copy what the screen shows in a rectangle. Copying from the screen, not asking the window to paint itself,
    /// is what works for windows that draw with the GPU, like Claudiu's own.
    fn capture(left: i32, top: i32, width: i32, height: i32) -> Result<image::RgbaImage, String> {
        if width <= 0 || height <= 0 {
            return Err("the window has no size on screen".into());
        }
        unsafe {
            let screen = GetDC(std::ptr::null_mut());
            let memory = CreateCompatibleDC(screen);
            let bitmap = CreateCompatibleBitmap(screen, width, height);
            let previous = SelectObject(memory, bitmap);
            // CAPTUREBLT includes layered windows such as tooltips and menus.
            let copied = BitBlt(memory, 0, 0, width, height, screen, left, top, SRCCOPY | CAPTUREBLT) != 0;
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // Negative: rows top to bottom.
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            };
            let mut pixels = vec![0u8; width as usize * height as usize * 4];
            SelectObject(memory, previous);
            let rows = if copied { GetDIBits(memory, bitmap, 0, height as u32, pixels.as_mut_ptr() as *mut _, &mut info, DIB_RGB_COLORS) } else { 0 };
            DeleteObject(bitmap);
            DeleteDC(memory);
            ReleaseDC(std::ptr::null_mut(), screen);
            if rows != height {
                return Err("could not copy the screen".into());
            }
            // GDI hands back blue, green, red and an unused byte.
            for px in pixels.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
                px[3] = 255;
            }
            image::RgbaImage::from_raw(width as u32, height as u32, pixels).ok_or_else(|| "could not copy the screen".into())
        }
    }

    fn mouse(flags: u32) -> INPUT {
        INPUT { r#type: INPUT_MOUSE, Anonymous: INPUT_0 { mi: MOUSEINPUT { dx: 0, dy: 0, mouseData: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } } }
    }

    fn keyboard(key: Key, up: bool) -> INPUT {
        let flags = if key.extended { KEYEVENTF_EXTENDEDKEY } else { 0 } | if up { KEYEVENTF_KEYUP } else { 0 };
        INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: key.code, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } } }
    }

    fn unicode(unit: u16, up: bool) -> INPUT {
        let flags = KEYEVENTF_UNICODE | if up { KEYEVENTF_KEYUP } else { 0 };
        INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: 0, wScan: unit, dwFlags: flags, time: 0, dwExtraInfo: 0 } } }
    }

    /// Windows drops input aimed at a window that runs as administrator when this process doesn't, and says so only
    /// by sending fewer events than asked.
    fn send(inputs: &[INPUT]) -> Result<(), String> {
        let sent = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
        if sent as usize != inputs.len() {
            return Err("Windows blocked the input; the window may run as administrator, or the screen is locked".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(id: isize, title: &str, process: &str) -> Window {
        Window { id, title: title.into(), process: process.into(), left: 0, top: 0, right: 10, bottom: 10, minimized: false, host: false }
    }

    #[test]
    fn a_combo_holds_its_keys_in_order() {
        let k = |code, extended| Key { code, extended };
        assert_eq!(parse_combo("Ctrl+Shift+T"), Ok(vec![k(0x11, false), k(0x10, false), k(0x54, false)]));
        assert_eq!(parse_combo("enter"), Ok(vec![k(0x0D, false)]));
        assert_eq!(parse_combo("alt + f4"), Ok(vec![k(0x12, false), k(0x73, false)]));
        assert_eq!(parse_combo("f24"), Ok(vec![k(0x87, false)]));
        assert_eq!(parse_combo("ctrl+7"), Ok(vec![k(0x11, false), k(0x37, false)]));
        assert_eq!(parse_combo("delete"), Ok(vec![k(0x2E, true)]), "Delete, not the number pad's");
        assert!(parse_combo("f25").is_err());
        assert!(parse_combo("ctrl+").is_err(), "an empty key is not a key");
        assert!(parse_combo("ctrl+?").is_err(), "punctuation goes through type_text");
    }

    #[test]
    fn the_frontmost_window_that_matches_every_part_is_picked() {
        let windows = [window(3, "Claude Code - Notes", "Code.exe"), window(7, "Claudiu", "claudiu.exe"), window(9, "Claudiu", "claudiu.exe")];
        let sel = |args: Value| Selector::parse(&args).unwrap().unwrap();
        assert_eq!(pick(&windows, &sel(json!({"process": "Claudiu"})), false).unwrap().id, 7, "frontmost first, with or without .exe, any case");
        assert_eq!(pick(&windows, &sel(json!({"title": "claude"})), false).unwrap().id, 3);
        assert_eq!(pick(&windows, &sel(json!({"window_id": 9})), false).unwrap().id, 9);
        assert!(pick(&windows, &sel(json!({"title": "claude", "process": "claudiu"})), false).is_err(), "every part must match");
        assert!(pick(&windows, &sel(json!({"process": "claudiu.ex"})), false).is_err(), "the process is a whole file name");
        assert_eq!(Selector::parse(&json!({"title": "  "})), Ok(None), "a blank title names no window");
        assert_eq!(pick(&windows, &sel(json!({"process": "claudiu"})), true).unwrap().id, 7);
    }

    #[test]
    fn input_never_goes_to_the_claudiu_running_the_session() {
        let host = Window { host: true, ..window(2, "Claudiu", "claudiu.exe") };
        let windows = [host, window(5, "Set up Claudiu", "claudiu.exe")];
        let sel = |args: Value| Selector::parse(&args).unwrap().unwrap();
        assert_eq!(pick(&windows, &sel(json!({"process": "claudiu"})), false).unwrap().id, 2, "a screenshot may show it");
        assert_eq!(pick(&windows, &sel(json!({"process": "claudiu"})), true).unwrap().id, 5, "input skips to the next match");
        assert_eq!(pick(&windows, &sel(json!({"window_id": 2})), true), Err(HOST.to_string()));
        assert!(windows[0].line().ends_with("(the Claudiu running this session: screenshots only)"));
        assert!(Selector::parse(&json!({"window_id": "7"})).is_err());
    }

    #[test]
    fn arguments_become_requests() {
        assert_eq!(
            Request::parse("click", &json!({"x": 5, "y": 6, "button": "right", "double": true, "process": "claudiu"})),
            Ok(Request::Click { window: Some(Selector { id: None, title: None, process: Some("claudiu".into()) }), x: 5, y: 6, button: Button::Right, double: true })
        );
        assert_eq!(Request::parse("screenshot", &json!({})), Ok(Request::Screenshot(None)), "no window means the screen");
        assert_eq!(Request::parse("list_windows", &json!({"query": ""})), Ok(Request::List { query: None }));
        assert!(Request::parse("click", &json!({"x": 1.5, "y": 2})).is_err());
        assert!(Request::parse("click", &json!({"x": 5_000_000_000i64, "y": 2})).is_err());
        assert!(Request::parse("press_keys", &json!({"keys": "ctrl+s"})).is_err(), "keys is a list");
    }

    #[test]
    fn every_tool_has_a_schema_where_it_works() {
        let names: Vec<String> = tools().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, if SUPPORTED { TOOLS.to_vec() } else { vec![] });
    }
}

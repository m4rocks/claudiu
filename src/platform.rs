//! Everything OS-specific lives here so the rest of the app stays portable.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Program + args for the user's default interactive shell.
pub fn default_shell() -> (String, Vec<String>) {
    #[cfg(target_os = "windows")]
    {
        let program = find_executable("pwsh")
            .or_else(|| find_executable("powershell"))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "powershell.exe".into());
        (program, vec!["-NoLogo".into()])
    }
    #[cfg(not(target_os = "windows"))]
    {
        let program = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into());
        (program, vec!["-l".into()])
    }
}

pub fn shell_label(program: &str) -> String {
    let stem = Path::new(program).file_stem().and_then(|s| s.to_str()).unwrap_or(program);
    match stem.to_ascii_lowercase().as_str() {
        "pwsh" => "PowerShell 7".into(),
        "powershell" => "PowerShell".into(),
        "cmd" => "Command Prompt".into(),
        "zsh" => "zsh".into(),
        "bash" => "bash".into(),
        "fish" => "fish".into(),
        other => other.to_string(),
    }
}

pub fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf())
}

/// PATH lookup (honors PATHEXT on Windows).
pub fn find_executable(name: &str) -> Option<PathBuf> {
    which::which(name).ok()
}

/// Spawn a GUI/helper process fully detached and without flashing a console window.
pub fn spawn_detached(program: &Path, args: &[&OsStr]) -> std::io::Result<()> {
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    }
    cmd.spawn().map(|_| ())
}

/// Keep a helper process from flashing a console window (Windows only).
pub fn no_window(cmd: &mut Command) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(target_os = "windows"))]
    let _ = cmd;
}

/// Ask the OS to draw attention to the app while it is in the background: taskbar flash (Windows, until the
/// window is focused) or a dock bounce (macOS). Call only when the window is not active.
pub fn request_attention(window: &gpui::Window) {
    let _ = window;
    #[cfg(target_os = "windows")]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use windows_sys::Win32::UI::WindowsAndMessaging::{FLASHW_TIMERNOFG, FLASHW_TRAY, FLASHWINFO, FlashWindowEx};
        if let Ok(handle) = HasWindowHandle::window_handle(window)
            && let RawWindowHandle::Win32(h) = handle.as_raw()
        {
            let info = FLASHWINFO {
                cbSize: size_of::<FLASHWINFO>() as u32,
                hwnd: h.hwnd.get() as _,
                dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
                uCount: 0,
                dwTimeout: 0,
            };
            // SAFETY: `info` is fully initialised and outlives the call; the HWND is our own window.
            unsafe { FlashWindowEx(&info) };
        }
    }
    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSApplication, NSRequestUserAttentionType};
        if let Some(mtm) = MainThreadMarker::new() {
            NSApplication::sharedApplication(mtm).requestUserAttention(NSRequestUserAttentionType::InformationalRequest);
        }
    }
}

/// Strip the Windows verbatim prefix (`\?\`) so paths compare and display normally.
pub fn clean_path(path: &Path) -> PathBuf {
    dunce::simplified(path).to_path_buf()
}

/// Canonical display form: no verbatim prefix, native separators, no trailing separator.
pub fn normalize(path: &Path) -> PathBuf {
    clean_path(path).components().collect()
}

/// Path equality / containment that ignores case and separators on Windows.
pub fn path_key(path: &Path) -> String {
    let s = clean_path(path).to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/').to_string();
    if cfg!(windows) { s.to_lowercase() } else { s }
}

pub fn is_same_or_under(child: &Path, parent: &Path) -> bool {
    let (c, p) = (path_key(child), path_key(parent));
    c == p || c.starts_with(&format!("{p}/"))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Editor {
    VsCode,
    Zed,
}

impl Editor {
    pub const ALL: [Editor; 2] = [Editor::VsCode, Editor::Zed];

    pub fn label(self) -> &'static str {
        match self {
            Editor::VsCode => "Visual Studio Code",
            Editor::Zed => "Zed",
        }
    }

    /// Locate the editor's CLI launcher; PATH first, then well-known install locations.
    pub fn locate(self) -> Option<PathBuf> {
        let (names, extra): (&[&str], Vec<PathBuf>) = match self {
            Editor::VsCode => (&["code"], well_known(&[
                (r"%LOCALAPPDATA%\Programs\Microsoft VS Code\bin\code.cmd", ""),
                (r"%ProgramFiles%\Microsoft VS Code\bin\code.cmd", ""),
                ("", "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"),
                ("", "/usr/local/bin/code"),
            ])),
            Editor::Zed => (&["zed"], well_known(&[
                (r"%LOCALAPPDATA%\Programs\Zed\bin\zed.exe", ""),
                (r"%LOCALAPPDATA%\Programs\Zed\Zed.exe", ""),
                ("", "/Applications/Zed.app/Contents/MacOS/cli"),
                ("", "/usr/local/bin/zed"),
            ])),
        };
        names.iter().find_map(|n| find_executable(n)).or_else(|| extra.into_iter().find(|p| p.is_file()))
    }

    pub fn open(self, project: &Path) -> Result<(), String> {
        let exe = self.locate().ok_or_else(|| format!("{} was not found on this machine", self.label()))?;
        let is_script = exe.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
        let result = if is_script {
            spawn_detached(Path::new("cmd.exe"), &[OsStr::new("/c"), exe.as_os_str(), project.as_os_str()])
        } else {
            spawn_detached(&exe, &[project.as_os_str()])
        };
        result.map_err(|e| format!("Could not launch {}: {e}", self.label()))
    }
}

fn well_known(entries: &[(&str, &str)]) -> Vec<PathBuf> {
    entries
        .iter()
        .filter_map(|(win, unix)| {
            if cfg!(windows) && !win.is_empty() {
                Some(PathBuf::from(expand_env(win)))
            } else if !cfg!(windows) && !unix.is_empty() {
                Some(PathBuf::from(unix))
            } else {
                None
            }
        })
        .collect()
}

fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                out.push_str(&std::env::var(&after[..end]).unwrap_or_default());
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containment_ignores_separator_style_and_trailing_slash() {
        assert!(is_same_or_under(Path::new("C:/a/b/c"), Path::new("C:/a/b")));
        assert!(is_same_or_under(Path::new("C:/a/b"), Path::new("C:/a/b/")));
        assert!(!is_same_or_under(Path::new("C:/a/bc"), Path::new("C:/a/b")));
    }

    #[test]
    fn verbatim_prefix_is_stripped() {
        assert_eq!(clean_path(Path::new(r"\\?\C:\x\y")), PathBuf::from(r"C:\x\y"));
    }

    #[test]
    fn env_expansion() {
        unsafe { std::env::set_var("CLAUDIU_TEST_X", "val") };
        assert_eq!(expand_env("a%CLAUDIU_TEST_X%b"), "avalb");
    }

    #[test]
    fn shell_labels() {
        assert_eq!(shell_label(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"), "PowerShell");
        assert_eq!(shell_label("/bin/zsh"), "zsh");
    }
}

//! Everything OS-specific lives here so the rest of the app stays portable.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Program + args for new terminals: `custom` if set, else the user's default interactive shell.
pub fn default_shell(custom: Option<&str>) -> (String, Vec<String>) {
    #[cfg(target_os = "windows")]
    {
        let program = custom
            .map(String::from)
            .or_else(|| std::env::var("COMSPEC").ok())
            .filter(|s| !s.is_empty())
            .or_else(|| find_executable("cmd").map(|p| p.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "cmd.exe".into());
        (program, Vec::new())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let program = custom
            .map(String::from)
            .or_else(|| std::env::var("SHELL").ok())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/bin/zsh".into());
        (program, vec!["-l".into()])
    }
}

/// On macOS and Linux a desktop-launched app inherits a minimal PATH (no nvm, fnm or a Homebrew node), so Claude
/// Code hooks that call `node` fail. Run the program through the user's login shell so it sees their normal
/// environment. Windows is returned unchanged.
pub fn through_login_shell(program: String, args: Vec<String>) -> (String, Vec<String>) {
    #[cfg(unix)]
    {
        let shell = login_shell();
        let script = if shell.ends_with("/fish") { "exec $argv" } else { r#"exec "$@""# };
        let mut wrapped = vec!["-l".into(), "-i".into(), "-c".into(), script.into(), "claudiu".into(), program];
        wrapped.extend(args);
        (shell, wrapped)
    }
    #[cfg(not(unix))]
    {
        (program, args)
    }
}

/// `SHELL` when a desktop launch set it, otherwise the platform's usual login shell.
#[cfg(unix)]
fn login_shell() -> String {
    std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| {
        if cfg!(target_os = "macos") { "/bin/zsh" } else { "/bin/bash" }.into()
    })
}

/// The fixed-width font for terminals and code. GPUI matches family names exactly and, when one is missing, falls
/// back to a proportional UI font, which breaks a terminal's grid; so Linux uses the monospace family fontconfig
/// picks for the desktop instead of naming one that may not be installed.
pub fn monospace_font() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "Cascadia Mono"
    }
    #[cfg(target_os = "macos")]
    {
        "Menlo"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        static FAMILY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        FAMILY.get_or_init(|| fontconfig_monospace().unwrap_or_else(|| "DejaVu Sans Mono".into()))
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn fontconfig_monospace() -> Option<String> {
    let output = Command::new("fc-match").args(["--format=%{family[0]}", "monospace"]).stderr(Stdio::null()).output().ok()?;
    if !output.status.success() {
        return None;
    }
    family_from_fc_match(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(any(not(any(target_os = "windows", target_os = "macos")), test))]
fn family_from_fc_match(output: &str) -> Option<String> {
    Some(output.trim()).filter(|family| !family.is_empty()).map(String::from)
}

/// The window class Linux desktops match to the launcher entry (`StartupWMClass` in the AppImage's .desktop file) to
/// pick the dock and panel icon. Without it a window gets a generic one. Other platforms ignore it.
pub const APP_ID: &str = "Claudiu";

pub fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf())
}

/// PATH lookup (honors PATHEXT on Windows).
pub fn find_executable(name: &str) -> Option<PathBuf> {
    let found = which::which(name).ok();
    #[cfg(unix)]
    let found = found.or_else(|| {
        let path = LOGIN_PATH.lock().ok()?.clone()?;
        which::which_in(name, Some(path), std::env::current_dir().ok()?).ok()
    });
    found
}

/// The PATH of the user's login shell, as `reload_path` last read it. A desktop-launched app gets a minimal PATH, and
/// the environment of a running process is not changed here (`set_var` is unsafe beside other threads on Unix), so
/// `find_executable` looks here as well. Sessions already start through the login shell (`through_login_shell`).
#[cfg(unix)]
static LOGIN_PATH: std::sync::Mutex<Option<std::ffi::OsString>> = std::sync::Mutex::new(None);

/// Look for the PATH again, for a tool installed since Claudiu started or one that only the shell's startup files
/// put on the PATH. Windows reads the system and user PATH from the registry and uses it for this process and the
/// programs it starts. macOS and Linux ask the login shell (blocking, up to a few seconds). Call it off the UI thread.
pub fn reload_path() {
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
        let system = registry_path(HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment");
        let user = registry_path(HKEY_CURRENT_USER, "Environment");
        // Without either value the PATH stays as it is.
        if system.is_none() && user.is_none() {
            return;
        }
        let registry: Vec<PathBuf> = [system, user].into_iter().flatten().flat_map(|text| std::env::split_paths(&text).collect::<Vec<_>>()).collect();
        let current = std::env::var_os("PATH").unwrap_or_default();
        // SAFETY: on Windows `set_var` is safe to call from any thread (std's documentation says so).
        unsafe { std::env::set_var("PATH", rebuilt_path(&registry, &current)) };
    }
    #[cfg(unix)]
    {
        if let Some(text) = login_shell_path() {
            let shell_dirs: Vec<PathBuf> = std::env::split_paths(&text).collect();
            let current = std::env::var_os("PATH").unwrap_or_default();
            if let Ok(mut slot) = LOGIN_PATH.lock() {
                *slot = Some(rebuilt_path(&shell_dirs, &current));
            }
        }
    }
}

/// Ask the user's login shell for its PATH. Interactive (`-i`) so `.zshrc` runs too, which is where nvm and friends
/// add theirs. A bash login shell reads `.bashrc` only when its profile sources it, as the usual Linux skeleton
/// does, and that file stops unless the shell is interactive.
#[cfg(unix)]
fn login_shell_path() -> Option<std::ffi::OsString> {
    path_from_shell(std::ffi::OsStr::new(&login_shell()), &print_path_script(), std::time::Duration::from_secs(5))
}

#[cfg(any(unix, test))]
const PATH_BEGIN: &str = "<<claudiu-path";
#[cfg(any(unix, test))]
const PATH_END: &str = "claudiu-path>>";

/// A shell command that prints `$PATH` between the markers. `printf` and the quoting mean the same in sh, bash, zsh
/// and fish (which joins its PATH list with colons inside quotes).
#[cfg(any(unix, test))]
fn print_path_script() -> String {
    format!("printf '%s%s%s' '{PATH_BEGIN}' \"$PATH\" '{PATH_END}'")
}

/// Run `script` in `shell` as a login shell and return the PATH it printed. Startup files may print anything, so the
/// PATH sits between markers. A shell that has not answered after `wait` is killed and gives `None`, as does any shell
/// that fails to run it.
#[cfg(any(unix, test))]
fn path_from_shell(shell: &OsStr, script: &str, wait: std::time::Duration) -> Option<std::ffi::OsString> {
    use std::io::Read;
    let mut child = Command::new(shell)
        .args(["-l", "-i", "-c", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        let _ = sender.send(text);
    });
    let output = receiver.recv_timeout(wait);
    // Killing a shell that already ended is harmless, and it must not be left running if it did not.
    let _ = child.kill();
    let _ = child.wait();
    path_between_markers(&output.ok()?).map(std::ffi::OsString::from)
}

/// The text between the markers, ignoring whatever the shell's startup files printed around them.
#[cfg(any(unix, test))]
fn path_between_markers(output: &str) -> Option<&str> {
    let after = output.split_once(PATH_BEGIN)?.1;
    Some(after.split_once(PATH_END)?.0)
}

/// The registry's directories first, then whatever this process had that the registry lacks (for example folders a
/// launcher added for Claudiu alone). Each directory appears once.
#[cfg(any(windows, unix, test))]
fn rebuilt_path(registry: &[PathBuf], current: &OsStr) -> std::ffi::OsString {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for dir in registry.iter().cloned().chain(std::env::split_paths(current)) {
        if !dir.as_os_str().is_empty() && !dirs.iter().any(|seen| path_key(seen) == path_key(&dir)) {
            dirs.push(dir);
        }
    }
    std::env::join_paths(dirs).unwrap_or_else(|_| current.to_owned())
}

/// The `Path` value under a registry key, with `%VARIABLES%` already expanded.
#[cfg(target_os = "windows")]
fn registry_path(root: windows_sys::Win32::System::Registry::HKEY, subkey: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW};
    let subkey: Vec<u16> = subkey.encode_utf16().chain(Some(0)).collect();
    let name: Vec<u16> = "Path".encode_utf16().chain(Some(0)).collect();
    let kinds = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ;
    let mut bytes = 0u32;
    // SAFETY: both calls get NUL-terminated names and a buffer of the size they report.
    unsafe {
        if RegGetValueW(root, subkey.as_ptr(), name.as_ptr(), kinds, std::ptr::null_mut(), std::ptr::null_mut(), &mut bytes) != 0 {
            return None;
        }
        let mut text = vec![0u16; (bytes as usize).div_ceil(2)];
        bytes = (text.len() * 2) as u32;
        if RegGetValueW(root, subkey.as_ptr(), name.as_ptr(), kinds, std::ptr::null_mut(), text.as_mut_ptr().cast(), &mut bytes) != 0 {
            return None;
        }
        // The reported size includes the closing NUL.
        text.truncate((bytes as usize / 2).saturating_sub(1));
        Some(String::from_utf16_lossy(&text))
    }
}

/// A program typed by the user: an existing file, or a name found on PATH.
pub fn resolve_program(text: &str) -> Option<PathBuf> {
    let p = Path::new(text);
    if p.is_file() { Some(p.to_path_buf()) } else { find_executable(text) }
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

/// Put an image on the clipboard as a bitmap, which is what Claude Code's image paste reads on Windows (it asks
/// .NET's `Clipboard.GetImage`, and gpui's clipboard write only offers the registered `PNG` format). Returns whether
/// it worked; replaces whatever the clipboard held.
#[cfg(target_os = "windows")]
pub fn set_clipboard_bitmap(encoded: &[u8]) -> bool {
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    const CF_DIB: u32 = 8;
    // A DIB is a BMP file without its 14-byte file header.
    let mut bmp = Vec::new();
    let Ok(img) = image::load_from_memory(encoded) else { return false };
    if img.to_rgb8().write_to(&mut std::io::Cursor::new(&mut bmp), image::ImageFormat::Bmp).is_err() || bmp.len() <= 14 {
        return false;
    }
    let dib = &bmp[14..];
    unsafe {
        let mem = GlobalAlloc(GMEM_MOVEABLE, dib.len());
        if mem.is_null() {
            return false;
        }
        let dst = GlobalLock(mem).cast::<u8>();
        if dst.is_null() {
            GlobalFree(mem);
            return false;
        }
        std::ptr::copy_nonoverlapping(dib.as_ptr(), dst, dib.len());
        GlobalUnlock(mem);
        // Another program may hold the clipboard for a moment.
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(std::ptr::null_mut()) != 0 {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !opened {
            GlobalFree(mem);
            return false;
        }
        EmptyClipboard();
        // The clipboard owns the memory once this succeeds.
        let ok = !SetClipboardData(CF_DIB, mem).is_null();
        if !ok {
            GlobalFree(mem);
        }
        CloseClipboard();
        ok
    }
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
    fn a_rebuilt_path_lists_the_registry_first_and_keeps_the_rest_once() {
        let dir = |name: &str| PathBuf::from(name);
        let current = std::env::join_paths(["extra", "shared", "extra"]).unwrap();
        let rebuilt = rebuilt_path(&[dir("fresh"), dir("shared"), dir("")], &current);
        let dirs: Vec<PathBuf> = std::env::split_paths(&rebuilt).collect();
        assert_eq!(dirs, vec![dir("fresh"), dir("shared"), dir("extra")]);
        if cfg!(windows) {
            let rebuilt = rebuilt_path(&[dir("C:/Tools/Bin")], std::ffi::OsStr::new("c:\\tools\\bin\\"));
            assert_eq!(std::env::split_paths(&rebuilt).count(), 1, "the same folder in another case is one entry");
        }
    }

    #[test]
    fn a_shells_path_is_found_between_the_markers_whatever_else_it_printed() {
        let output = format!("Last login: today
Welcome!
{PATH_BEGIN}/usr/bin:/opt/tools{PATH_END}
bye
");
        assert_eq!(path_between_markers(&output), Some("/usr/bin:/opt/tools"));
        assert_eq!(path_between_markers(&format!("{PATH_BEGIN}{PATH_END}")), Some(""));
        assert_eq!(path_between_markers("the shell printed no markers"), None);
        assert_eq!(path_between_markers(&format!("{PATH_BEGIN}/usr/bin")), None, "a cut-off answer is not used");
    }

    /// A shell that takes `-l -i -c`: `/bin/sh` on macOS, Git's bash on Windows (not the WSL launcher in System32,
    /// which fails without a distribution). `None` when the machine has neither.
    fn posix_shell() -> Option<PathBuf> {
        let candidates = if cfg!(windows) { vec![PathBuf::from(expand_env(r"%ProgramFiles%\Git\bin\bash.exe"))] } else { vec![PathBuf::from("/bin/sh")] };
        candidates.into_iter().find(|p| p.is_file())
    }

    #[test]
    fn a_desktop_launch_starts_the_program_through_the_login_shell() {
        let (program, args) = through_login_shell("/usr/bin/claude".into(), vec!["--resume".into(), "abc".into()]);
        if cfg!(windows) {
            assert_eq!(program, "/usr/bin/claude");
            assert_eq!(args, ["--resume".to_string(), "abc".into()]);
            return;
        }
        let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| {
            if cfg!(target_os = "macos") { "/bin/zsh".into() } else { "/bin/bash".into() }
        });
        let script = if shell.ends_with("/fish") { "exec $argv" } else { r#"exec "$@""# };
        assert_eq!(program, shell);
        let expected: Vec<String> = ["-l", "-i", "-c", script, "claudiu", "/usr/bin/claude", "--resume", "abc"].map(String::from).to_vec();
        assert_eq!(args, expected);
    }

    /// A tool that is not on this process's PATH, sitting in a directory recorded as the login shell's.
    #[test]
    #[cfg(unix)]
    fn find_executable_uses_a_directory_only_the_login_shell_reported() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("claudiu-login-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let tool = dir.join("claudiu-login-path-probe");
        std::fs::write(&tool, b"#!/bin/sh\n").unwrap();
        let mut perms = std::fs::metadata(&tool).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tool, perms).unwrap();

        let previous = LOGIN_PATH.lock().unwrap().clone();
        *LOGIN_PATH.lock().unwrap() = Some(dir.as_os_str().to_os_string());
        let found = find_executable("claudiu-login-path-probe");
        *LOGIN_PATH.lock().unwrap() = previous;
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(found, Some(tool));
    }

    #[test]
    fn a_shell_answers_with_its_path_and_a_hung_one_is_given_up_on() {
        let Some(bash) = posix_shell() else { return };
        let asked = std::time::Instant::now();
        let path = path_from_shell(bash.as_os_str(), &print_path_script(), std::time::Duration::from_secs(20)).expect("bash printed its PATH");
        assert!(!path.is_empty());
        assert!(asked.elapsed() < std::time::Duration::from_secs(20), "it answered, not timed out");

        let hung = format!("sleep 30; {}", print_path_script());
        let asked = std::time::Instant::now();
        assert_eq!(path_from_shell(bash.as_os_str(), &hung, std::time::Duration::from_millis(500)), None);
        assert!(asked.elapsed() < std::time::Duration::from_secs(10), "the wait ended at the timeout, not when the shell did");

        assert_eq!(path_from_shell(OsStr::new("no-such-shell-xyz"), &print_path_script(), std::time::Duration::from_secs(1)), None);
    }

    #[test]
    #[cfg(windows)]
    fn the_registry_path_of_this_machine_can_be_read() {
        use windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE;
        let system = registry_path(HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment").expect("the system Path value exists");
        assert!(!system.contains('%'), "variables are expanded: {system}");
        assert!(system.to_lowercase().contains("system32"), "{system}");
    }

    #[test]
    fn typed_programs_resolve_only_when_they_exist() {
        let exe = std::env::current_exe().unwrap();
        assert_eq!(resolve_program(exe.to_str().unwrap()), Some(exe));
        assert_eq!(resolve_program("definitely-not-a-program-xyz"), None);
    }

    #[test]
    fn containment_ignores_separator_style_and_trailing_slash() {
        assert!(is_same_or_under(Path::new("C:/a/b/c"), Path::new("C:/a/b")));
        assert!(is_same_or_under(Path::new("C:/a/b"), Path::new("C:/a/b/")));
        assert!(!is_same_or_under(Path::new("C:/a/bc"), Path::new("C:/a/b")));
    }

    #[test]
    #[cfg(windows)]
    fn verbatim_prefix_is_stripped() {
        assert_eq!(clean_path(Path::new(r"\\?\C:\x\y")), PathBuf::from(r"C:\x\y"));
    }

    #[test]
    fn the_monospace_family_is_what_fc_match_printed() {
        assert_eq!(family_from_fc_match("DejaVu Sans Mono"), Some("DejaVu Sans Mono".to_string()));
        assert_eq!(family_from_fc_match("  Fira Mono\n"), Some("Fira Mono".to_string()));
        assert_eq!(family_from_fc_match(""), None, "no answer keeps the default");
        assert_eq!(family_from_fc_match(" \n"), None);
    }

    #[test]
    fn env_expansion() {
        unsafe { std::env::set_var("CLAUDIU_TEST_X", "val") };
        assert_eq!(expand_env("a%CLAUDIU_TEST_X%b"), "avalb");
    }
}

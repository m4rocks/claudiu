//! A best-effort reading of a shell command line for the guards.
//!
//! It splits a command into simple commands (on `;`, `&&`, `||`, `|`, `&` and newlines), honours quotes, and looks
//! only at what it can read plainly: redirect targets, the path arguments of common file commands, and a short list
//! of dangerous commands. Anything built from a variable or a subshell is not understood and is let through.

use std::path::{Path, PathBuf};

use crate::guard::{Allowed, home_dir, resolve};

pub enum Verdict {
    Ok,
    /// A dangerous command: the one-line refusal.
    Refuse(String),
    /// A write outside the project, with the offending path.
    Outside(PathBuf),
}

/// A token, plus whether it was quoted (a quoted `>` is text, not a redirect).
fn split(command: &str) -> Vec<Vec<String>> {
    let mut segments: Vec<Vec<String>> = vec![Vec::new()];
    let mut word = String::new();
    let mut in_word = false;
    let (mut single, mut double) = (false, false);
    let chars: Vec<char> = command.chars().collect();
    let mut i = 0;

    fn flush(segments: &mut [Vec<String>], word: &mut String, in_word: &mut bool) {
        if *in_word {
            segments.last_mut().unwrap().push(std::mem::take(word));
            *in_word = false;
        }
    }
    fn end_segment(segments: &mut Vec<Vec<String>>) {
        if !segments.last().unwrap().is_empty() {
            segments.push(Vec::new());
        }
    }

    while i < chars.len() {
        let c = chars[i];
        if single {
            if c == '\'' { single = false } else { word.push(c) }
        } else if double {
            if c == '"' { double = false } else { word.push(c) }
        } else {
            match c {
                '\'' => {
                    single = true;
                    in_word = true;
                }
                '"' => {
                    double = true;
                    in_word = true;
                }
                c if c.is_whitespace() && c != '\n' => flush(&mut segments, &mut word, &mut in_word),
                '\n' | ';' | '|' => {
                    flush(&mut segments, &mut word, &mut in_word);
                    end_segment(&mut segments);
                }
                '&' => {
                    // `&>` and `2>&1` belong to redirects; a lone or doubled `&` ends the command.
                    if chars.get(i + 1) == Some(&'>') {
                        flush(&mut segments, &mut word, &mut in_word);
                    } else {
                        flush(&mut segments, &mut word, &mut in_word);
                        end_segment(&mut segments);
                    }
                }
                '>' => {
                    // A file descriptor right before `>` is part of the operator, not an argument.
                    if in_word && word.chars().all(|d| d.is_ascii_digit()) {
                        word.clear();
                        in_word = false;
                    } else {
                        flush(&mut segments, &mut word, &mut in_word);
                    }
                    let mut op = String::from(">");
                    if chars.get(i + 1) == Some(&'>') {
                        op.push('>');
                        i += 1;
                    }
                    if chars.get(i + 1) == Some(&'&') {
                        // `>&2`: a duplicated descriptor, no file.
                        op.push('&');
                        i += 1;
                    }
                    segments.last_mut().unwrap().push(op);
                }
                c => {
                    word.push(c);
                    in_word = true;
                }
            }
        }
        i += 1;
    }
    flush(&mut segments, &mut word, &mut in_word);
    segments.retain(|s| !s.is_empty());
    segments
}

fn is_flag(token: &str) -> bool {
    token.starts_with('-') && token.len() > 1
}

/// `/s`, `/q`: a cmd.exe switch rather than a path.
fn is_slash_switch(token: &str) -> bool {
    token.len() == 2 && token.starts_with('/') && token.as_bytes()[1].is_ascii_alphabetic()
}

fn unknown(token: &str) -> bool {
    token.contains('$') || token.contains('%') || token.contains('`')
}

fn looks_like_path(token: &str) -> bool {
    let b = token.as_bytes();
    token.starts_with('/') || token.starts_with('\\') || token.starts_with('~') || token.split(['/', '\\']).any(|p| p == "..") || (b.len() > 2 && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\'))
}

fn expand_home(token: &str) -> PathBuf {
    match (token.strip_prefix('~'), home_dir()) {
        (Some(rest), Some(home)) => home.join(rest.trim_start_matches(['/', '\\'])),
        _ => PathBuf::from(token),
    }
}

/// The root of a drive or file system, or the user's home folder.
fn is_root_or_home(token: &str) -> bool {
    let t = token.trim_end_matches(['/', '\\']);
    let lower = token.to_lowercase();
    if t.is_empty() || matches!(token, "/*" | "~" | "~/" | "~/*" | "$HOME" | "$HOME/" | "$HOME/*" | "${HOME}" | "${HOME}/" | "${HOME}/*") {
        return true;
    }
    if lower.contains("%userprofile%") || lower.contains("$env:userprofile") || lower.contains("$env:homepath") {
        return true;
    }
    let b = token.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() && token[2..].trim_matches(['/', '\\', '*']).is_empty() {
        return true;
    }
    if let Some(home) = home_dir() {
        let path = Path::new(t.trim_end_matches('*').trim_end_matches(['/', '\\']));
        return path == home;
    }
    false
}

const RECURSIVE_DELETE: &[&str] = &["rm", "remove-item", "ri", "del", "erase", "rd", "rmdir"];
const PATH_ARGS: &[&str] = &[
    "rm", "rmdir", "mkdir", "touch", "chmod", "chown", "truncate", "del", "erase", "rd", "md", "remove-item", "ri", "new-item", "ni", "set-content", "add-content",
    "out-file", "clear-content", "mv", "move", "move-item", "mi", "tee", "ren", "rename-item",
];
const DEST_LAST: &[&str] = &["cp", "copy", "copy-item", "ln", "install", "xcopy", "robocopy"];

pub fn check(command: &str, allowed: &Allowed) -> Verdict {
    for tokens in split(command) {
        let mut at = 0;
        // Leading words that only wrap the real command.
        while at < tokens.len() && (matches!(tokens[at].as_str(), "sudo" | "command" | "env" | "time" | "nohup" | "exec" | "call") || (tokens[at].contains('=') && !tokens[at].starts_with('-') && !tokens[at].contains('/'))) {
            at += 1;
        }
        let Some(first) = tokens.get(at) else { continue };
        let name = first.rsplit(['/', '\\']).next().unwrap_or(first).to_lowercase();
        let name = name.strip_suffix(".exe").unwrap_or(&name).to_string();
        let args: Vec<&String> = tokens[at + 1..].iter().collect();

        if let Some(reason) = dangerous(&name, &args, &tokens) {
            return Verdict::Refuse(format!("Refused: {reason}. Do it on specific paths inside the project, or ask the user to run it themselves."));
        }
        if let Some(path) = writes_outside(&name, &args, &tokens, allowed) {
            return Verdict::Outside(path);
        }
    }
    Verdict::Ok
}

fn dangerous(name: &str, args: &[&String], all: &[String]) -> Option<String> {
    let flags: Vec<String> = args.iter().filter(|a| is_flag(a)).map(|a| a.to_lowercase()).collect();
    let non_flags: Vec<&String> = args.iter().copied().filter(|a| !is_flag(a) && !is_slash_switch(a)).collect();

    if RECURSIVE_DELETE.contains(&name) {
        let recursive = flags.iter().any(|f| f == "--recursive" || f == "-recurse" || f.starts_with("-recurs") || (!f.starts_with("--") && f.contains('r')))
            || args.iter().any(|a| a.eq_ignore_ascii_case("/s"));
        if (recursive || flags.iter().any(|f| f == "--no-preserve-root")) && non_flags.iter().any(|t| is_root_or_home(t)) {
            return Some("recursive delete of the home folder or a drive root".into());
        }
    }
    if name == "git" && all.iter().any(|t| t == "push") {
        let forced = args.iter().any(|a| {
            let a = a.as_str();
            a == "--force" || a.starts_with("--force=") || (is_flag(a) && !a.starts_with("--") && a.contains('f')) || (a.starts_with('+') && a.len() > 1)
        });
        if forced {
            return Some("force-push".into());
        }
    }
    let disk = name.starts_with("mkfs") || matches!(name, "diskpart" | "format-volume" | "clear-disk" | "initialize-disk" | "wipefs" | "fdisk" | "parted")
        || (name == "format" && non_flags.iter().any(|t| t.len() <= 3 && t.ends_with(':')))
        || (name == "dd" && args.iter().any(|a| a.starts_with("of=/dev/")));
    disk.then(|| "formatting or overwriting a disk".into())
}

fn writes_outside(name: &str, args: &[&String], all: &[String], allowed: &Allowed) -> Option<PathBuf> {
    let mut targets: Vec<&str> = Vec::new();
    // Redirects: `> file`, `>> file` (but not `>&2`).
    for (i, t) in all.iter().enumerate() {
        if (t == ">" || t == ">>") && let Some(target) = all.get(i + 1) {
            targets.push(target);
        }
    }
    let paths = || args.iter().map(|a| a.as_str()).filter(|a| !is_flag(a) && !is_slash_switch(a) && *a != ">" && *a != ">>" && *a != ">&");
    if PATH_ARGS.contains(&name) {
        targets.extend(paths());
    } else if DEST_LAST.contains(&name) {
        targets.extend(paths().last());
    } else if name == "sed" && args.iter().any(|a| a.starts_with("-i") || *a == "--in-place") {
        targets.extend(paths().skip(1));
    }
    for target in targets {
        if matches!(target, "/dev/null" | "/dev/stdout" | "/dev/stderr" | "nul" | "NUL" | "$null") || unknown(target) || !looks_like_path(target) {
            continue;
        }
        let path = expand_home(target);
        if !allowed.permits(&resolve(&path, allowed.cwd)) {
            return Some(path);
        }
    }
    None
}

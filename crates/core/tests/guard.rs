mod common;

use common::*;
use claudiu_core::GuardState;
use serde_json::json;

#[test]
fn a_write_is_allowed_inside_the_workspace_and_refused_outside_with_the_alternative() {
    let h = Harness::new(json!({}));
    let id = h.core.start_session(&h.project()).unwrap();

    let inside = pre(&h.core, &id, "Write", json!({"file_path": h.project().join("src").join("main.rs"), "content": "x"}));
    assert_eq!(decision(&inside), None, "work inside the project goes through untouched");

    let outside = pre(&h.core, &id, "Write", json!({"file_path": h.dir.path().join("elsewhere").join("x.txt"), "content": "x"}));
    assert_eq!(decision(&outside), Some("deny"));
    assert!(reason(&outside).contains("request_access"), "the refusal names the way forward: {}", reason(&outside));
    assert!(reason(&outside).contains("project"), "and the allowed place: {}", reason(&outside));
    assert!(!reason(&outside).contains('\n'), "one line");
}

#[test]
fn write_paths_are_judged_after_resolving_dots_caches_and_every_file_tool() {
    let h = Harness::new(json!({}));
    let id = h.core.start_session(&h.project()).unwrap();
    let denied = |tool: &str, key: &str, path: std::path::PathBuf| decision(&pre(&h.core, &id, tool, json!({ key: path }))) == Some("deny");

    assert!(denied("Write", "file_path", std::path::PathBuf::from("..").join("escape.txt")), "a relative path that climbs out");
    assert!(denied("Write", "file_path", h.project().join("a").join("..").join("..").join("escape.txt")), "dots inside an absolute path");
    assert!(denied("Edit", "file_path", h.dir.path().join("other.rs")), "Edit is a write");
    assert!(denied("MultiEdit", "file_path", h.dir.path().join("other.rs")), "so is MultiEdit");
    assert!(denied("NotebookEdit", "notebook_path", h.dir.path().join("n.ipynb")), "and NotebookEdit, which names its file differently");

    assert!(!denied("Write", "file_path", std::path::PathBuf::from("src").join("lib.rs")), "a relative path inside the project");
    assert!(!denied("Write", "file_path", h.dir.path().join("toolchain-cache").join("registry").join("x")), "the toolchain cache list");
    assert!(!denied("Write", "file_path", h.project().join("a").join("..").join("b.txt")), "dots that stay inside");
    if cfg!(windows) {
        let shouting = h.project().to_string_lossy().to_uppercase();
        assert!(!denied("Write", "file_path", std::path::PathBuf::from(shouting).join("b.txt")), "paths compare without case on Windows");
    }
}

fn bash(h: &Harness, id: &claudiu_core::SessionId, command: &str) -> serde_json::Value {
    pre(&h.core, id, "Bash", json!({ "command": command }))
}

fn forward(p: std::path::PathBuf) -> String {
    p.to_string_lossy().replace('\\', "/")
}

#[test]
fn ordinary_commands_are_not_refused() {
    let h = Harness::new(json!({}));
    let id = h.core.start_session(&h.project()).unwrap();
    let elsewhere = forward(h.dir.path().join("elsewhere").join("x.txt"));
    for command in [
        "cargo build --release",
        "echo hi > out.txt",
        "echo hi 2>&1 | tee build.log",
        "cat /etc/hosts",
        "ls ~",
        &format!("cat {elsewhere}"),
        "echo hi > /dev/null",
        "git push origin main",
        "git push --force-with-lease origin main",
        "rm -rf target ./build",
        "cp a.txt b.txt && mv b.txt c.txt",
        "npm install && npm test",
        "sed -i s/a/b/ src/main.rs",
        "FOO=1 cargo test",
        "echo \"a > b\"",
        "rm -rf $TARGET_DIR",
    ] {
        let reply = bash(&h, &id, command);
        assert_eq!(decision(&reply), None, "`{command}` should pass, got: {}", reason(&reply));
    }
}

#[test]
fn dangerous_commands_are_refused_in_one_line_with_the_alternative() {
    let h = Harness::new(json!({}));
    let id = h.core.start_session(&h.project()).unwrap();
    for command in [
        "rm -rf /",
        "rm -rf /*",
        "rm -rf ~",
        "rm -fr ~/",
        "rm -rf $HOME",
        "sudo rm -rf --no-preserve-root /",
        "cd build && rm -rf /",
        "Remove-Item -Recurse -Force C:\\",
        "rd /s /q C:\\",
        "git push --force origin main",
        "git push origin main -f",
        "git push origin +main",
        "git -C repo push --force",
        "mkfs.ext4 /dev/sda1",
        "format D:",
        "dd if=disk.img of=/dev/sda",
        "diskpart",
    ] {
        let reply = bash(&h, &id, command);
        assert_eq!(decision(&reply), Some("deny"), "`{command}` should be refused");
        let why = reason(&reply);
        assert!(why.starts_with("Refused:") && !why.contains('\n'), "one line: {why}");
        assert!(why.len() > 40, "it names an alternative: {why}");
    }
}

#[test]
fn shell_writes_outside_the_project_are_refused_on_a_best_effort_reading() {
    let h = Harness::new(json!({}));
    let id = h.core.start_session(&h.project()).unwrap();
    let elsewhere = forward(h.dir.path().join("elsewhere").join("x.txt"));
    for command in [
        format!("echo x > {elsewhere}"),
        format!("echo x >> {elsewhere}"),
        format!("echo x | tee {elsewhere}"),
        format!("cp a.txt {elsewhere}"),
        format!("mv a.txt {elsewhere}"),
        format!("rm {elsewhere}"),
        format!("mkdir {elsewhere}"),
        format!("touch {elsewhere}"),
        format!("sed -i s/a/b/ {elsewhere}"),
        "touch ../x".to_string(),
        "rm -rf ..".to_string(),
        "echo x > ~/notes.txt".to_string(),
        format!("Set-Content -Path {elsewhere} -Value hi"),
    ] {
        let reply = bash(&h, &id, &command);
        assert_eq!(decision(&reply), Some("deny"), "`{command}` writes outside the project");
        assert!(reason(&reply).contains("request_access"), "{}", reason(&reply));
    }
}

#[test]
fn hooks_are_installed_per_process_with_the_guards_failing_closed() {
    let h = Harness::new(json!({}));
    let id = h.core.start_session(&h.project()).unwrap();

    let new = &h.adapter_calls("session/new")[0];
    let options = &new["_meta"]["claudeCode"]["options"];
    assert_eq!(options["settings"]["disableAllHooks"], false, "a project that disables hooks must not switch ours off");
    assert_eq!(options["enableFileCheckpointing"], true);
    let guard = &options["settings"]["hooks"]["PreToolUse"][0]["hooks"][0];
    assert_eq!(guard["type"], "http");
    assert_eq!(guard["url"], h.core.hook_url(&id, "pre"));
    assert_eq!(guard["onFailure"], "block", "a dead policy process must stop the call, not wave it through");
    assert_eq!(h.adapter_calls("session/set_mode")[0]["modeId"], "auto", "tasks run in Claude Code's auto mode");
}

#[test]
fn guards_that_never_fire_drop_the_task_to_ask_mode_with_a_badge() {
    let h = Harness::new(json!({}));
    let rx = h.core.subscribe();
    let id = h.core.start_session(&h.project()).unwrap();
    assert_eq!(h.core.guard_state(&id), GuardState::Pending);

    h.core.send_prompt(&id, "go").unwrap();
    until(&rx, "guards_off");

    assert_eq!(h.core.guard_state(&id), GuardState::Off);
    let modes: Vec<_> = h.adapter_calls("session/set_mode").iter().map(|p| p["modeId"].clone()).collect();
    assert_eq!(modes, ["auto", "default"], "auto at the start, ask mode once the canary fails");
}

#[test]
fn a_hook_call_after_the_first_prompt_proves_the_guards_are_live() {
    let h = Harness::new(json!({}));
    let id = h.core.start_session(&h.project()).unwrap();
    h.core.send_prompt(&id, "go").unwrap();

    let (status, _) = post(&h.core.hook_url(&id, "prompt"), &json!({"hook_event_name": "UserPromptSubmit", "prompt": "go"}));
    assert_eq!(status, 200);
    std::thread::sleep(std::time::Duration::from_millis(900));

    assert_eq!(h.core.guard_state(&id), GuardState::Active);
    let modes: Vec<_> = h.adapter_calls("session/set_mode").iter().map(|p| p["modeId"].clone()).collect();
    assert_eq!(modes, ["auto"], "no fallback to ask mode");
}

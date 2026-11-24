#[test]
fn the_worktree_toggle_saved_by_an_earlier_version_is_ignored() {
    let settings: Settings = serde_json::from_str(r#"{"worktree_sessions":true}"#).unwrap();
    assert_eq!(settings, Settings::default());
}

use super::*;

#[test]
fn the_picker_lists_only_the_ticked_models_once_any_are_ticked() {
    use crate::harness::subagents::{Agent, AgentModel};
    use crate::harness::Harness;
    let model = |id: &str| AgentModel { id: id.into(), name: id.to_uppercase() };
    let mut settings = Settings { enabled_harnesses: Some(vec![Harness::Claude, Harness::Codex]), ..Default::default() };
    settings.claude_models = vec![model("opus"), model("sonnet")];
    settings.custom_subagents.get_mut(Agent::Codex).catalog = vec![model("gpt-a"), model("gpt-b"), model("gpt-c")];
    let ids = |s: &Settings| s.aggregated_models().into_iter().map(|m| m.id).collect::<Vec<_>>();
    assert_eq!(ids(&settings), ["opus", "sonnet", "gpt-a", "gpt-b", "gpt-c"], "nothing ticked: every model shows");

    settings.claude_picked = vec!["sonnet".into()];
    settings.custom_subagents.get_mut(Agent::Codex).models = vec!["gpt-c".into(), "gpt-a".into()];
    assert_eq!(ids(&settings), ["sonnet", "gpt-a", "gpt-c"], "the catalog's order stays");

    settings.claude_picked = vec!["retired".into()];
    assert_eq!(ids(&settings)[..2], ["opus", "sonnet"], "ticks the harness no longer offers don't empty it");
}

#[test]
fn default_model_uses_an_enabled_harness_when_claude_is_disabled() {
    use crate::harness::Harness;
    let settings = Settings {
        harness: Harness::Claude,
        enabled_harnesses: Some(vec![Harness::Codex, Harness::OpenCode]),
        ..Default::default()
    };
    let chosen = settings.default_model();
    assert_eq!(chosen.harness, Harness::Codex);
}

#[test]
fn the_recent_list_is_closed_after_a_restart() {
    let layout = Layout { recent_open: true, ..Default::default() };
    let saved = serde_json::to_string(&layout).unwrap();
    assert!(!serde_json::from_str::<Layout>(&saved).unwrap().recent_open);
}

fn project(id: &str, path: &str) -> Project {
    Project { id: id.into(), name: id.into(), path: PathBuf::from(path), git: None, collapsed: false, added_at: 0 }
}

#[test]
fn the_last_antigravity_reading_is_there_after_a_restart() {
    use crate::harness::limits::Pool;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let mut s = Store::load(path.clone());
    let limit = RateLimit { utilization: Some(0.5), resets_at: Some(200), seen_at: 100 };
    s.data.subagent_limits.insert("antigravity".into(), vec![Pool { name: "Gemini Models".into(), usage: UsageLimits { five_hour: None, seven_day: Some(limit) } }]);
    s.save();
    assert_eq!(Store::load(path).data.subagent_limits, s.data.subagent_limits);
    // A state.json from before the field loads with no reading.
    let old = dir.path().join("old.json");
    fs::write(&old, r#"{"version":1,"projects":[],"sessions":[]}"#).unwrap();
    assert!(Store::load(old).data.subagent_limits.is_empty());
}

#[test]
fn saves_land_in_the_order_made_and_a_failed_one_is_tried_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let mut s = Store::load(path.clone());
    for name in ["first", "second", "third"] {
        s.add_project(&dir.path().join(name));
        s.save_if_dirty();
    }
    s.save();
    let names = |p: &Path| Store::load(p.to_path_buf()).data.projects.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&path), ["first", "second", "third"]);

    // A folder where the file should be: every write fails, and the store keeps its changes to write again.
    let blocked = dir.path().join("blocked");
    fs::create_dir_all(blocked.join("state.json")).unwrap();
    let mut b = Store::load(blocked.join("state.json"));
    b.mark_dirty();
    b.save();
    assert!(b.dirty);
}

fn store_with(projects: Vec<Project>, sessions: Vec<(&str, &str, Option<&str>)>) -> (Store, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::load(dir.path().join("state.json"));
    s.data.projects = projects;
    s.data.sessions = sessions
        .into_iter()
        .map(|(id, cwd, project)| SessionRecord {
            id: id.into(),
            kind: SessionKind::Agent,
            title: id.into(),
            custom_title: None,
            auto_title: None,
            cwd: PathBuf::from(cwd),
            project_id: project.map(Into::into),
            created_at: 1,
            last_active: 1,
            exit_code: None,
            hidden: false,
            agent: None,
        })
        .collect();
    (s, dir)
}

fn order(s: &Store) -> Vec<&str> {
    s.data.projects.iter().map(|p| p.id.as_str()).collect()
}

#[test]
fn a_removed_project_comes_back_in_its_place_with_its_sessions() {
    let (mut s, _dir) = store_with(vec![project("a", "/a"), project("b", "/b"), project("c", "/c")], vec![("s1", "/b/x", Some("b")), ("s2", "/a", Some("a"))]);
    let (at, removed) = s.remove_project("b").expect("b is there");
    assert_eq!(order(&s), ["a", "c"]);
    assert_eq!(s.session("s1").unwrap().project_id, None, "its session stays, without a project");
    s.restore_project(at, removed);
    assert_eq!(order(&s), ["a", "b", "c"]);
    assert_eq!(s.session("s1").unwrap().project_id.as_deref(), Some("b"));
    assert_eq!(s.session("s2").unwrap().project_id.as_deref(), Some("a"));
    assert!(s.remove_project("gone").is_none());
}

#[test]
fn undoing_a_removal_after_the_folder_was_added_again_adds_nothing() {
    let (mut s, _dir) = store_with(vec![project("a", "/a"), project("b", "/b")], vec![]);
    let (at, removed) = s.remove_project("b").unwrap();
    s.data.projects.push(project("b2", "/b"));
    s.restore_project(at, removed);
    assert_eq!(order(&s), ["a", "b2"]);
}

#[test]
fn a_project_is_renamed_and_an_empty_name_goes_back_to_its_folders() {
    let (mut s, _dir) = store_with(vec![project("a", "/w/app")], vec![]);
    s.rename_project("a", "  Main app ");
    assert_eq!(s.project("a").unwrap().name, "Main app");
    s.rename_project("a", "   ");
    assert_eq!(s.project("a").unwrap().name, "app");
}

#[test]
fn projects_move_one_place_and_stop_at_the_ends() {
    let (mut s, _dir) = store_with(vec![project("a", "/a"), project("b", "/b"), project("c", "/c")], vec![]);
    s.move_project("c", true);
    assert_eq!(order(&s), ["a", "c", "b"]);
    s.move_project("a", true);
    assert_eq!(order(&s), ["a", "c", "b"], "the first stays first");
    s.move_project("a", false);
    assert_eq!(order(&s), ["c", "a", "b"]);
    s.move_project("b", false);
    assert_eq!(order(&s), ["c", "a", "b"], "the last stays last");
}

#[test]
fn adding_a_project_adopts_existing_unmapped_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = Store::load(tmp.path().join("state.json"));
    let proj_dir = tmp.path().join("proj");
    fs::create_dir_all(&proj_dir).unwrap();
    store.add_session(SessionRecord {
        id: "s1".into(),
        kind: SessionKind::Agent,
        cwd: proj_dir.join("sub"),
        project_id: None,
        title: "T".into(),
        custom_title: None,
        auto_title: None,
        created_at: 1,
        last_active: 1,
        hidden: false,
        exit_code: None,
        agent: None,
    });
    assert!(store.data.sessions[0].project_id.is_none());
    let id = store.add_project(&proj_dir);
    assert_eq!(store.data.sessions[0].project_id.as_deref(), Some(id.as_str()));
    assert_eq!(store.add_project(&proj_dir), id, "idempotent");
}

#[test]
fn shell_sessions_are_not_restored_from_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("s.json");
    let mut a = Store::load(path.clone());
    for (id, kind) in [("sh", SessionKind::Shell), ("cl", SessionKind::Agent)] {
        a.data.sessions.push(SessionRecord {
            id: id.into(),
            kind,
            title: id.into(),
            custom_title: None,
            auto_title: None,
            cwd: PathBuf::from("/w"),
            project_id: None,
            created_at: 1,
            last_active: 1,
            exit_code: None,
            hidden: false,
            agent: None,
        });
    }
    a.save();
    let b = Store::load(path);
    assert_eq!(b.data.sessions.len(), 1);
    assert_eq!(b.data.sessions[0].id, "cl");
}

#[test]
fn legacy_account_usage_loads_into_claude_usage() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("s.json");
    let old = r#"{"version":1,"projects":[],"sessions":[],"account":{"five_hour":{"utilization":0.5,"resets_at":null,"seen_at":3}},
        "layout":{"selected_session":null,"sidebar_visible":true,"other_collapsed":false},"settings":{"claude_path":null,"update_url":null}}"#;
    fs::write(&path, old).unwrap();
    let mut s = Store::load(path.clone());
    assert_eq!(s.data.claude_usage.five_hour.as_ref().unwrap().utilization, Some(0.5));
    s.save();
    assert!(fs::read_to_string(&path).unwrap().contains("\"account\""), "older versions require the field");
    assert_eq!(Store::load(path).data.claude_usage.five_hour.as_ref().unwrap().utilization, Some(0.5));
}

#[test]
fn legacy_accounts_file_usage_is_imported_when_state_usage_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.json");
    let accounts = r#"{"active":"alt","accounts":[{"id":"alt","usage":{"five_hour":{"utilization":1.0,"resets_at":null,"seen_at":10}}},{"id":"main","usage":{"five_hour":{"utilization":0.42,"resets_at":null,"seen_at":9}}}]}"#;
    fs::write(tmp.path().join("accounts.json"), accounts).unwrap();
    let s = Store::load(path);
    assert_eq!(s.data.claude_usage.five_hour.as_ref().unwrap().utilization, Some(0.42));
}

#[test]
fn legacy_default_compaction_inherits_and_explicit_off_survives() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.json");
    let mut data = Store::load(path.clone()).data;
    data.version = 1;
    for (id, auto_compact) in [("enabled", true), ("disabled", false)] {
        data.sessions.push(SessionRecord {
            id: id.into(),
            kind: SessionKind::Agent,
            title: id.into(),
            custom_title: None,
            auto_title: None,
            cwd: tmp.path().into(),
            project_id: None,
            created_at: 1,
            last_active: 1,
            exit_code: None,
            hidden: false,
            agent: Some(AgentMeta { managed_compaction: true, auto_compact: Some(auto_compact), ..Default::default() }),
        });
    }
    fs::write(&path, serde_json::to_vec(&data).unwrap()).unwrap();
    let mut store = Store::load(path.clone());
    assert_eq!(store.session("enabled").unwrap().agent.as_ref().unwrap().auto_compact_override, None);
    assert_eq!(store.session("disabled").unwrap().agent.as_ref().unwrap().auto_compact_override, Some(false));
    store.session_mut("disabled").unwrap().agent.as_mut().unwrap().auto_compact_override = None;
    store.save();
    assert_eq!(Store::load(path).session("disabled").unwrap().agent.as_ref().unwrap().auto_compact_override, None);
}

#[test]
fn save_load_roundtrip_and_corrupt_file_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("s.json");
    let mut a = Store::load(path.clone());
    a.data.projects.push(project("p", "/x"));
    a.save();
    let b = Store::load(path.clone());
    assert_eq!(b.data.projects.len(), 1);

    fs::write(&path, "{ not json").unwrap();
    let c = Store::load(path.clone());
    assert!(c.data.projects.is_empty());
    assert!(path.with_extension("json.corrupt").exists(), "corrupt state is preserved, not destroyed");
}

#[test]
fn a_launch_is_the_first_only_while_there_is_no_state_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.json");
    assert!(Store::is_first_launch(&path), "nothing saved yet");
    Store::load(path.clone()).save();
    assert!(!Store::is_first_launch(&path), "a saved state file ends the first launch");

    fs::write(&path, "{ not json").unwrap();
    assert!(!Store::is_first_launch(&path), "a corrupt state file still means m4code ran here");
}

#[test]
fn window_geometry_survives_a_save_and_old_state_files_load_without_it() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("s.json");
    let mut a = Store::load(path.clone());
    assert_eq!(a.data.layout.window, None, "a first run has nothing saved");
    let geom = WindowGeom { x: 40.0, y: 30.0, w: 900.0, h: 600.0, maximized: true };
    a.data.layout.window = Some(geom);
    a.save();
    assert_eq!(Store::load(path.clone()).data.layout.window, Some(geom));

    let mut value: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    value["layout"].as_object_mut().unwrap().remove("window");
    fs::write(&path, value.to_string()).unwrap();
    assert_eq!(Store::load(path).data.layout.window, None, "state from before this setting loads");
}

#[test]
fn a_window_is_restored_only_where_a_display_still_shows_it() {
    let geom = |x| WindowGeom { x, y: 100.0, w: 1000.0, h: 700.0, maximized: false };
    let primary = (0.0, 0.0, 1920.0, 1080.0);
    let right = (1920.0, 0.0, 1920.0, 1080.0);
    assert_eq!(geom(100.0).display_of(&[primary]), Some(0));
    assert_eq!(geom(2000.0).display_of(&[primary, right]), Some(1));
    assert_eq!(geom(2000.0).display_of(&[primary]), None, "the second monitor was unplugged");
    assert_eq!(geom(100.0).display_of(&[]), None, "no display reported");
}

#[test]
fn default_model_resolves_across_enabled_harnesses_and_falls_back_when_disabled() {
    use crate::harness::subagents::{Agent, AgentModel};
    use crate::harness::{CatalogModel, Harness};

    let mut settings = Settings {
        enabled_harnesses: Some(vec![Harness::Claude, Harness::Codex]),
        harness: Harness::Codex,
        ..Default::default()
    };
    settings.custom_subagents.get_mut(Agent::Codex).catalog = vec![
        AgentModel { id: "gpt-5.3-codex".into(), name: "GPT-5.3 Codex".into() },
    ];
    settings.new_session.model = Some("gpt-5.3-codex".into());

    let all = settings.aggregated_models();
    assert!(all.iter().any(|m| m.harness == Harness::Claude && m.id == "opus"));
    assert!(all.iter().any(|m| m.harness == Harness::Codex && m.id == "gpt-5.3-codex"));
    assert_eq!(
        settings.default_model(),
        CatalogModel { harness: Harness::Codex, id: "gpt-5.3-codex".into(), name: "GPT-5.3 Codex".into() }
    );

    // Disabling Codex falls back to an enabled harness's model.
    settings.enabled_harnesses = Some(vec![Harness::Claude]);
    assert_eq!(settings.default_model().harness, Harness::Claude);
    assert_eq!(settings.default_model().id, "opus");
}

#[test]
fn migrate_data_dir_moves_claudiu_folder_when_m4code_is_empty() {
    let root = tempfile::tempdir().unwrap();
    let old = root.path().join("Claudiu");
    let new = root.path().join("m4code");
    fs::create_dir_all(&old).unwrap();
    fs::write(old.join("state.json"), r#"{"version":1}"#).unwrap();
    fs::write(old.join("accounts.json"), r#"{"accounts":[]}"#).unwrap();

    migrate_data_dir(&new, std::slice::from_ref(&old));

    assert!(new.join("state.json").exists());
    assert!(new.join("accounts.json").exists());
    assert!(!old.exists());
}

#[test]
fn migrate_data_dir_keeps_existing_m4code_state() {
    let root = tempfile::tempdir().unwrap();
    let old = root.path().join("Claudiu");
    let new = root.path().join("m4code");
    fs::create_dir_all(&old).unwrap();
    fs::create_dir_all(&new).unwrap();
    fs::write(old.join("state.json"), r#"{"version":1,"old":true}"#).unwrap();
    fs::write(new.join("state.json"), r#"{"version":1,"new":true}"#).unwrap();

    migrate_data_dir(&new, std::slice::from_ref(&old));

    assert!(fs::read_to_string(new.join("state.json")).unwrap().contains("\"new\":true"));
    assert!(old.join("state.json").exists());
}

#[test]
fn settings_deserializes_legacy_claudiu_tools_off() {
    let s: Settings = serde_json::from_str(r#"{"claudiu_tools_off":["visual"]}"#).unwrap();
    assert_eq!(s.m4code_tools_off, vec![crate::mcp::Feature::Visual]);
}


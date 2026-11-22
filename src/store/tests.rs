#[test]
fn the_worktree_toggle_saved_by_an_earlier_version_is_ignored() {
    let settings: Settings = serde_json::from_str(r#"{"worktree_sessions":true}"#).unwrap();
    assert_eq!(settings, Settings::default());
}

use super::*;

#[test]
fn the_picker_lists_only_the_ticked_models_once_any_are_ticked() {
    use crate::claude::subagents::{Agent, AgentModel};
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
fn the_recent_list_is_closed_after_a_restart() {
    let layout = Layout { recent_open: true, ..Default::default() };
    let saved = serde_json::to_string(&layout).unwrap();
    assert!(!serde_json::from_str::<Layout>(&saved).unwrap().recent_open);
}

fn ext(id: &str, cwd: &str, title: &str, last: i64) -> ExternalSession {
    ExternalSession {
        session_id: id.into(),
        cwd: Some(PathBuf::from(cwd)),
        title: Some(title.into()),
        custom_title: None,
        created_at: 1,
        last_active: last,
        model: Some("claude-sonnet-5-5".into()),
        context_tokens: Some(1000),
        cache_ttl: None,
        cache_at: None,
        transcript: PathBuf::from(format!("/t/{id}.jsonl")),
    }
}

fn project(id: &str, path: &str) -> Project {
    Project { id: id.into(), name: id.into(), path: PathBuf::from(path), git: None, collapsed: false, added_at: 0 }
}

#[test]
fn the_last_antigravity_reading_is_there_after_a_restart() {
    use crate::claude::limits::Pool;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let mut s = Store::load(path.clone());
    let limit = RateLimit { utilization: Some(0.5), resets_at: Some(200), seen_at: 100 };
    s.data.subagent_limits.insert("antigravity".into(), vec![Pool { name: "Gemini 4 Argon".into(), usage: AccountUsage { five_hour: None, seven_day: Some(limit) } }]);
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
            kind: SessionKind::Claude,
            title: id.into(),
            custom_title: None,
            auto_title: None,
            cwd: PathBuf::from(cwd),
            project_id: project.map(Into::into),
            created_at: 1,
            last_active: 1,
            exit_code: None,
            hidden: false,
            claude: None,
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
fn imports_new_sessions_and_maps_to_deepest_project() {
    let mut d = Data { projects: vec![project("outer", "/w"), project("inner", "/w/app")], ..Default::default() };
    let stats = reconcile(&mut d, &[ext("a", "/w/app/src", "T", 10), ext("b", "/other", "U", 5)], true);
    assert_eq!(stats.added, 2);
    assert_eq!(d.sessions[0].project_id.as_deref(), Some("inner"));
    assert_eq!(d.sessions[1].project_id, None);
}

#[test]
fn a_rename_inside_claude_code_renames_the_session_in_claudiu() {
    let mut d = Data::default();
    let named = |name: Option<&str>| ExternalSession { custom_title: name.map(Into::into), ..ext("a", "/w", "Generated", 10) };
    reconcile(&mut d, &[named(None)], true);
    d.sessions[0].auto_title = Some("Old model title".into());
    reconcile(&mut d, &[named(Some("First /rename"))], true);
    assert_eq!(d.sessions[0].display_title(), "First /rename", "the first name of a session Claudiu never named");

    // Named in Claudiu before Claudiu saw a name in Claude Code: the one Claude Code holds may be older.
    d.sessions[0].custom_title = Some("Named in Claudiu".into());
    d.sessions[0].claude.as_mut().unwrap().claude_title = None;
    reconcile(&mut d, &[named(Some("Old name"))], true);
    assert_eq!(d.sessions[0].display_title(), "Named in Claudiu", "the first name seen is only recorded");
    reconcile(&mut d, &[named(Some("Named in Claudiu"))], true);

    reconcile(&mut d, &[named(Some("From /rename"))], true);
    assert_eq!(d.sessions[0].display_title(), "From /rename");

    // Renamed in Claudiu: Claude Code still holds the old name until the mod's /rename reaches it.
    d.sessions[0].custom_title = Some("From Claudiu".into());
    reconcile(&mut d, &[named(Some("From /rename"))], true);
    assert_eq!(d.sessions[0].display_title(), "From Claudiu", "a rename made in Claudiu is not undone");
    reconcile(&mut d, &[named(None)], true);
    assert_eq!(d.sessions[0].display_title(), "From Claudiu", "a generated title never renames it");
    reconcile(&mut d, &[named(Some("From Claudiu"))], true);
    reconcile(&mut d, &[named(Some("Again"))], true);
    assert_eq!(d.sessions[0].display_title(), "Again");

    // Renamed twice in Claudiu, quickly: Claude Code reaches the first name while the second is on its way.
    d.sessions[0].custom_title = Some("Second".into());
    reconcile(&mut d, &[named(Some("First"))], true);
    assert_eq!(d.sessions[0].display_title(), "Second", "Claude Code catching up is not a rename");

    // Claudiu's name with spaces around it, which Claude Code holds trimmed: later renames still count.
    d.sessions[0].custom_title = Some(" Spaced ".into());
    reconcile(&mut d, &[named(Some("Spaced"))], true);
    reconcile(&mut d, &[named(Some("Next"))], true);
    assert_eq!(d.sessions[0].display_title(), "Next");
}

#[test]
fn rerunning_is_idempotent_and_never_duplicates() {
    let mut d = Data::default();
    let e = [ext("a", "/w", "T", 10)];
    reconcile(&mut d, &e, true);
    let stats = reconcile(&mut d, &e, true);
    assert_eq!(d.sessions.len(), 1);
    assert_eq!(stats, ReconcileStats::default());
}

#[test]
fn session_launched_by_claudiu_is_matched_not_duplicated() {
    let mut d = Data::default();
    d.sessions.push(SessionRecord {
        id: "local".into(),
        kind: SessionKind::Claude,
        title: "Claude".into(),
        custom_title: Some("My name".into()),
        auto_title: None,
        cwd: PathBuf::from("/w"),
        project_id: None,
        created_at: 1,
        last_active: 2,
        exit_code: Some(0),
        hidden: true,
        claude: Some(ClaudeMeta { session_id: "a".into(), ..Default::default() }),
    });
    reconcile(&mut d, &[ext("a", "/w", "Ext title", 50)], true);
    assert_eq!(d.sessions.len(), 1);
    let s = &d.sessions[0];
    assert_eq!(s.id, "local");
    assert_eq!(s.title, "Ext title");
    assert_eq!(s.display_title(), "My name", "Claudiu-only label survives");
    assert!(s.hidden, "hidden flag survives");
    assert_eq!(s.last_active, 50);
    assert_eq!(s.claude.as_ref().unwrap().external, ExternalState::Present);
}

#[test]
fn missing_external_is_flagged_not_deleted_and_only_on_good_scan() {
    let mut d = Data::default();
    reconcile(&mut d, &[ext("a", "/w", "T", 1)], true);

    let stats = reconcile(&mut d, &[], false);
    assert_eq!(stats.marked_missing, 0, "unreadable scan must not change state");
    assert_eq!(d.sessions[0].claude.as_ref().unwrap().external, ExternalState::Present);

    let stats = reconcile(&mut d, &[], true);
    assert_eq!(stats.marked_missing, 1);
    assert_eq!(d.sessions.len(), 1, "history is never deleted");
    assert_eq!(d.sessions[0].claude.as_ref().unwrap().external, ExternalState::Missing);

    // It comes back if the file reappears.
    reconcile(&mut d, &[ext("a", "/w", "T", 1)], true);
    assert_eq!(d.sessions[0].claude.as_ref().unwrap().external, ExternalState::Present);
}

#[test]
fn shell_sessions_are_untouched() {
    let mut d = Data::default();
    d.sessions.push(SessionRecord {
        id: "sh".into(),
        kind: SessionKind::Shell,
        title: "PowerShell".into(),
        custom_title: None,
        auto_title: None,
        cwd: PathBuf::from("/w"),
        project_id: None,
        created_at: 1,
        last_active: 1,
        exit_code: None,
        hidden: false,
        claude: None,
    });
    let stats = reconcile(&mut d, &[], true);
    assert_eq!(stats, ReconcileStats::default());
    assert_eq!(d.sessions.len(), 1);
}

#[test]
fn adding_a_project_adopts_existing_unmapped_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = Store::load(tmp.path().join("state.json"));
    let proj_dir = tmp.path().join("proj");
    fs::create_dir_all(&proj_dir).unwrap();
    let e = ext("a", &proj_dir.join("sub").to_string_lossy(), "T", 1);
    store.reconcile(&[e], true);
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
    for (id, kind) in [("sh", SessionKind::Shell), ("cl", SessionKind::Claude)] {
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
            claude: None,
        });
    }
    a.save();
    let b = Store::load(path);
    assert_eq!(b.data.sessions.len(), 1);
    assert_eq!(b.data.sessions[0].id, "cl");
}

#[test]
fn usage_from_before_accounts_moves_into_the_main_account() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("s.json");
    let old = r#"{"version":1,"projects":[],"sessions":[],"account":{"five_hour":{"utilization":0.5,"resets_at":null,"seen_at":3}},
        "layout":{"selected_session":null,"sidebar_visible":true,"other_collapsed":false},"settings":{"claude_path":null,"update_url":null}}"#;
    fs::write(&path, old).unwrap();
    let mut s = Store::load(path.clone());
    assert_eq!(s.active_account().unwrap().usage.five_hour.as_ref().unwrap().utilization, Some(0.5));
    s.save();
    assert!(fs::read_to_string(&path).unwrap().contains("\"account\""), "older versions require the field");
    assert_eq!(Store::load(path).data.accounts.len(), 1, "migrates once");
}

#[test]
fn a_session_starts_under_its_own_account_while_that_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Store::load(tmp.path().join("s.json"));
    s.data.accounts.push(Account { id: "alt".into(), ..Default::default() });
    s.data.active_account = Some("alt".into());
    let session = |id: &str, account: Option<&str>| SessionRecord {
        id: id.into(),
        kind: SessionKind::Claude,
        title: "Claude".into(),
        custom_title: None,
        auto_title: None,
        cwd: PathBuf::from("/w"),
        project_id: None,
        created_at: 1,
        last_active: 1,
        exit_code: None,
        hidden: false,
        claude: Some(ClaudeMeta { session_id: id.into(), account: account.map(Into::into), ..Default::default() }),
    };
    s.add_session(session("main-owned", Some(MAIN_ACCOUNT)));
    s.add_session(session("never-started", None));
    s.add_session(session("removed-owner", Some("gone")));
    let account = |s: &Store, id: &str| s.session_account(id).map(|a| a.id.clone());
    assert_eq!(account(&s, "main-owned").as_deref(), Some(MAIN_ACCOUNT), "not the active alt");
    assert_eq!(account(&s, "never-started").as_deref(), Some("alt"));
    assert_eq!(account(&s, "removed-owner").as_deref(), Some("alt"));
    assert_eq!(account(&s, "not-yet-stored").as_deref(), Some("alt"), "a new session");
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
    assert_eq!(c.active_account().map(|a| a.id.as_str()), Some(MAIN_ACCOUNT), "main account always exists");
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
    assert!(!Store::is_first_launch(&path), "a corrupt state file still means Claudiu ran here");
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
    use crate::claude::subagents::{Agent, AgentModel};
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


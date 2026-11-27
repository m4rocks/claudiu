use super::*;

type TextGenerator = Box<dyn Fn(&str, &str) -> Result<String, String> + Send>;

const TITLE_PROMPT: &str = "Write a short title for the session described by this message. Reply with a single title of at most 60 characters. Use no quotes or preamble.";

impl Workspace {
    /// The model that writes a summary of a session on `harness` (running `model`): the one picked in Settings, else the
    /// session's own, when its adapter answers m4code's prompts. `None` means a local summary.
    pub(super) fn summary_model(&self, harness: Harness, model: Option<String>) -> Option<crate::store::TextGenerationModel> {
        let chosen = self.store.data.settings.text_generation_model.clone()
            .unwrap_or(crate::store::TextGenerationModel { harness, id: model.unwrap_or_default() });
        chosen.harness.adapter().capabilities.text_generation.allowed().then_some(chosen)
    }

    pub(super) fn text_generator(&self, cx: &App) -> Result<Option<TextGenerator>, String> {
        let settings = &self.store.data.settings;
        let Some(model) = settings.text_generation_model.clone() else { return Ok(None) };
        if let Some(refusal) = model.harness.adapter().capabilities.text_generation.refusal() {
            return Err(format!("{refusal} Pick another text generation model in Settings → General."));
        }
        if !settings.harness_enabled(model.harness) {
            return Err(format!("{} is disabled. Pick a text generation model in Settings → General.", model.harness.name()));
        }
        if model.id.is_empty() || model.id == "default" {
            return Err("Pick a specific text generation model in Settings → General.".into());
        }
        let env = self.harness_model_env(model.harness)?;
        let executor = cx.background_executor().clone();
        Ok(Some(Box::new(move |system, prompt| {
            let timeout = executor.timer(Duration::from_secs(90));
            futures::executor::block_on(harness::text_generation::complete(&model, env.clone(), system, prompt, timeout))
                .map_err(|why| format!("{} could not generate text: {why}", model.harness.name()))
        })))
    }

    pub(super) fn auto_title(&mut self, id: &str, raw: &str, cx: &mut Context<Self>) {
        let Some(record) = self.store.session(id) else { return };
        if record.title != "New session" || record.chosen_title().is_some() || record.is_subagent() {
            return;
        }
        let fallback = clean_title(raw);
        if fallback.is_empty() { return; }
        // Persist a useful title immediately, and reserve this session so later prompts cannot
        // start duplicate requests. Generation failures leave this local summary in place.
        self.store.session_mut(id).unwrap().auto_title = Some(fallback.clone());
        self.store.mark_dirty();
        cx.notify();
        match self.text_generator(cx) {
            Ok(Some(generate)) => {
                let id = id.to_string();
                let prompt: String = raw.chars().take(6000).collect();
                self.in_background(
                    move || crate::git::commit::generate(generate.as_ref(), TITLE_PROMPT, &prompt),
                    move |ws, result, cx| {
                        if let Ok(title) = result {
                            let title = clean_title(&title);
                            if let Some(record) = ws.store.session_mut(&id)
                                && can_replace_auto_title(record, &fallback)
                                && !title.is_empty()
                            {
                                record.auto_title = Some(title);
                                ws.store.mark_dirty();
                                cx.notify();
                            }
                        }
                    },
                    cx,
                );
            }
            Ok(None) => {}
            Err(why) => self.toast(why, cx),
        }
    }

    pub(super) fn regen_title(&mut self, id: &str, cx: &mut Context<Self>) {
        let first_prompt = self
            .live
            .iter()
            .find(|l| l.id == id)
            .and_then(|l| l.view.thread())
            .and_then(|t| t.read(cx).thread.sent_texts().into_iter().next());
        let Some(raw) = first_prompt.filter(|text| !text.trim().is_empty()) else {
            return self.toast("Nothing to title yet.", cx);
        };

        match self.text_generator(cx) {
            Ok(Some(generate)) => {
                let id = id.to_string();
                let previous = self.store.session(&id).and_then(|record| record.custom_title.clone());
                self.in_background(
                    move || {
                        let prompt: String = raw.chars().take(6000).collect();
                        crate::git::commit::generate(generate.as_ref(), TITLE_PROMPT, &prompt)
                    },
                    move |ws, result, cx| match result {
                        Ok(title) => {
                            if ws.store.session(&id).is_some_and(|record| record.custom_title == previous) {
                                ws.apply_generated_title(&id, &title, cx);
                            }
                        }
                        Err(why) => ws.toast(why, cx),
                    },
                    cx,
                );
            }
            Ok(None) => self.apply_generated_title(id, &raw, cx),
            Err(why) => self.toast(why, cx),
        }
    }

    fn apply_generated_title(&mut self, id: &str, raw: &str, cx: &mut Context<Self>) {
        let title = clean_title(raw);
        if title.is_empty() {
            return self.toast("Nothing to title yet.", cx);
        }
        if let Some(rec) = self.store.session_mut(id) {
            rec.custom_title = Some(title.clone());
            self.toast(format!("Renamed to “{title}”"), cx);
        }
    }
}

fn can_replace_auto_title(record: &SessionRecord, fallback: &str) -> bool {
    record.custom_title.is_none()
        && record.title == "New session"
        && record.auto_title.as_deref() == Some(fallback)
}

fn clean_title(raw: &str) -> String {
    raw.lines()
        .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('>'))
        .or_else(|| raw.lines().find(|l| !l.trim().is_empty()))
        .unwrap_or_default()
        .trim()
        .trim_matches(['"', '\'', '`', '.'])
        .chars()
        .take(60)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> SessionRecord {
        SessionRecord {
            id: "session".into(), kind: SessionKind::Agent, title: "New session".into(),
            custom_title: None, auto_title: Some("Fix login".into()), cwd: PathBuf::new(),
            project_id: None, created_at: 0, last_active: 0, exit_code: None, hidden: false,
            agent: None,
        }
    }

    #[test]
    #[ignore = "drives the workspace"]
    fn new_sessions_receive_a_title_once_for_every_harness() {
        let _data = crate::datadir::own_folder();
        let mut cx = gpui::TestAppContext::single();
        cx.update(gpui_kit::init);
        let (_, ws) = cx.update(|app| gpui_kit::open_window(Default::default(), app, |window, app| {
            app.new(|cx| Workspace::new(window, vec![], cx))
        })).unwrap();
        ws.update(&mut cx, |ws, cx| {
            ws.store.data.settings.text_generation_model = None;
            for harness in [Harness::Claude, Harness::Codex, Harness::OpenCode, Harness::Antigravity] {
                let mut rec = record();
                rec.id = harness.name().into();
                rec.auto_title = None;
                rec.agent = Some(AgentMeta { harness, ..Default::default() });
                let id = rec.id.clone();
                ws.store.data.sessions.push(rec);
                ws.auto_title(&id, "Fix login", cx);
                assert_eq!(ws.store.session(&id).unwrap().display_title(), "Fix login");
                assert_eq!(ws.store.session(&id).unwrap().custom_title, None);
                ws.auto_title(&id, "Another request", cx);
                assert_eq!(ws.store.session(&id).unwrap().display_title(), "Fix login");
            }
        });
    }

    #[test]
    fn late_generation_preserves_manual_native_and_replacement_titles() {
        let mut record = record();
        assert!(can_replace_auto_title(&record, "Fix login"));
        record.custom_title = Some("My name".into());
        assert!(!can_replace_auto_title(&record, "Fix login"));
        record.custom_title = None;
        record.title = "Native title".into();
        assert!(!can_replace_auto_title(&record, "Fix login"));
        record.title = "New session".into();
        record.auto_title = Some("Replacement".into());
        assert!(!can_replace_auto_title(&record, "Fix login"));
    }

    #[test]
    fn local_titles_skip_blank_lines_strip_quotes_and_limit_unicode_characters() {
        assert_eq!(clean_title("> Attached local files (open these paths to inspect them):\n> - \"/tmp/a.png\"\n\nFix login"), "Fix login");
        assert_eq!(clean_title("\n  `Fix login`.  \nMore detail"), "Fix login");
        assert_eq!(clean_title(&"?".repeat(80)), "?".repeat(60));
        assert_eq!(clean_title("  \n"), "");
    }
}

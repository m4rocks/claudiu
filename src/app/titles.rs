use super::*;

type TextGenerator = Box<dyn Fn(&str, &str) -> Result<String, String> + Send>;

const TITLE_PROMPT: &str = "Write a short title for the session described by this message. Reply with a single title of at most 60 characters. Use no quotes or preamble.";

impl Workspace {
    pub(super) fn text_generator(&self, cx: &App) -> Result<Option<TextGenerator>, String> {
        let settings = &self.store.data.settings;
        let Some(model) = settings.text_generation_model.clone() else { return Ok(None) };
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
        let title: String = raw
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or_default()
            .trim()
            .trim_matches(['"', '\'', '`', '.'])
            .chars()
            .take(60)
            .collect();
        if title.is_empty() {
            return self.toast("Nothing to title yet.", cx);
        }
        if let Some(rec) = self.store.session_mut(id) {
            rec.custom_title = Some(title.clone());
            self.toast(format!("Renamed to “{title}”"), cx);
        }
    }
}

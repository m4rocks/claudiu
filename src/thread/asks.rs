use std::collections::VecDeque;

use gpui::{Context, Window};
use serde_json::Value;
use uuid::Uuid;

use super::{Entry, Queued, ThreadView};
use crate::harness::codex::{self, AsyncQuestion};
use crate::store::FollowUp;

#[derive(Default)]
pub(super) struct AsyncAsks {
    pending: VecDeque<AsyncAsk>,
    typing: Option<Uuid>,
    last_message: Option<MessageChunk>,
}

pub(super) struct AsyncAsk {
    pub id: Uuid,
    pub question: AsyncQuestion,
    remaining: std::vec::IntoIter<AsyncQuestion>,
    replies: Vec<String>,
}

struct MessageChunk {
    entry: usize,
    text: String,
}

impl AsyncAsks {
    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub(super) fn composing_answer(&self) -> bool {
        self.typing.is_some()
    }

    pub(super) fn record_message_chunk(&mut self, entry: usize, update: &Value) {
        self.last_message = (|| {
            if update["sessionUpdate"] != "agent_message_chunk" {
                return None;
            }
            Some(MessageChunk {
                entry,
                text: update.pointer("/content/text")?.as_str()?.to_string(),
            })
        })();
    }

    pub(super) fn turn_ended(&mut self) {
        self.last_message = None;
    }
}

impl ThreadView {
    pub(super) fn show_async_ask(&mut self, questions: Vec<AsyncQuestion>, cx: &mut Context<Self>) {
        // codex-acp 2.6 sends the form without the originating message ID. Fold only a complete matching
        // chunk at the end of the transcript so display changes in the adapter cannot hide other messages.
        if let Some(chunk) = self.asks.last_message.take()
            && chunk.text == codex::async_question_text(&questions)
            && chunk.entry + 1 == self.thread.entries.len()
            && let Some(Entry::Agent(text)) = self.thread.entries.last_mut()
            && text.ends_with(&chunk.text)
        {
            text.truncate(text.len() - chunk.text.len());
            if text.is_empty() {
                self.thread.entries.pop();
            }
            self.markdown.remove(&chunk.entry);
            self.user_md_len.remove(&chunk.entry);
            self.agent_cache.remove(&chunk.entry);
            self.remeasure(chunk.entry);
        }
        let mut remaining = questions.into_iter();
        if let Some(question) = remaining.next() {
            self.asks.pending.push_back(AsyncAsk {
                id: Uuid::new_v4(), question, remaining, replies: Vec::new(),
            });
            cx.notify();
        }
    }

    pub(super) fn async_ask(&self) -> Option<&AsyncAsk> {
        if !self.questions.is_empty() {
            return None;
        }
        self.asks.pending.front()
    }

    pub(super) fn async_answer_id(&self) -> Option<Uuid> {
        self.asks.typing.filter(|&id| self.async_ask().is_some_and(|ask| ask.id == id))
    }

    pub(super) fn async_answer_blocked(&self) -> bool {
        self.asks.composing_answer() && !self.questions.is_empty()
    }

    pub(super) fn toggle_async_answer(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        if self.async_ask().is_some_and(|ask| ask.id == id) {
            self.asks.typing = (self.asks.typing != Some(id)).then_some(id);
            self.focus_composer(window, cx);
            cx.notify();
        }
    }

    pub(super) fn choose_async_ask(&mut self, id: Uuid, option: usize, cx: &mut Context<Self>) {
        let answer = self.async_ask().filter(|ask| ask.id == id).and_then(|ask| ask.question.options.get(option)).cloned();
        if let Some(answer) = answer {
            self.answer_async_ask(id, Some(answer), cx);
        }
    }

    pub(super) fn answer_async_ask(&mut self, id: Uuid, answer: Option<String>, cx: &mut Context<Self>) {
        if self.async_ask().is_none_or(|ask| ask.id != id) {
            return;
        }
        let mut ask = self.asks.pending.pop_front().unwrap();
        self.asks.typing = None;
        if let Some(answer) = answer {
            let quoted = ask.question.title.lines().map(|line| format!("> {line}")).collect::<Vec<_>>().join("\n");
            ask.replies.push(format!("{quoted}\n\n{answer}"));
        }
        if let Some(question) = ask.remaining.next() {
            ask.id = Uuid::new_v4();
            ask.question = question;
            self.asks.pending.push_front(ask);
        } else if !ask.replies.is_empty() {
            let mut item = Queued::new(ask.replies.join("\n\n"), true);
            item.ahead = true;
            if self.native_codex_steering() && self.can_steer(&item.text, FollowUp::Steer) {
                self.steer(item, cx);
            } else {
                let at = self.queue.iter().take_while(|entry| entry.ahead).count();
                self.queue.insert(at, item);
                self.queue_changed();
                self.send_next(cx);
            }
        }
        cx.notify();
    }
}

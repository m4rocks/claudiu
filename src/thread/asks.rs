use std::collections::{BTreeSet, VecDeque};

use gpui::Context;
use serde_json::Value;
use uuid::Uuid;

use super::{Entry, Question, QuestionOption, QuestionReply, Queued, ThreadView};
use crate::harness::codex::{self, AsyncQuestion};
use crate::store::FollowUp;

#[derive(Default)]
pub(super) struct AsyncAsks {
    pending: VecDeque<Question>,
    last_message: Option<MessageChunk>,
}

struct MessageChunk {
    entry: usize,
    text: String,
}

impl AsyncAsks {
    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.pending.len()
    }

    pub(super) fn front(&self) -> Option<&Question> {
        self.pending.front()
    }

    pub(super) fn front_mut(&mut self) -> Option<&mut Question> {
        self.pending.front_mut()
    }

    pub(super) fn pop_front(&mut self) -> Option<Question> {
        self.pending.pop_front()
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

fn question_from_async(question: AsyncQuestion, remaining: VecDeque<AsyncQuestion>, replies: Vec<String>) -> Question {
    let options = question
        .options
        .into_iter()
        .enumerate()
        .map(|(i, opt)| QuestionOption {
            id: format!("async-{i}"),
            name: opt.label,
            description: opt.description,
            kind: "allow_once".into(),
        })
        .collect();
    Question {
        id: Uuid::new_v4(),
        reply: QuestionReply::AsyncAsk { remaining, replies },
        title: question.title,
        options,
        highlight: 0,
        multi_select: question.multi_select,
        selected: BTreeSet::new(),
        takes_own_answer: true,
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
        let mut remaining: VecDeque<_> = questions.into();
        if let Some(first) = remaining.pop_front() {
            self.asks.pending.push_back(question_from_async(first, remaining, Vec::new()));
            cx.notify();
        }
    }

    pub(super) fn finish_async_ask(
        &mut self,
        title: String,
        mut remaining: VecDeque<AsyncQuestion>,
        mut replies: Vec<String>,
        answer: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(answer) = answer {
            let quoted = title.lines().map(|line| format!("> {line}")).collect::<Vec<_>>().join("\n");
            replies.push(format!("{quoted}\n\n{answer}"));
        }
        if let Some(next) = remaining.pop_front() {
            self.asks.pending.push_front(question_from_async(next, remaining, replies));
        } else if !replies.is_empty() {
            let mut item = Queued::new(replies.join("\n\n"), true);
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
    }
}

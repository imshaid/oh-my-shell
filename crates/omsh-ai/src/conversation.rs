//! The thread of a `? request` that goes on after the first plan (blueprint §13.6, `[c] chat`).
//!
//! The thread holds only what the model should remember: the request, each plan it gave that
//! was accepted, and each follow-up message. A refused answer and the retry message that
//! followed it are *not* kept (they are part of one request, see [`crate::retry_request`]).
//! Every follow-up is masked here with the rules of `omsh-secrets` (§14.4), whatever the caller
//! did, like the first request.

use omsh_secrets::{looks_secret, mask};

use crate::plan::extract_object;
use crate::provider::{ChatRequest, Message, Role};

/// After this many chat turns the panel suggests editing the command instead (§13.6). Only a
/// hint: chat is never refused.
pub const SOFT_TURN_LIMIT: usize = 5;
/// The longest follow-up message accepted, in characters.
pub const MAX_FOLLOW_UP_CHARS: usize = 4000;

/// The accepted part of a conversation with the model.
#[derive(Clone, Debug)]
pub struct Conversation {
    system: Option<String>,
    max_tokens: u32,
    json: bool,
    thread: Vec<Message>,
}

impl Conversation {
    /// Starts from the first request (the instructions and the request, already masked).
    #[must_use]
    pub fn new(first: &ChatRequest) -> Self {
        Self {
            system: first.system.clone(),
            max_tokens: first.max_tokens,
            json: first.json,
            thread: first.messages.clone(),
        }
    }

    /// The model's answer to `request` was accepted. The thread becomes exactly the messages of
    /// `request` (without any retry message that `request` was answered after) plus the answer,
    /// reduced to its JSON object.
    pub fn accept(&mut self, request: &ChatRequest, answer: &str) {
        self.thread = request.messages.clone();
        let plan = extract_object(answer).unwrap_or(answer);
        self.thread.push(Message::assistant(plan));
    }

    /// How many follow-ups have been answered so far.
    #[must_use]
    pub fn turns(&self) -> usize {
        let asked = self.thread.iter().filter(|m| m.role == Role::User).count();
        asked.saturating_sub(1)
    }

    /// True once the suggestion to edit instead of chatting is due.
    #[must_use]
    pub fn hint_due(&self) -> bool {
        self.turns() >= SOFT_TURN_LIMIT
    }

    /// The request for a follow-up. `edits` are the commands the person changed by hand since
    /// the model made the plan: (step number from 1, new command). The model is told about them,
    /// so that it does not undo them.
    ///
    /// # Errors
    ///
    /// A short reason when the message is empty or longer than [`MAX_FOLLOW_UP_CHARS`].
    pub fn follow_up(&self, text: &str, edits: &[(usize, String)]) -> Result<ChatRequest, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("the message is empty".to_owned());
        }
        if text.chars().count() > MAX_FOLLOW_UP_CHARS {
            return Err(format!(
                "the message is longer than {MAX_FOLLOW_UP_CHARS} characters"
            ));
        }
        let mut message = String::new();
        for (step, command) in edits {
            message.push_str(&format!(
                "Note: before this message I changed the command of step {step} to: {}\n",
                mask(command)
            ));
        }
        message.push_str(&mask(text));
        let mut messages = self.thread.clone();
        messages.push(Message::user(message));
        Ok(ChatRequest {
            system: self.system.clone(),
            messages,
            max_tokens: self.max_tokens,
            json: self.json,
        })
    }
}

/// True if [`Conversation::follow_up`] will hide something in these inputs.
#[must_use]
pub fn follow_up_hides_secret(text: &str, edits: &[(usize, String)]) -> bool {
    looks_secret(text) || edits.iter().any(|(_, command)| looks_secret(command))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first() -> ChatRequest {
        ChatRequest {
            system: Some("instructions".to_owned()),
            messages: vec![Message::user("list files")],
            max_tokens: 99,
            json: true,
        }
    }

    fn key() -> String {
        format!("ghp_{}", "a1B2".repeat(9))
    }

    #[test]
    fn a_new_conversation_has_no_turns_and_no_hint() {
        let c = Conversation::new(&first());
        assert_eq!(c.turns(), 0);
        assert!(!c.hint_due());
    }

    #[test]
    fn a_follow_up_carries_the_whole_thread_in_order() {
        let mut c = Conversation::new(&first());
        c.accept(&first(), "Here: {\"summary\":\"one\"} done");
        let next = c.follow_up("only hidden ones", &[]).unwrap();
        assert_eq!(next.system.as_deref(), Some("instructions"));
        assert_eq!(next.max_tokens, 99);
        assert!(next.json);
        assert_eq!(
            next.messages,
            vec![
                Message::user("list files"),
                // Only the JSON object is kept, not the prose around it.
                Message::assistant("{\"summary\":\"one\"}"),
                Message::user("only hidden ones"),
            ]
        );
    }

    #[test]
    fn the_follow_up_is_not_kept_until_its_answer_is_accepted() {
        let mut c = Conversation::new(&first());
        c.accept(&first(), "{\"a\":1}");
        let next = c.follow_up("more", &[]).unwrap();
        assert_eq!(c.turns(), 0);
        c.accept(&next, "{\"b\":2}");
        assert_eq!(c.turns(), 1);
        let again = c.follow_up("again", &[]).unwrap();
        assert_eq!(again.messages.len(), 5);
        assert_eq!(again.messages[3], Message::assistant("{\"b\":2}"));
    }

    #[test]
    fn a_refused_answer_and_the_retry_message_never_enter_the_thread() {
        let mut c = Conversation::new(&first());
        c.accept(&first(), "{\"a\":1}");
        let next = c.follow_up("more", &[]).unwrap();
        // The request after a refused answer has two extra messages.
        let retry = crate::retry_request(&next, "garbage", &crate::PlanError::new("bad"));
        assert_eq!(retry.messages.len(), 5);
        // The accepted answer belongs to `next`, so the thread is `next` plus the answer.
        c.accept(&next, "{\"b\":2}");
        let after = c.follow_up("x", &[]).unwrap();
        let texts: Vec<&str> = after.messages.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["list files", "{\"a\":1}", "more", "{\"b\":2}", "x"]);
        assert!(
            !texts
                .iter()
                .any(|t| t.contains("garbage") || t.contains("not usable"))
        );
    }

    #[test]
    fn the_hint_is_due_at_the_fifth_turn_and_not_before() {
        let mut c = Conversation::new(&first());
        c.accept(&first(), "{}");
        for turn in 1..=SOFT_TURN_LIMIT {
            assert!(!c.hint_due(), "before turn {turn}");
            let next = c.follow_up("again", &[]).unwrap();
            c.accept(&next, "{}");
            assert_eq!(c.turns(), turn);
        }
        assert!(c.hint_due());
        // Chat is never refused.
        let sixth = c.follow_up("once more", &[]).unwrap();
        c.accept(&sixth, "{}");
        assert_eq!(c.turns(), SOFT_TURN_LIMIT + 1);
        assert!(c.hint_due());
    }

    #[test]
    fn the_text_and_the_edits_are_masked() {
        let mut c = Conversation::new(&first());
        c.accept(&first(), "{}");
        let text = format!("use {} for it", key());
        let edits = vec![(2, format!("curl -H 'x: {}' host", key()))];
        assert!(follow_up_hides_secret(&text, &[]));
        assert!(follow_up_hides_secret("fine", &edits));
        assert!(!follow_up_hides_secret("fine", &[(1, "ls".to_owned())]));
        let next = c.follow_up(&text, &edits).unwrap();
        let sent = &next.messages.last().unwrap().text;
        assert!(!sent.contains("ghp_"), "{sent}");
        assert!(sent.contains("[MASKED]"));
        assert!(sent.contains("changed the command of step 2"));
    }

    #[test]
    fn an_edit_is_told_to_the_model_before_the_message() {
        let mut c = Conversation::new(&first());
        c.accept(&first(), "{}");
        let next = c
            .follow_up(
                "and sort it",
                &[(1, "ls -la".to_owned()), (3, "pwd".to_owned())],
            )
            .unwrap();
        assert_eq!(
            next.messages.last().unwrap().text,
            "Note: before this message I changed the command of step 1 to: ls -la\n\
             Note: before this message I changed the command of step 3 to: pwd\n\
             and sort it"
        );
    }

    #[test]
    fn an_empty_or_huge_message_is_refused() {
        let c = Conversation::new(&first());
        assert!(c.follow_up("   ", &[]).is_err());
        assert!(
            c.follow_up(&"x".repeat(MAX_FOLLOW_UP_CHARS + 1), &[])
                .is_err()
        );
        assert!(c.follow_up(&"x".repeat(MAX_FOLLOW_UP_CHARS), &[]).is_ok());
    }
}

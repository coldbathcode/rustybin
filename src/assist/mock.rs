//! The built-in provider: deterministic answers prepared by the task
//! (`Request.mock_answer`), streamed in small pieces like a real model, with
//! estimated token counts. No key, no network: tests and demos run every AI
//! path with it.

use serde_json::Value;

use super::provider::{estimate_tokens, Event, EventStream, Request};

/// Pieces a text answer is streamed in (words).
const WORDS_PER_DELTA: usize = 6;

pub fn complete(req: &Request) -> EventStream {
    let answer = req.mock_answer.clone().unwrap_or(Value::Null);
    let text = match (&req.schema, &answer) {
        (Some(_), v) => v.to_string(),
        (None, Value::String(s)) => s.clone(),
        (None, v) => v.to_string(),
    };
    let input = estimate_tokens(&req.system) + estimate_tokens(&req.user);
    let output = estimate_tokens(&text);
    let mut events = vec![Event::Usage { input, output: 0 }];
    if req.schema.is_some() {
        events.push(Event::Delta(text));
    } else {
        let mut piece = String::new();
        let mut words = 0;
        for token in text.split_inclusive(char::is_whitespace) {
            piece.push_str(token);
            if token.ends_with(char::is_whitespace) {
                words += 1;
            }
            if words >= WORDS_PER_DELTA {
                events.push(Event::Delta(std::mem::take(&mut piece)));
                words = 0;
            }
        }
        if !piece.is_empty() {
            events.push(Event::Delta(piece));
        }
    }
    events.push(Event::Usage { input, output });
    Box::pin(futures_util::stream::iter(events))
}

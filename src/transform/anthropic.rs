use serde_json::{json, Value};

pub fn anthropic_to_opencode_request(body: &Value) -> Value {
    let model = body.get("model").and_then(|v| v.as_str()).unwrap_or("");
    let max_tokens = body.get("max_tokens").and_then(|v| v.as_u64()).unwrap_or(4096);
    let stream = body.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);
    let temperature = body.get("temperature").cloned();
    let top_p = body.get("top_p").cloned();

    let mut messages: Vec<Value> = Vec::new();

    if let Some(system) = body.get("system") {
        let system_content = match system {
            Value::String(s) => s.clone(),
            Value::Array(arr) => arr
                .iter()
                .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if !system_content.is_empty() {
            messages.push(json!({
                "role": "system",
                "content": system_content
            }));
        }
    }

    if let Some(msgs) = body.get("messages").and_then(|v| v.as_array()) {
        for msg in msgs {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user");
            let content = match msg.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(arr)) => arr
                    .iter()
                    .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n"),
                Some(Value::Null) | None => String::new(),
                _ => String::new(),
            };
            let openai_role = match role {
                "assistant" => "assistant",
                _ => "user",
            };
            messages.push(json!({
                "role": openai_role,
                "content": content
            }));
        }
    }

    let mut req = json!({
        "model": model,
        "messages": messages,
        "stream": stream,
        "max_tokens": max_tokens
    });

    if let Some(obj) = req.as_object_mut() {
        if let Some(t) = temperature {
            obj.insert("temperature".to_string(), t);
        }
        if let Some(p) = top_p {
            obj.insert("top_p".to_string(), p);
        }
    }

    req
}

pub fn opencode_response_to_anthropic(body: &Value, model: &str) -> Value {
    let mut text = String::new();
    let mut finish_reason = "stop";

    if let Some(choices) = body.get("choices").and_then(|v| v.as_array()) {
        if let Some(choice) = choices.first() {
            if let Some(message) = choice.get("message") {
                if let Some(content) = message.get("content").and_then(|v| v.as_str()) {
                    text = content.to_string();
                }
            }
            if let Some(reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                finish_reason = match reason {
                    "stop" => "end_turn",
                    "length" => "max_tokens",
                    _ => "end_turn",
                };
            }
        }
    }

    let usage = body.get("usage").cloned().unwrap_or(json!({
        "input_tokens": 0,
        "output_tokens": 0
    }));

    json!({
        "id": format!("msg_{}", uuid_short()),
        "type": "message",
        "role": "assistant",
        "content": [{
            "type": "text",
            "text": text
        }],
        "model": model,
        "stop_reason": finish_reason,
        "stop_sequence": null,
        "usage": usage
    })
}

/// Stateful converter that turns OpenAI-format SSE chunks into Anthropic-format
/// SSE events.
///
/// Anthropic's streaming contract requires, in order:
///   1. `message_start` (once, before any content)
///   2. `content_block_start` (once per content block, before deltas)
///   3. `content_block_delta` (text deltas)
///   4. `content_block_stop` (when the block ends)
///   5. `message_delta` (carries the finish reason)
///   6. `message_stop` (once, at the very end)
///
/// The previous implementation only emitted deltas/stop, which breaks clients
/// expecting the full preamble. This converter buffers the first real content
/// chunk so it can emit the preamble events, and emits `message_stop` on the
/// OpenAI `[DONE]` sentinel.
pub struct AnthropicStreamConverter {
    model: String,
    started: bool,
    block_started: bool,
    block_stopped: bool,
    message_stopped: bool,
    message_id: String,
    input_tokens: u64,
    output_tokens: u64,
}

impl AnthropicStreamConverter {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            started: false,
            block_started: false,
            block_stopped: false,
            message_stopped: false,
            message_id: format!("msg_{}", uuid_short()),
            input_tokens: 0,
            output_tokens: 0,
        }
    }

    /// Convert one upstream SSE line into zero or more Anthropic SSE events.
    /// Returns `None` for lines that carry no event (comments, blanks).
    pub fn convert(&mut self, line: &str) -> Option<String> {
        if line.is_empty() || line.starts_with(':') {
            return None;
        }

        let data = line.strip_prefix("data: ")?;

        if data == "[DONE]" {
            return Some(self.finish());
        }

        let val = serde_json::from_str::<Value>(data).ok()?;

        if let Some(usage) = val.get("usage") {
            if let Some(v) = usage.get("prompt_tokens").and_then(|v| v.as_u64()) {
                self.input_tokens = v;
            }
            if let Some(v) = usage.get("completion_tokens").and_then(|v| v.as_u64()) {
                self.output_tokens = v;
            }
        }

        if let Some(id) = val.get("id").and_then(|v| v.as_str()) {
            if !id.is_empty() {
                self.message_id = id.to_string();
            }
        }

        let mut out = String::new();

        let choices = val.get("choices").and_then(|v| v.as_array())?;
        let choice = choices.first()?;

        if let Some(delta_text) = choice
            .get("delta")
            .and_then(|d| d.get("content"))
            .and_then(|c| c.as_str())
        {
            if !delta_text.is_empty() {
                out.push_str(&self.ensure_started());
                out.push_str(&self.text_delta(delta_text));
            }
        }

        if let Some(reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
            if !self.block_stopped {
                out.push_str(&self.content_block_stop());
            }
            out.push_str(&self.message_delta(reason));
        }

        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    fn ensure_started(&mut self) -> String {
        if !self.started {
            self.started = true;
            format!(
                "event: message_start\ndata: {}\n\n",
                serde_json::json!({
                    "type": "message",
                    "id": self.message_id,
                    "role": "assistant",
                    "model": self.model,
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {"input_tokens": 0, "output_tokens": 0}
                })
            )
        } else if !self.block_started {
            self.content_block_start()
        } else {
            String::new()
        }
    }

    fn content_block_start(&mut self) -> String {
        self.block_started = true;
        format!(
            "event: content_block_start\ndata: {}\n\n",
            serde_json::json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "text", "text": ""}
            })
        )
    }

    fn text_delta(&mut self, text: &str) -> String {
        if !self.block_started {
            self.block_started = true;
        }
        format!(
            "event: content_block_delta\ndata: {}\n\n",
            serde_json::json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": text}
            })
        )
    }

    fn content_block_stop(&mut self) -> String {
        self.block_stopped = true;
        format!(
            "event: content_block_stop\ndata: {}\n\n",
            serde_json::json!({
                "type": "content_block_stop",
                "index": 0
            })
        )
    }

    fn message_delta(&mut self, reason: &str) -> String {
        let stop_reason = match reason {
            "stop" => "end_turn",
            "length" => "max_tokens",
            _ => "end_turn",
        };
        format!(
            "event: message_delta\ndata: {}\n\n",
            serde_json::json!({
                "type": "message_delta",
                "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                "usage": {"input_tokens": self.input_tokens, "output_tokens": self.output_tokens}
            })
        )
    }

    fn finish(&mut self) -> String {
        if self.message_stopped {
            return String::new();
        }
        self.message_stopped = true;

        let mut out = String::new();
        if !self.block_stopped {
            if !self.block_started {
                // No content was ever produced: still emit an empty block so
                // the client sees a balanced start/stop pair.
                out.push_str(&self.content_block_start());
            }
            out.push_str(&self.content_block_stop());
        }
        out.push_str(&format!(
            "event: message_delta\ndata: {}\n\n",
            serde_json::json!({
                "type": "message_delta",
                "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                "usage": {"input_tokens": self.input_tokens, "output_tokens": self.output_tokens}
            })
        ));
        out.push_str("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
        out
    }
}

fn uuid_short() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{:x}{:x}", duration.as_secs(), duration.subsec_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_anthropic_messages_and_generation_options() {
        let request = json!({
            "model": "example-model",
            "system": [{"type": "text", "text": "Be precise."}],
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "Hello"}]},
                {"role": "assistant", "content": "Hi"}
            ],
            "max_tokens": 512,
            "temperature": 0.2,
            "top_p": 0.8,
            "stream": true
        });

        let converted = anthropic_to_opencode_request(&request);

        assert_eq!(converted["model"], "example-model");
        assert_eq!(converted["messages"][0]["role"], "system");
        assert_eq!(converted["messages"][0]["content"], "Be precise.");
        assert_eq!(converted["messages"][1]["content"], "Hello");
        assert_eq!(converted["messages"][2]["role"], "assistant");
        assert_eq!(converted["max_tokens"], 512);
        assert_eq!(converted["temperature"], 0.2);
        assert_eq!(converted["top_p"], 0.8);
        assert_eq!(converted["stream"], true);
    }

    #[test]
    fn converts_non_streaming_response_and_stop_reason() {
        let response = opencode_response_to_anthropic(
            &json!({
                "choices": [{
                    "message": {"content": "Done"},
                    "finish_reason": "length"
                }],
                "usage": {"input_tokens": 8, "output_tokens": 12}
            }),
            "example-model",
        );

        assert_eq!(response["type"], "message");
        assert_eq!(response["role"], "assistant");
        assert_eq!(response["content"][0]["text"], "Done");
        assert_eq!(response["stop_reason"], "max_tokens");
        assert_eq!(response["usage"]["output_tokens"], 12);
    }

    #[test]
    fn converts_streaming_delta_and_completion_events() {
        let delta = opencode_stream_to_anthropic(
            r#"data: {"choices":[{"delta":{"content":"A \"quoted\" line"}}]}"#,
            "example-model",
        )
        .unwrap();
        let data = delta
            .strip_prefix("event: content_block_delta\ndata: ")
            .unwrap()
            .trim();
        let parsed: Value = serde_json::from_str(data).unwrap();
        assert_eq!(parsed["delta"]["text"], "A \"quoted\" line");

        let finish = opencode_stream_to_anthropic(
            r#"data: {"choices":[{"finish_reason":"stop"}]}"#,
            "example-model",
        )
        .unwrap();
        assert!(finish.contains("event: message_delta"));
        assert!(finish.contains("\"stop_reason\":\"end_turn\""));

        let stop = opencode_stream_to_anthropic("data: [DONE]", "example-model").unwrap();
        assert!(stop.contains("event: message_stop"));
    }
}

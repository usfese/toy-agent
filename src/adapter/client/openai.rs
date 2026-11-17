use anyhow::{bail, Context as _, Result};
use reqwest::blocking::Client;
use serde_json::{json, Value};

use crate::application::model_client::ModelClient;
use crate::application::model_setting::ModelSetting;
use crate::domain::context::Context;
use crate::domain::message::{AssistantMessage, Message, ToolCall};
use std::collections::HashSet;

pub struct OpenAiClient {
    client: Client,
    api_key: String,
    base_url: String,
}

impl OpenAiClient {
    pub fn new(api_key: String, base_url: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url,
        }
    }
}

impl ModelClient for OpenAiClient {
    fn generate(&self, context: &Context<'_>, settings: &ModelSetting) -> Result<AssistantMessage> {
        let mut messages = Vec::with_capacity(context.messages.len() + 1);
        messages.push(json!({ "role": "system", "content": context.instruction }));
        let mut pending = HashSet::new();
        for message in context.messages {
            match message {
                Message::Assistant { tool_calls, .. } => {
                    if !pending.is_empty() { bail!("assistant message follows unresolved tool calls"); }
                    for call in tool_calls {
                        if call.id.is_empty() || !pending.insert(call.id.as_str()) {
                            bail!("assistant message has empty or duplicate tool call IDs");
                        }
                    }
                }
                Message::Tool { tool_call_id, .. } => {
                    if !pending.remove(tool_call_id.as_str()) { bail!("tool result has no matching pending call: {tool_call_id}"); }
                }
                Message::User { .. } if !pending.is_empty() => bail!("user message follows unresolved tool calls"),
                Message::User { .. } => {}
            }
            messages.push(match message {
                Message::User { content } => json!({ "role": "user", "content": content }),
                Message::Assistant { content, tool_calls } => {
                    let calls = tool_calls
                        .iter()
                        .map(|call| {
                            Ok(json!({
                                "id": call.id,
                                "type": "function",
                                "function": {
                                    "name": call.name,
                                    "arguments": serde_json::to_string(&call.arguments)?,
                                },
                            }))
                        })
                        .collect::<Result<Vec<Value>>>()?;
                    let mut value = json!({ "role": "assistant", "content": content });
                    if !calls.is_empty() {
                        value["tool_calls"] = Value::Array(calls);
                    }
                    value
                }
                Message::Tool { tool_call_id, content } => json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": content,
                }),
            });
        }
        if !pending.is_empty() { bail!("conversation has unresolved tool calls"); }

        let mut request = json!({ "model": settings.model_id, "messages": messages });
        if !context.tools.is_empty() {
            request["tools"] = Value::Array(
                context.tools.iter().map(|tool| json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    },
                })).collect(),
            );
        }

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let response = self.client.post(url)
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .context("OpenAI request failed")?;
        let status = response.status();
        if !status.is_success() {
            bail!("OpenAI request returned HTTP status {status}");
        }
        let body: Value = response.json().context("Invalid OpenAI response JSON")?;
        parse_response(&body)
    }
}

fn parse_response(body: &Value) -> Result<AssistantMessage> {
        let choice = body.get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .context("OpenAI response is missing a choice")?;
        let finish_reason = choice.get("finish_reason").and_then(Value::as_str)
            .context("OpenAI response is missing a finish reason")?;
        if finish_reason != "stop" && finish_reason != "tool_calls" {
            bail!("OpenAI response did not complete successfully: {finish_reason}");
        }
        let message = choice.get("message").and_then(Value::as_object)
            .context("OpenAI response is missing an assistant message")?;
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            bail!("OpenAI response has an invalid message role");
        }
        let mut tool_calls = Vec::new();
        let mut ids = HashSet::new();
        if let Some(calls) = message.get("tool_calls").filter(|calls| !calls.is_null()) {
            let calls = calls.as_array().context("Invalid OpenAI tool_calls")?;
            for call in calls {
                if call.get("type").and_then(Value::as_str) != Some("function") {
                    bail!("Invalid OpenAI tool call type");
                }
                let id = call.get("id").and_then(Value::as_str)
                    .filter(|id| !id.is_empty()).context("OpenAI tool call is missing an id")?;
                if !ids.insert(id) {
                    bail!("OpenAI response has duplicate tool call IDs");
                }
                let function = call.get("function").and_then(Value::as_object)
                    .context("OpenAI tool call is missing a function")?;
                let name = function.get("name").and_then(Value::as_str)
                    .filter(|name| !name.is_empty()).context("OpenAI tool call is missing a name")?;
                let arguments = function.get("arguments").and_then(Value::as_str)
                    .context("OpenAI tool call is missing arguments")?;
                let arguments: Value = serde_json::from_str(arguments)
                    .context("Invalid OpenAI tool call arguments JSON")?;
                if !arguments.is_object() {
                    bail!("OpenAI tool call arguments must be a JSON object");
                }
                tool_calls.push(ToolCall { id: id.to_owned(), name: name.to_owned(), arguments });
            }
        }
        if (finish_reason == "tool_calls") != !tool_calls.is_empty() {
            bail!("OpenAI finish reason is inconsistent with tool calls");
        }
        let content = match message.get("content") {
            Some(Value::String(content)) => content.clone(),
            Some(Value::Null) if !tool_calls.is_empty() => String::new(),
            _ => bail!("OpenAI response has invalid or missing content"),
        };
        Ok(AssistantMessage { content, tool_calls })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_truncated_and_inconsistent_responses() {
        for reason in [json!("length"), json!("content_filter"), Value::Null, json!("tool_calls")] {
            let body = json!({"choices":[{"finish_reason":reason,"message":{"role":"assistant","content":"partial"}}]});
            assert!(parse_response(&body).is_err());
        }
        let body = json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"ok"}}]});
        assert_eq!(parse_response(&body).unwrap().content, "ok");
    }
}
use anyhow::{bail, Context as _, Result};
use reqwest::blocking::Client;
use serde_json::{json, Value};

use crate::application::model_client::ModelClient;
use crate::application::model_setting::ModelSetting;
use crate::domain::context::Context;
use crate::domain::message::{Message, ToolCall};

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
    fn generate(&self, context: &Context, settings: &ModelSetting) -> Result<Message> {
        let mut messages = Vec::with_capacity(context.messages.len() + 1);
        messages.push(json!({ "role": "system", "content": context.instruction }));
        for message in &context.messages {
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
        let message = body.get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .and_then(Value::as_object)
            .context("OpenAI response is missing an assistant message")?;
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            bail!("OpenAI response has an invalid message role");
        }
        let mut tool_calls = Vec::new();
        if let Some(calls) = message.get("tool_calls") {
            let calls = calls.as_array().context("Invalid OpenAI tool_calls")?;
            for call in calls {
                if call.get("type").and_then(Value::as_str) != Some("function") {
                    bail!("Invalid OpenAI tool call type");
                }
                let id = call.get("id").and_then(Value::as_str)
                    .filter(|id| !id.is_empty()).context("OpenAI tool call is missing an id")?;
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
        let content = match message.get("content") {
            Some(Value::String(content)) => content.clone(),
            Some(Value::Null) if !tool_calls.is_empty() => String::new(),
            _ => bail!("OpenAI response has invalid or missing content"),
        };
        Ok(Message::Assistant { content, tool_calls })
    }
}
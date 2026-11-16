use std::collections::HashSet;

use crate::{
    application::{
        model_client::ModelClient,
        model_setting::ModelSetting,
        role::{
            assisstant::Assisstant,
            tool_call_reviewer::{Decision, ToolCallReviewer},
        },
        session_store::SessionStore,
    },
    domain::{message::Message, session::Session},
};
use anyhow::{Context as _, bail};

pub struct AgentService {
    pub model_client: Box<dyn ModelClient>,
    pub session_store: Box<dyn SessionStore>,
    pub assistant: Assisstant,
    pub reviewer: Box<dyn ToolCallReviewer>,
    pub model_setting: ModelSetting,
    pub max_iterations: usize,
}

impl AgentService {
    pub fn list_sessions(&self) -> anyhow::Result<Vec<String>> {
        self.session_store.list()
    }

    pub fn chat(&mut self, session_id: &str, user_message: &str) -> anyhow::Result<String> {
        if self.max_iterations == 0 {
            bail!("max_iterations must be positive");
        }
        let mut session = self
            .session_store
            .load(session_id)?
            .unwrap_or_else(|| Session {
                id: session_id.to_owned(),
                messages: Vec::new(),
            });

        // A crash after execution but before persistence makes the outcome unknowable.
        // Never replay a pending call: record its uncertainty before accepting new input.
        let mut pending = Vec::new();
        for message in &session.messages {
            match message {
                Message::Assistant { tool_calls, .. } => {
                    pending.extend(tool_calls.iter().map(|c| c.id.clone()))
                }
                Message::Tool { tool_call_id, .. } => pending.retain(|id| id != tool_call_id),
                Message::User { .. } => {}
            }
        }
        for id in pending {
            session.messages.push(Message::Tool {
                tool_call_id: id,
                content: "Interrupted before a result was saved; execution outcome is unknown and was not retried.".into(),
            });
        }
        session.messages.push(Message::User {
            content: user_message.to_owned(),
        });
        self.session_store.save(&session)?;

        for _ in 0..self.max_iterations {
            let response = self
                .model_client
                .generate(&self.assistant.context(&session), &self.model_setting)?;
            let Message::Assistant {
                content,
                tool_calls,
            } = response
            else {
                bail!("model returned a non-assistant message");
            };
            let mut ids = HashSet::new();
            if tool_calls
                .iter()
                .any(|call| call.id.is_empty() || !ids.insert(&call.id))
            {
                bail!("model returned empty or duplicate tool call IDs");
            }
            session.messages.push(Message::Assistant {
                content: content.clone(),
                tool_calls: tool_calls.clone(),
            });
            self.session_store.save(&session)?;
            if tool_calls.is_empty() {
                return Ok(content);
            }

            for call in tool_calls {
                let result = match self
                    .assistant
                    .tools
                    .iter()
                    .find(|tool| tool.definition().name == call.name)
                {
                    None => format!("Unknown tool: {}", call.name),
                    Some(tool) => match self.reviewer.review(&call) {
                        Ok(Decision::Allowed) => match tool.execute(&call.arguments) {
                            Ok(output) => output,
                            Err(error) => format!("Tool execution failed: {error:#}"),
                        },
                        Ok(Decision::Rejected { reason }) => {
                            format!("Tool call rejected: {reason}")
                        }
                        Err(error) => format!("Tool review failed; call rejected: {error:#}"),
                    },
                };
                session.messages.push(Message::Tool {
                    tool_call_id: call.id,
                    content: result,
                });
                self.session_store
                    .save(&session)
                    .context("saving tool result; execution may already have occurred")?;
            }
        }
        bail!(
            "agent reached the model call limit ({})",
            self.max_iterations
        )
    }
}

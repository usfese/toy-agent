use std::collections::HashSet;

use crate::{
    application::{
        model_client::ModelClient,
        model_setting::ModelSetting,
        role::{
            assistant::Assistant,
            tool_call_reviewer::{Decision, ToolCallReviewer},
        },
        session_store::SessionStore,
    },
    domain::{
        context::Context,
        message::{AssistantMessage, Message},
        session::Session,
        tool::ToolDefinition,
    },
};
use anyhow::{Context as _, bail};

pub struct AgentService {
    model_client: Box<dyn ModelClient>,
    session_store: Box<dyn SessionStore>,
    assistant: Assistant,
    reviewer: Box<dyn ToolCallReviewer>,
    model_setting: ModelSetting,
    max_iterations: usize,
    tool_definitions: Vec<ToolDefinition>,
}

impl AgentService {
    pub fn new(
        model_client: Box<dyn ModelClient>,
        session_store: Box<dyn SessionStore>,
        assistant: Assistant,
        reviewer: Box<dyn ToolCallReviewer>,
        model_setting: ModelSetting,
        max_iterations: usize,
    ) -> anyhow::Result<Self> {
        if max_iterations == 0 {
            bail!("max_iterations must be positive");
        }
        let mut names = HashSet::new();
        let mut tool_definitions = Vec::with_capacity(assistant.tools.len());
        for tool in &assistant.tools {
            let definition = tool.definition();
            if tool.name().is_empty()
                || definition.name != tool.name()
                || !names.insert(definition.name.clone())
            {
                bail!("invalid or duplicate tool name: {}", tool.name());
            }
            tool_definitions.push(definition);
        }
        Ok(Self {
            model_client,
            session_store,
            assistant,
            reviewer,
            model_setting,
            max_iterations,
            tool_definitions,
        })
    }

    pub fn model_id(&self) -> &str {
        &self.model_setting.model_id
    }
    pub fn set_model(&mut self, model: String) {
        self.model_setting.model_id = model;
    }
    pub fn load_session(&self, id: &str) -> anyhow::Result<Option<Session>> {
        self.session_store.load(id)
    }

    pub fn list_sessions(&self) -> anyhow::Result<Vec<String>> {
        self.session_store.list()
    }

    pub fn chat(&mut self, session_id: &str, user_message: &str) -> anyhow::Result<String> {
        let _lock = self.session_store.lock(session_id)?;
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
            let context = Context {
                instruction: &self.assistant.instruction,
                tools: &self.tool_definitions,
                messages: &session.messages,
            };
            let AssistantMessage {
                content,
                tool_calls,
            } = self.model_client.generate(&context, &self.model_setting)?;
            let mut ids = HashSet::new();
            if tool_calls.iter().any(|call| {
                call.id.is_empty()
                    || call.name.is_empty()
                    || !call.arguments.is_object()
                    || !ids.insert(&call.id)
            }) {
                bail!(
                    "model returned invalid tool calls (empty or duplicate ID, empty name, or non-object arguments)"
                );
            }
            let call_count = tool_calls.len();
            let assistant_index = session.messages.len();
            session.messages.push(Message::Assistant {
                content,
                tool_calls,
            });
            self.session_store.save(&session)?;
            if call_count == 0 {
                let Message::Assistant { content, .. } = session.messages.pop().unwrap() else {
                    unreachable!()
                };
                return Ok(content);
            }

            for index in 0..call_count {
                let Message::Assistant { tool_calls, .. } = &session.messages[assistant_index]
                else {
                    unreachable!()
                };
                let call = &tool_calls[index];
                let result = match self
                    .assistant
                    .tools
                    .iter()
                    .find(|tool| tool.name() == call.name)
                {
                    None => format!("Unknown tool: {}", call.name),
                    Some(tool) => match self.reviewer.review(call) {
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
                let id = call.id.clone();
                session.messages.push(Message::Tool {
                    tool_call_id: id,
                    content: result,
                });
                self.session_store.save(&session).with_context(|| {
                    let Message::Tool { content, .. } = session.messages.last().unwrap() else { unreachable!() };
                    format!("saving tool result failed; execution may already have occurred. Result: {content}")
                })?;
            }
        }
        bail!(
            "agent reached the model call limit ({}); completed tool results were saved and will not be replayed",
            self.max_iterations
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::session_store::SessionLock;
    use crate::domain::message::ToolCall;
    use std::{cell::RefCell, rc::Rc};

    struct Guard;
    impl SessionLock for Guard {}
    struct Store(Rc<RefCell<Option<Session>>>);
    impl SessionStore for Store {
        fn lock(&self, _: &str) -> anyhow::Result<Box<dyn SessionLock>> {
            Ok(Box::new(Guard))
        }
        fn list(&self) -> anyhow::Result<Vec<String>> {
            Ok(vec![])
        }
        fn load(&self, _: &str) -> anyhow::Result<Option<Session>> {
            Ok(self.0.borrow().clone())
        }
        fn save(&mut self, session: &Session) -> anyhow::Result<()> {
            *self.0.borrow_mut() = Some(session.clone());
            Ok(())
        }
    }
    struct Model;
    impl ModelClient for Model {
        fn generate(
            &self,
            context: &Context<'_>,
            _: &ModelSetting,
        ) -> anyhow::Result<AssistantMessage> {
            assert!(context.messages.iter().any(|message| matches!(message, Message::Tool { tool_call_id, content } if tool_call_id == "pending" && content.contains("unknown"))));
            Ok(AssistantMessage {
                content: "done".into(),
                tool_calls: vec![],
            })
        }
    }
    struct Reviewer;
    impl ToolCallReviewer for Reviewer {
        fn review(&self, _: &ToolCall) -> anyhow::Result<Decision> {
            Ok(Decision::Allowed)
        }
    }
    #[test]
    fn interrupted_calls_are_resolved_without_replaying_them() {
        let saved = Rc::new(RefCell::new(Some(Session {
            id: "s".into(),
            messages: vec![Message::Assistant {
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "pending".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({}),
                }],
            }],
        })));
        let mut service = AgentService::new(
            Box::new(Model),
            Box::new(Store(saved.clone())),
            Assistant {
                instruction: String::new(),
                tools: vec![],
            },
            Box::new(Reviewer),
            ModelSetting {
                model_id: "m".into(),
            },
            1,
        )
        .unwrap();
        assert_eq!(service.chat("s", "next").unwrap(), "done");
        assert_eq!(saved.borrow().as_ref().unwrap().messages.len(), 4);
    }

    #[test]
    fn iteration_limit_saves_tool_result_without_replaying_it() {
        use crate::domain::tool::Tool;
        use std::cell::Cell;

        struct CallingModel(Rc<Cell<usize>>);
        impl ModelClient for CallingModel {
            fn generate(
                &self,
                _: &Context<'_>,
                _: &ModelSetting,
            ) -> anyhow::Result<AssistantMessage> {
                let turn = self.0.get();
                self.0.set(turn + 1);
                Ok(if turn == 0 {
                    AssistantMessage {
                        content: String::new(),
                        tool_calls: vec![ToolCall {
                            id: "call".into(),
                            name: "count".into(),
                            arguments: serde_json::json!({}),
                        }],
                    }
                } else {
                    AssistantMessage {
                        content: "done".into(),
                        tool_calls: vec![],
                    }
                })
            }
        }
        struct CountingTool(Rc<Cell<usize>>);
        impl Tool for CountingTool {
            fn name(&self) -> &str {
                "count"
            }
            fn definition(&self) -> ToolDefinition {
                ToolDefinition {
                    name: "count".into(),
                    description: String::new(),
                    parameters: serde_json::json!({}),
                }
            }
            fn execute(&self, _: &serde_json::Value) -> anyhow::Result<String> {
                self.0.set(self.0.get() + 1);
                Ok("executed".into())
            }
        }
        let saved = Rc::new(RefCell::new(None));
        let calls = Rc::new(Cell::new(0));
        let mut service = AgentService::new(
            Box::new(CallingModel(Rc::new(Cell::new(0)))),
            Box::new(Store(saved.clone())),
            Assistant {
                instruction: String::new(),
                tools: vec![Box::new(CountingTool(calls.clone()))],
            },
            Box::new(Reviewer),
            ModelSetting {
                model_id: "m".into(),
            },
            1,
        )
        .unwrap();
        let error = service.chat("s", "first").unwrap_err().to_string();
        assert!(error.contains("completed tool results were saved"));
        assert!(saved.borrow().as_ref().unwrap().messages.iter().any(
            |message| matches!(message, Message::Tool { tool_call_id, content } if tool_call_id == "call" && content == "executed")
        ));
        assert_eq!(service.chat("s", "next").unwrap(), "done");
        assert_eq!(calls.get(), 1);
    }
}

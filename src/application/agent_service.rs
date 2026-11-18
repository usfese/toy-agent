use std::collections::HashSet;

use crate::{
    application::{
        model_client::ModelClient,
        model_setting::ModelSetting,
        role::{
            Role,
            tool_call_reviewer::{Decision, ToolCallReviewer},
        },
        session_store::SessionStore,
        tool_approval::ToolApproval,
    },
    domain::{
        message::{AssistantMessage, Message},
        session::Session,
    },
};
use anyhow::{Context as _, bail};

pub struct AgentService {
    model_client: Box<dyn ModelClient>,
    session_store: Box<dyn SessionStore>,
    role: Box<dyn Role>,
    reviewer: ToolCallReviewer,
    approval: Box<dyn ToolApproval>,
    model_setting: ModelSetting,
    max_iterations: usize,
}

impl AgentService {
    pub fn new(
        model_client: Box<dyn ModelClient>,
        session_store: Box<dyn SessionStore>,
        role: Box<dyn Role>,
        reviewer: ToolCallReviewer,
        approval: Box<dyn ToolApproval>,
        model_setting: ModelSetting,
        max_iterations: usize,
    ) -> anyhow::Result<Self> {
        if max_iterations == 0 {
            bail!("max_iterations must be positive");
        }
        Ok(Self {
            model_client,
            session_store,
            role,
            reviewer,
            approval,
            model_setting,
            max_iterations,
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

        // 如果执行后、持久化前发生崩溃，执行结果将无法确定。
        // 绝不重放未完成的调用；接收新输入前，先记录其结果未知。
        // 下面处理一下上次中断的工具调用
        let mut pending = Vec::new();
        for message in &session.messages {
            match message {
                Message::Assistant { tool_calls, .. } => {
                    pending.extend(tool_calls.iter().map(|c| c.id.clone()))
                }
                Message::ToolResult { tool_call_id, .. } => pending.retain(|id| id != tool_call_id),
                Message::User { .. } | Message::PlanResult { .. } => {}
            }
        }
        for id in pending {
            session.messages.push(Message::ToolResult {
                tool_call_id: id,
                content: "Interrupted before a result was saved; execution outcome is unknown and was not retried.".into(),
            });
        }
        session.messages.push(Message::User {
            content: user_message.to_owned(),
        });
        self.session_store.save(&session)?;

        for _ in 0..self.max_iterations {
            let AssistantMessage {
                content,
                tool_calls,
            } = self.role.respond(
                self.model_client.as_ref(),
                &self.model_setting,
                &session.messages,
            )?;
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
                let result = match self.role.tool(&call.name) {
                    None => format!("Unknown tool: {}", call.name),
                    Some(tool) => match self.reviewer.review(
                        self.model_client.as_ref(),
                        &self.model_setting,
                        &session.messages,
                        call,
                    ) {
                        Ok(Decision::Allowed) => match self.approval.approve(call) {
                            Ok(Decision::Allowed) => match tool.execute(&call.arguments) {
                                Ok(output) => output,
                                Err(error) => format!("Tool execution failed: {error:#}"),
                            },
                            Ok(Decision::Rejected { reason }) => {
                                format!("Tool call rejected by user: {reason}")
                            }
                            Err(error) => format!("Tool approval failed; call rejected: {error:#}"),
                        },
                        Ok(Decision::Rejected { reason }) => {
                            format!("Tool call rejected by AI reviewer: {reason}")
                        }
                        Err(error) => format!("AI tool review failed; call rejected: {error:#}"),
                    },
                };
                let id = call.id.clone();
                session.messages.push(Message::ToolResult {
                    tool_call_id: id,
                    content: result,
                });
                self.session_store.save(&session).with_context(|| {
                    let Message::ToolResult { content, .. } = session.messages.last().unwrap() else { unreachable!() };
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
    use crate::application::role::assistant::Assistant;
    use crate::application::session_store::SessionLock;
    use crate::domain::message::{Plan, SubPlan, ToolCall};
    use crate::domain::{context::Context, tool::ToolDefinition};
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
            assert!(context.messages.iter().any(|message| matches!(message, Message::ToolResult { tool_call_id, content } if tool_call_id == "pending" && content.contains("unknown"))));
            Ok(AssistantMessage {
                content: "done".into(),
                tool_calls: vec![],
            })
        }
    }
    struct Reviewer;
    impl ToolApproval for Reviewer {
        fn approve(&self, _: &ToolCall) -> anyhow::Result<Decision> {
            Ok(Decision::Allowed)
        }
    }
    #[test]
    fn interrupted_calls_are_resolved_without_replaying_them() {
        let saved = Rc::new(RefCell::new(Some(Session {
            id: "s".into(),
            messages: vec![
                Message::Assistant {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "pending".into(),
                        name: "bash".into(),
                        arguments: serde_json::json!({}),
                    }],
                },
                Message::PlanResult {
                    goal: "goal".into(),
                    plan: Plan {
                        sub_plans: vec![SubPlan {
                            title: "stage".into(),
                            items: vec!["task".into()],
                        }],
                    },
                },
            ],
        })));
        let mut service = AgentService::new(
            Box::new(Model),
            Box::new(Store(saved.clone())),
            Box::new(Assistant::new(vec![]).unwrap()),
            ToolCallReviewer::new(),
            Box::new(Reviewer),
            ModelSetting {
                model_id: "m".into(),
            },
            1,
        )
        .unwrap();
        assert_eq!(service.chat("s", "next").unwrap(), "done");
        assert_eq!(saved.borrow().as_ref().unwrap().messages.len(), 5);
    }

    #[test]
    fn agent_accepts_another_role_with_its_own_instruction() {
        use crate::domain::tool::Tool;

        struct PlanningModel;
        impl ModelClient for PlanningModel {
            fn generate(
                &self,
                context: &Context<'_>,
                _: &ModelSetting,
            ) -> anyhow::Result<AssistantMessage> {
                assert!(
                    matches!(context.messages.last(), Some(Message::User { content }) if content == "task")
                );
                assert_eq!(context.instruction, "plan-instruction");
                Ok(AssistantMessage {
                    content: "plan ready".into(),
                    tool_calls: vec![],
                })
            }
        }
        struct PlanningRole;
        impl Role for PlanningRole {
            fn respond(
                &self,
                client: &dyn ModelClient,
                settings: &ModelSetting,
                messages: &[Message],
            ) -> anyhow::Result<AssistantMessage> {
                client.generate(
                    &Context {
                        instruction: "plan-instruction",
                        tools: &[],
                        messages,
                    },
                    settings,
                )
            }
            fn tool(&self, _: &str) -> Option<&dyn Tool> {
                None
            }
        }
        let saved = Rc::new(RefCell::new(None));
        let mut service = AgentService::new(
            Box::new(PlanningModel),
            Box::new(Store(saved)),
            Box::new(PlanningRole),
            ToolCallReviewer::new(),
            Box::new(Reviewer),
            ModelSetting {
                model_id: "m".into(),
            },
            1,
        )
        .unwrap();
        assert_eq!(service.chat("s", "task").unwrap(), "plan ready");
    }

    #[test]
    fn iteration_limit_saves_tool_result_without_replaying_it() {
        use crate::domain::tool::Tool;
        use std::cell::Cell;

        struct CallingModel(Rc<Cell<usize>>);
        impl ModelClient for CallingModel {
            fn generate(
                &self,
                context: &Context<'_>,
                _: &ModelSetting,
            ) -> anyhow::Result<AssistantMessage> {
                if context.instruction.contains("你是工具调用审查角色") {
                    return Ok(AssistantMessage {
                        content: r#"{"decision":"allowed"}"#.into(),
                        tool_calls: vec![],
                    });
                }
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
            Box::new(Assistant::new(vec![Box::new(CountingTool(calls.clone()))]).unwrap()),
            ToolCallReviewer::new(),
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
            |message| matches!(message, Message::ToolResult { tool_call_id, content } if tool_call_id == "call" && content == "executed")
        ));
        assert_eq!(service.chat("s", "next").unwrap(), "done");
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn ai_review_and_human_approval_both_gate_tool_execution() {
        use crate::domain::tool::Tool;
        use std::cell::Cell;

        #[derive(Clone, Copy)]
        enum Review {
            Allow,
            Reject,
            Invalid,
            Error,
        }
        struct ScenarioModel(Review);
        impl ModelClient for ScenarioModel {
            fn generate(
                &self,
                context: &Context<'_>,
                _: &ModelSetting,
            ) -> anyhow::Result<AssistantMessage> {
                if context.instruction.contains("你是工具调用审查角色") {
                    assert!(context.tools.is_empty());
                    let Message::User { content } = &context.messages[0] else {
                        panic!("expected review input");
                    };
                    let input: serde_json::Value = serde_json::from_str(content)?;
                    assert_eq!(input["tool_call"]["name"], "count");
                    let content = match self.0 {
                        Review::Allow => r#"{"decision":"allowed"}"#,
                        Review::Reject => r#"{"decision":"rejected","reason":"unsafe"}"#,
                        Review::Invalid => "not json",
                        Review::Error => anyhow::bail!("review model unavailable"),
                    };
                    return Ok(AssistantMessage {
                        content: content.into(),
                        tool_calls: vec![],
                    });
                }
                if matches!(context.messages.last(), Some(Message::ToolResult { .. })) {
                    return Ok(AssistantMessage {
                        content: "done".into(),
                        tool_calls: vec![],
                    });
                }
                Ok(AssistantMessage {
                    content: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "call".into(),
                        name: "count".into(),
                        arguments: serde_json::json!({}),
                    }],
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

        struct HumanApproval {
            calls: Rc<Cell<usize>>,
            allow: bool,
            fail: bool,
        }
        impl ToolApproval for HumanApproval {
            fn approve(&self, _: &ToolCall) -> anyhow::Result<Decision> {
                self.calls.set(self.calls.get() + 1);
                if self.fail {
                    anyhow::bail!("approval unavailable");
                }
                Ok(if self.allow {
                    Decision::Allowed
                } else {
                    Decision::Rejected {
                        reason: "no".into(),
                    }
                })
            }
        }

        for (review, approve, fail, expected_human, expected_exec, expected_result) in [
            (Review::Allow, true, false, 1, 1, "executed"),
            (Review::Allow, false, false, 1, 0, "rejected by user"),
            (Review::Allow, true, true, 1, 0, "approval failed"),
            (
                Review::Reject,
                true,
                false,
                0,
                0,
                "rejected by AI reviewer: unsafe",
            ),
            (Review::Invalid, true, false, 0, 0, "AI tool review failed"),
            (Review::Error, true, false, 0, 0, "AI tool review failed"),
        ] {
            let saved = Rc::new(RefCell::new(None));
            let executions = Rc::new(Cell::new(0));
            let approvals = Rc::new(Cell::new(0));
            let mut agent = AgentService::new(
                Box::new(ScenarioModel(review)),
                Box::new(Store(saved.clone())),
                Box::new(Assistant::new(vec![Box::new(CountingTool(executions.clone()))]).unwrap()),
                ToolCallReviewer::new(),
                Box::new(HumanApproval {
                    calls: approvals.clone(),
                    allow: approve,
                    fail,
                }),
                ModelSetting {
                    model_id: "m".into(),
                },
                2,
            )
            .unwrap();
            assert_eq!(agent.chat("s", "request").unwrap(), "done");
            assert_eq!(approvals.get(), expected_human);
            assert_eq!(executions.get(), expected_exec);
            assert!(saved.borrow().as_ref().unwrap().messages.iter().any(|message|
                matches!(message, Message::ToolResult { content, .. } if content.contains(expected_result))));
        }
    }

    #[test]
    fn planner_role_returns_and_persists_a_numbered_plan() {
        use crate::application::role::planner::Planner;

        struct PlanningModel;
        impl ModelClient for PlanningModel {
            fn generate(
                &self,
                context: &Context<'_>,
                _: &ModelSetting,
            ) -> anyhow::Result<AssistantMessage> {
                assert!(context.tools.is_empty());
                Ok(AssistantMessage {
                    content: r#"{"sub_plans":[{"title":"准备","items":["分析","设计"]}]}"#.into(),
                    tool_calls: vec![],
                })
            }
        }

        let saved = Rc::new(RefCell::new(None));
        let mut service = AgentService::new(
            Box::new(PlanningModel),
            Box::new(Store(saved.clone())),
            Box::new(Planner::new("完成项目")),
            ToolCallReviewer::new(),
            Box::new(Reviewer),
            ModelSetting { model_id: "m".into() },
            1,
        ).unwrap();
        let result = service.chat("s", "开始规划").unwrap();
        assert_eq!(result, "目标：完成项目\n1. 准备\n  1.1 分析\n  1.2 设计");
        assert!(matches!(
            saved.borrow().as_ref().unwrap().messages.last(),
            Some(Message::Assistant { content, tool_calls }) if content == &result && tool_calls.is_empty()
        ));
    }
}

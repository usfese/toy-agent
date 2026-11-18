use std::fmt::{self, Display, Formatter};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct AssistantMessage {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    User {
        content: String,
    },
    Assistant {
        content: String,
        tool_calls: Vec<ToolCall>,
    },
    #[serde(alias = "tool")]
    ToolResult {
        tool_call_id: String,
        content: String,
    },
    PlanResult {
        goal: String,
        plan: Plan,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    pub sub_plans: Vec<SubPlan>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubPlan {
    pub title: String,
    pub items: Vec<String>,
}

impl Display for Plan {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        for (stage_index, sub_plan) in self.sub_plans.iter().enumerate() {
            if stage_index > 0 {
                writeln!(f)?;
            }
            let stage_number = stage_index + 1;
            write!(f, "{stage_number}. {}", sub_plan.title)?;
            for (item_index, item) in sub_plan.items.iter().enumerate() {
                write!(f, "\n  {stage_number}.{} {item}", item_index + 1)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_plan() -> Plan {
        Plan {
            sub_plans: vec![
                SubPlan {
                    title: "准备".into(),
                    items: vec!["收集资料".into(), "确定范围".into()],
                },
                SubPlan {
                    title: "执行".into(),
                    items: vec!["实施".into(), "检查".into()],
                },
            ],
        }
    }

    #[test]
    fn plan_display_preserves_order_and_numbers() {
        assert_eq!(
            sample_plan().to_string(),
            "1. 准备\n  1.1 收集资料\n  1.2 确定范围\n2. 执行\n  2.1 实施\n  2.2 检查"
        );
    }

    #[test]
    fn plan_display_handles_empty_stages_and_items() {
        assert_eq!(Plan { sub_plans: vec![] }.to_string(), "");
        let plan = Plan {
            sub_plans: vec![
                SubPlan {
                    title: String::new(),
                    items: vec![],
                },
                SubPlan {
                    title: "阶段".into(),
                    items: vec![String::new()],
                },
            ],
        };
        assert_eq!(plan.to_string(), "1. \n2. 阶段\n  2.1 ");
    }

    #[test]
    fn plan_message_json_round_trip() {
        let message = Message::PlanResult {
            goal: "目标".into(),
            plan: sample_plan(),
        };
        let value = serde_json::to_value(&message).unwrap();
        assert_eq!(value["role"], "plan_result");
        assert_eq!(serde_json::from_value::<Message>(value).unwrap(), message);
    }

    #[test]
    fn tool_result_reads_legacy_and_current_roles() {
        let message = Message::ToolResult {
            tool_call_id: "call-1".into(),
            content: "result".into(),
        };
        for role in ["tool", "tool_result"] {
            let value = json!({
                "role": role,
                "tool_call_id": "call-1",
                "content": "result",
            });
            assert_eq!(serde_json::from_value::<Message>(value).unwrap(), message);
        }
        assert_eq!(
            serde_json::to_value(message).unwrap()["role"],
            "tool_result"
        );
    }
}

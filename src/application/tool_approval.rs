use crate::{application::role::tool_call_reviewer::Decision, domain::message::ToolCall};

/// Human or host approval, applied after the AI reviewer allows a call.
pub trait ToolApproval {
    fn approve(&self, call: &ToolCall) -> anyhow::Result<Decision>;
}

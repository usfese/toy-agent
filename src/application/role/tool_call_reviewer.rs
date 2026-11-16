use crate::domain::message::ToolCall;

pub enum Decision {
    Allowed,
    Rejected { reason: String },
}

/// A policy must make a fresh decision for each tool call.
pub trait ToolCallReviewer {
    fn review(&self, call: &ToolCall) -> anyhow::Result<Decision>;
}

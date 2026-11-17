use crate::domain::{message::Message, tool::ToolDefinition};

pub struct Context<'a> {
    pub instruction: &'a str,
    pub tools: &'a [ToolDefinition],
    pub messages: &'a [Message],
}

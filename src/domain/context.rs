use crate::domain::{message::Message, tool::ToolDefinition};

pub struct Context {
    pub instruction: String,
    pub tools: Vec<ToolDefinition>,
    pub messages: Vec<Message>,
}

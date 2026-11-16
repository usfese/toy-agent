use crate::domain::{context::Context, session::Session, tool::Tool};

pub struct Assisstant {
    pub instruction: String,
    pub tools: Vec<Box<dyn Tool>>,
}

impl Assisstant {
    pub fn context(&self, session: &Session) -> Context {
        Context {
            instruction: self.instruction.clone(),
            tools: self.tools.iter().map(|tool| tool.definition()).collect(),
            messages: session.messages.clone(),
        }
    }
}

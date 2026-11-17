use crate::domain::tool::Tool;

pub struct Assistant {
    pub instruction: String,
    pub tools: Vec<Box<dyn Tool>>,
}

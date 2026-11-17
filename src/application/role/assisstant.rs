use crate::domain::tool::Tool;

pub struct Assisstant {
    pub instruction: String,
    pub tools: Vec<Box<dyn Tool>>,
}

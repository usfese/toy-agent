use serde_json::Value;

#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

pub trait Tool {
    fn definition(&self) -> ToolDefinition;
    fn execute(&self, arguments: &Value) -> anyhow::Result<String>;
}

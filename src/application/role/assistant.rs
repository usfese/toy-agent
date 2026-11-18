use std::collections::HashSet;

use anyhow::bail;

use crate::{
    application::{model_client::ModelClient, model_setting::ModelSetting, role::Role},
    domain::{
        context::Context,
        message::{AssistantMessage, Message},
        tool::{Tool, ToolDefinition},
    },
};

const INSTRUCTION: &str = "
你是一个toy-agent智能体，在需要的时候使用工具，优先使用中文回答，也可以使用其他语言。
如果在项目中，可以试图寻找AGENT.md并读取以获得项目的相关信息。可以使用Markdown和mermaid。
";

pub struct Assistant {
    instruction: String,
    tools: Vec<Box<dyn Tool>>,
    tool_definitions: Vec<ToolDefinition>,
}

impl Assistant {
    pub fn new(tools: Vec<Box<dyn Tool>>) -> anyhow::Result<Self> {
        let mut names = HashSet::new();
        let mut tool_definitions = Vec::with_capacity(tools.len());
        for tool in &tools {
            let definition = tool.definition();
            if tool.name().is_empty()
                || definition.name != tool.name()
                || !names.insert(definition.name.clone())
            {
                bail!("invalid or duplicate tool name: {}", tool.name());
            }
            tool_definitions.push(definition);
        }
        Ok(Self {
            instruction: INSTRUCTION.into(),
            tools,
            tool_definitions,
        })
    }
}

impl Role for Assistant {
    fn respond(
        &self,
        client: &dyn ModelClient,
        settings: &ModelSetting,
        messages: &[Message],
    ) -> anyhow::Result<AssistantMessage> {
        let context = Context {
            instruction: &self.instruction,
            tools: &self.tool_definitions,
            messages,
        };
        client.generate(&context, settings)
    }

    fn tool(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|tool| tool.name() == name)
            .map(|tool| tool.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    struct TestTool {
        name: &'static str,
        definition_name: &'static str,
    }

    impl Tool for TestTool {
        fn name(&self) -> &str {
            self.name
        }

        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.definition_name.into(),
                description: String::new(),
                parameters: json!({}),
            }
        }

        fn execute(&self, _: &Value) -> anyhow::Result<String> {
            Ok(String::new())
        }
    }

    #[test]
    fn rejects_duplicate_or_mismatched_tool_names() {
        let tool = || {
            Box::new(TestTool {
                name: "search",
                definition_name: "search",
            }) as Box<dyn Tool>
        };
        let error = Assistant::new(vec![tool(), tool()]).err().unwrap();
        assert!(error.to_string().contains("duplicate tool name"));

        let tool = TestTool {
            name: "search",
            definition_name: "other",
        };
        let error = Assistant::new(vec![Box::new(tool)]).err().unwrap();
        assert!(error.to_string().contains("invalid or duplicate tool name"));
    }
}

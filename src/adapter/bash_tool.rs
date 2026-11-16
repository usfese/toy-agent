use std::process::Command;

use anyhow::{Context, bail};
use serde_json::{Value, json};
use crate::domain::tool::{Tool, ToolDefinition};

pub struct BashTool;

impl Tool for BashTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "bash".into(),
            description: "Run a shell command after explicit user approval; this is not sandboxed.".into(),
            parameters: json!({
                "type": "object",
                "properties": {"command": {"type": "string", "description": "Shell command"}},
                "required": ["command"],
                "additionalProperties": false
            }),
        }
    }

    fn execute(&self, arguments: &Value) -> anyhow::Result<String> {
        let object = arguments.as_object().context("bash arguments must be an object")?;
        if object.len() != 1 { bail!("bash accepts only the command argument"); }
        let command = object.get("command").and_then(Value::as_str).context("command must be a string")?;
        let output = Command::new("bash").arg("-c").arg(command).output().context("starting bash")?;
        Ok(format!("exit status: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)))
    }
}

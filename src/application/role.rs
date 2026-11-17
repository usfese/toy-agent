pub mod assistant;
pub mod planner;
pub mod tool_call_reviewer;

use crate::{
    application::{model_client::ModelClient, model_setting::ModelSetting},
    domain::{message::{AssistantMessage, Message}, tool::Tool},
};

pub trait Role {
    fn respond(
        &self,
        client: &dyn ModelClient,
        settings: &ModelSetting,
        messages: &[Message],
    ) -> anyhow::Result<AssistantMessage>;

    fn tool(&self, name: &str) -> Option<&dyn Tool>;
}

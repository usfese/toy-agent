use crate::{application::model_setting::ModelSetting, domain::{context::Context, message::AssistantMessage}};

pub trait ModelClient {
    fn generate(&self, context: &Context<'_>, settings: &ModelSetting) -> anyhow::Result<AssistantMessage>;
}

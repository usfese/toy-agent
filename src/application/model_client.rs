use crate::{application::model_setting::ModelSetting, domain::{context::Context, message::Message}};

pub trait ModelClient {
    fn generate(&self, context: &Context, settings: &ModelSetting) -> anyhow::Result<Message>;
}

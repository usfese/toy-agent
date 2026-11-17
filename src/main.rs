pub mod adapter;
pub mod application;
pub mod domain;

use adapter::{
    client::openai::OpenAiClient, config::ClientConfig, frontend::cli::TerminalReviewer,
    session_store::jsonl_session_store::JsonlSessionStore, tool::bash_tool::BashTool,
};
use anyhow::{Context, bail};
use application::{
    agent_service::AgentService, model_setting::ModelSetting, role::assisstant::Assisstant,
    session_store::SessionStore,
};
use std::env;

fn main() -> anyhow::Result<()> {
    let mut args = env::args().skip(1);
    let command = args.next().unwrap_or_default();
    let directory = env::var("TOY_AGENT_SESSIONS").unwrap_or_else(|_| ".toy-agent/sessions".into());
    if command.is_empty() || command == "ui" {
        if args.next().is_some() {
            bail!("Usage: toy-agent [ui] | list | chat <session-id> <message>");
        }
        return adapter::frontend::cli::run(directory, ClientConfig::load()?);
    }
    if command == "list" {
        for id in JsonlSessionStore::new(directory).list()? {
            println!("{id}");
        }
        return Ok(());
    }
    if command != "chat" {
        bail!("Usage: toy-agent [ui] | list | chat <session-id> <message>");
    }
    let id = args.next().context("missing session ID")?;
    let message = args.collect::<Vec<_>>().join(" ");
    if message.is_empty() {
        bail!("missing message");
    }
    let config = ClientConfig::load()?;
    if config.api_key.is_empty() || config.model.is_empty() {
        bail!("configure client.api_key and client.model, or set OPENAI_API_KEY and OPENAI_MODEL");
    }
    let mut agent = create_agent(directory, config);
    println!("{}", agent.chat(&id, &message)?);
    Ok(())
}

fn create_agent(directory: String, config: ClientConfig) -> AgentService {
    let ClientConfig {
        api_key,
        model: model_id,
        base_url,
    } = config;
    AgentService {
        model_client: Box::new(OpenAiClient::new(api_key, base_url)),
        session_store: Box::new(JsonlSessionStore::new(directory)),
        assistant: Assisstant {
            instruction: "You are a helpful assistant. Use tools only when needed.".into(),
            tools: vec![Box::new(BashTool)],
        },
        reviewer: Box::new(TerminalReviewer),
        model_setting: ModelSetting { model_id },
        max_iterations: 10,
    }
}

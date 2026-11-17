pub mod adapter;
pub mod application;
pub mod domain;

use adapter::{
    client::openai::OpenAiClient, config::ClientConfig, frontend::cli::TerminalReviewer,
    session_store::jsonl_session_store::JsonlSessionStore, tool::bash_tool::BashTool,
};
use anyhow::{Context, bail};
use application::{
    agent_service::AgentService, model_setting::ModelSetting, role::assistant::Assistant,
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
        let config = ClientConfig::load()?;
        let has_api_key = !config.api_key.is_empty();
        return adapter::frontend::cli::run(create_agent(directory, config)?, has_api_key);
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
        bail!("configure client.api_key and client.model, or set OPENAI_MODEL");
    }
    let mut agent = create_agent(directory, config)?;
    println!("{}", agent.chat(&id, &message)?);
    Ok(())
}

fn create_agent(directory: String, config: ClientConfig) -> anyhow::Result<AgentService> {
    let ClientConfig {
        api_key,
        model: model_id,
        base_url,
    } = config;
    AgentService::new(
        Box::new(OpenAiClient::new(api_key, base_url)),
        Box::new(JsonlSessionStore::new(directory)),
        Assistant {
            instruction: "You are a helpful assistant. Use tools only when needed.".into(),
            tools: vec![Box::new(BashTool)],
        },
        Box::new(TerminalReviewer),
        ModelSetting { model_id },
        10,
    )
}

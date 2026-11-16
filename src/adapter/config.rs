use std::{env, fs, io::ErrorKind, path::PathBuf};

use anyhow::{Context, bail};
use serde::Deserialize;

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ConfigFile {
    client: ClientFile,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ClientFile {
    api_key: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
}

pub struct ClientConfig {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
}

impl ClientConfig {
    pub fn load() -> anyhow::Result<Self> {
        let explicit = env::var_os("TOY_AGENT_CONFIG");
        let path = match &explicit {
            Some(path) if path.is_empty() => bail!("TOY_AGENT_CONFIG must not be empty"),
            Some(path) => PathBuf::from(path),
            None => default_path()?,
        };
        let config = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<ConfigFile>(&bytes)
                .with_context(|| format!("parsing config file {}", path.display()))?,
            Err(error) if error.kind() == ErrorKind::NotFound && explicit.is_none() => ConfigFile::default(),
            Err(error) => return Err(error).with_context(|| format!("reading config file {}", path.display())),
        };
        Ok(Self {
            api_key: env_override("OPENAI_API_KEY", config.client.api_key)?.unwrap_or_default(),
            model: env_override("OPENAI_MODEL", config.client.model)?.unwrap_or_default(),
            base_url: env_override("OPENAI_BASE_URL", config.client.base_url)?
                .unwrap_or_else(|| "https://api.openai.com/v1".into()),
        })
    }
}

fn env_override(name: &str, value: Option<String>) -> anyhow::Result<Option<String>> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(value),
        Err(error) => Err(error).with_context(|| format!("reading {name}")),
    }
}

fn default_path() -> anyhow::Result<PathBuf> {
    if cfg!(debug_assertions) {
        return Ok(PathBuf::from(".toy-agent/config.json"));
    }
    let config_home = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| env::var_os("HOME").filter(|home| !home.is_empty()).map(|home| PathBuf::from(home).join(".config")))
        .context("set XDG_CONFIG_HOME, HOME, or TOY_AGENT_CONFIG to locate the config file")?;
    Ok(config_home.join("toy-agent/config.json"))
}

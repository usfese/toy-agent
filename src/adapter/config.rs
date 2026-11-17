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
            api_key: config.client.api_key.unwrap_or_default(),
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

#[cfg(test)]
mod tests {
    use super::ClientConfig;
    use std::{env, fs, process::Command, time::{SystemTime, UNIX_EPOCH}};

    #[test]
    fn api_key_comes_only_from_config_file() {
        const CHILD: &str = "TOY_AGENT_CONFIG_CREDENTIAL_TEST_CHILD";
        if let Ok(expected) = env::var(CHILD) {
            let config = ClientConfig::load().expect("load isolated config");
            match expected.as_str() {
                "missing" => assert!(config.api_key.is_empty()),
                "file" => assert_eq!(config.api_key, "file-test-key"),
                _ => panic!("unexpected test case"),
            }
            return;
        }

        let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = env::temp_dir().join(format!("toy-agent-config-test-{}-{unique}", std::process::id()));
        fs::create_dir(&dir).expect("create isolated test directory");
        let path = dir.join("config.json");

        for (case, contents) in [
            ("missing", r#"{"client":{}}"#),
            ("file", r#"{"client":{"api_key":"file-test-key"}}"#),
        ] {
            fs::write(&path, contents).expect("write isolated test config");
            let status = Command::new(env::current_exe().expect("locate test executable"))
                .arg("--exact")
                .arg(concat!(module_path!(), "::api_key_comes_only_from_config_file"))
                .env(CHILD, case)
                .env("TOY_AGENT_CONFIG", &path)
                .env("OPENAI_API_KEY", "environment-test-key")
                .env_remove("OPENAI_MODEL")
                .env_remove("OPENAI_BASE_URL")
                .output()
                .expect("run isolated test process")
                .status;
            assert!(status.success(), "isolated {case} case failed");
        }
        fs::remove_dir_all(dir).expect("remove isolated test directory");
    }
}

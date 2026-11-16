use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};

use crate::application::session_store::SessionStore;
use crate::domain::message::Message;
use crate::domain::session::Session;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct JsonlSessionStore {
    path: PathBuf,
}

impl JsonlSessionStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn session_path(&self, id: &str) -> Result<PathBuf> {
        if !valid_id(id) {
            bail!("invalid session id: {id:?}");
        }
        Ok(self.path.join(format!("{id}.jsonl")))
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && !id.chars().any(|c| c.is_control() || c == '/' || c == '\\')
}

// Remove a temporary file on every error path, including serialization errors.
struct TemporaryFile(PathBuf);

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn create_temporary_file(directory: &Path) -> Result<(TemporaryFile, File)> {
    loop {
        let number = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!(".session-{}-{number}.tmp", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((TemporaryFile(path), file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("creating {}", path.display()));
            }
        }
    }
}

impl SessionStore for JsonlSessionStore {
    fn list(&self) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        let entries = match fs::read_dir(&self.path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(ids),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reading session directory {}", self.path.display()));
            }
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Some(id) = name.strip_suffix(".jsonl") {
                if valid_id(id) {
                    ids.push(id.to_owned());
                }
            }
        }
        ids.sort();
        Ok(ids)
    }

    fn load(&self, id: &str) -> Result<Option<Session>> {
        let path = self.session_path(id)?;
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).with_context(|| format!("opening {}", path.display())),
        };
        let mut messages = Vec::new();
        for (index, line) in BufReader::new(file).lines().enumerate() {
            let line_number = index + 1;
            let line =
                line.with_context(|| format!("reading {} line {line_number}", path.display()))?;
            let message: Message = serde_json::from_str(&line)
                .with_context(|| format!("parsing {} line {line_number}", path.display()))?;
            messages.push(message);
        }
        Ok(Some(Session {
            id: id.to_owned(),
            messages,
        }))
    }

    fn save(&mut self, session: &Session) -> Result<()> {
        let destination = self.session_path(&session.id)?;
        fs::create_dir_all(&self.path)
            .with_context(|| format!("creating session directory {}", self.path.display()))?;
        let (temporary, file) = create_temporary_file(&self.path)?;
        {
            let mut writer = BufWriter::new(file);
            for message in &session.messages {
                serde_json::to_writer(&mut writer, message)
                    .with_context(|| format!("writing {}", temporary.0.display()))?;
                writer
                    .write_all(b"\n")
                    .with_context(|| format!("writing {}", temporary.0.display()))?;
            }
            writer
                .flush()
                .with_context(|| format!("flushing {}", temporary.0.display()))?;
        }
        fs::rename(&temporary.0, &destination)
            .with_context(|| format!("replacing {}", destination.display()))?;
        Ok(())
    }
}

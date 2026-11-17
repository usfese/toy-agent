use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};

use anyhow::{Context, Result, bail};

use crate::application::session_store::{SessionLock, SessionStore};
use crate::domain::message::Message;
use crate::domain::session::Session;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct JsonlSessionStore {
    path: PathBuf,
}

struct FileLock(File);
impl SessionLock for FileLock {}
impl Drop for FileLock {
    fn drop(&mut self) {
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN); }
    }
}

fn ensure_directory(path: &Path) -> Result<()> {
    let mut missing = Vec::new();
    let mut ancestor = path;
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(ancestor.to_path_buf());
                ancestor = ancestor.parent().filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."));
            }
            Err(error) => return Err(error).with_context(|| format!("checking {}", ancestor.display())),
        }
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(path).with_context(|| format!("creating session directory {}", path.display()))?;
    check_directory(path)?;
    // Persist the directory entries created by recursive mkdir, not just files within them.
    for directory in missing.iter().rev() {
        let parent = directory.parent().filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        File::open(parent)?.sync_all().with_context(|| format!("syncing parent directory {}", parent.display()))?;
    }
    Ok(())
}

fn check_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
        bail!("unsafe session directory {}; use a directory owned by the current user with permissions 0700 (e.g. chmod 700 on that directory), or choose a private directory; existing directories were not changed", path.display());
    }
    Ok(())
}

fn check_file(file: &File, path: &Path) -> Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 || metadata.nlink() != 1 {
        bail!("unsafe session file {}; ensure it is a regular file owned by the current user with no hard links, and run chmod 600 on that file; existing files were not changed", path.display());
    }
    Ok(())
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
        match OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(&path) {
            Ok(file) => return Ok((TemporaryFile(path), file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("creating {}", path.display()));
            }
        }
    }
}

impl SessionStore for JsonlSessionStore {
    fn lock(&self, id: &str) -> Result<Box<dyn SessionLock>> {
        self.session_path(id)?;
        ensure_directory(&self.path)?;
        let path = self.path.join(format!("{id}.lock"));
        let file = OpenOptions::new().read(true).write(true).create(true).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW).open(&path)
            .with_context(|| format!("opening session lock {}", path.display()))?;
        check_file(&file, &path)?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| format!("session {id:?} is locked"));
        }
        Ok(Box::new(FileLock(file)))
    }

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
        check_directory(&self.path)?;
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
        match fs::symlink_metadata(&self.path) {
            Ok(_) => check_directory(&self.path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let file = match OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).with_context(|| format!("opening {}", path.display())),
        };
        let mut messages = Vec::new();
        check_file(&file, &path)?;
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
        ensure_directory(&self.path)?;
        if destination.exists() {
            let existing = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(&destination)?;
            check_file(&existing, &destination)?;
        }
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
            writer.flush()
                .with_context(|| format!("flushing {}", temporary.0.display()))?;
            writer.get_ref().sync_all()
                .with_context(|| format!("syncing {}", temporary.0.display()))?;
        }
        fs::rename(&temporary.0, &destination)
            .with_context(|| format!("replacing {}", destination.display()))?;
        File::open(&self.path)?.sync_all()
            .with_context(|| format!("syncing session directory {}", self.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn directory() -> PathBuf {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("toy-agent-store-test-{}-{id}", std::process::id()))
    }

    #[test]
    fn locks_span_transactions_and_private_files_are_created() {
        let path = directory();
        let mut store = JsonlSessionStore::new(&path);
        let other = JsonlSessionStore::new(&path);
        let guard = store.lock("one").unwrap();
        assert!(other.lock("one").is_err());
        assert!(other.lock("../bad").is_err());
        store.save(&Session { id: "one".into(), messages: vec![Message::User { content: "hello".into() }] }).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(path.join("one.jsonl")).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(other.load("one").unwrap().unwrap().messages.len(), 1);
        drop(guard);
        assert!(other.lock("one").is_ok());
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn refuses_insecure_existing_directory_without_changing_it() {
        let path = directory();
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(JsonlSessionStore::new(&path).lock("one").is_err());
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o755);
        fs::remove_dir_all(path).unwrap();
    }
}

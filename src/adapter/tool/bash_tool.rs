use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use serde_json::{Value, json};
use crate::domain::tool::{Tool, ToolDefinition};

const TIMEOUT: Duration = Duration::from_secs(120);
const OUTPUT_LIMIT: usize = 1024 * 1024;

// Kill the whole job even if its shell has already exited: descendants may
// still be running or holding one of its output pipes open.
struct Job {
    child: Child,
    finished: bool,
}

impl Drop for Job {
    fn drop(&mut self) {
        if !self.finished {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL);
            }
            #[cfg(not(unix))]
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn run_command(command: &str, timeout: Duration, limit: usize) -> anyhow::Result<String> {
    let mut process = Command::new("bash");
    process.arg("-c").arg(command).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        process.process_group(0);
    }
    let mut job = Job { child: process.spawn().context("starting bash")?, finished: false };
    let stdout = job.child.stdout.take().context("opening bash stdout")?;
    let stderr = job.child.stderr.take().context("opening bash stderr")?;
    let (status, stdout, stderr, truncated) =
        capture(&mut job.child, stdout, stderr, timeout, limit)?;
    job.finished = true;
    Ok(format!("exit status: {}\nstdout:\n{}\nstderr:\n{}{}",
        status,
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr),
        if truncated { "\n[output truncated: capture limit reached]" } else { "" }))
}

fn append_output(target: &mut Vec<u8>, bytes: &[u8], remaining: &mut usize, truncated: &mut bool) {
    let keep = bytes.len().min(*remaining);
    target.extend_from_slice(&bytes[..keep]);
    *remaining -= keep;
    *truncated |= keep < bytes.len();
}

#[cfg(unix)]
fn capture(
    child: &mut Child,
    stdout: std::process::ChildStdout,
    stderr: std::process::ChildStderr,
    timeout: Duration,
    limit: usize,
) -> anyhow::Result<(ExitStatus, Vec<u8>, Vec<u8>, bool)> {
    let mut pipes: [Option<Box<dyn ReadFd>>; 2] = [
        Some(Box::new(stdout)), Some(Box::new(stderr)),
    ];
    for pipe in pipes.iter().flatten() {
        let fd = pipe.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(std::io::Error::last_os_error()).context("configuring bash output");
        }
    }
    let start = Instant::now();
    let mut outputs = [Vec::new(), Vec::new()];
    let mut remaining = limit;
    let mut truncated = false;
    let mut status = None;
    let mut buffer = [0u8; 8192];
    loop {
        if start.elapsed() >= timeout {
            bail!("bash command timed out after {:.3} seconds", timeout.as_secs_f64());
        }
        if status.is_none() {
            status = child.try_wait().context("waiting for bash")?;
        }
        if pipes.iter().all(Option::is_none) && status.is_some() {
            return Ok((status.unwrap(), std::mem::take(&mut outputs[0]),
                std::mem::take(&mut outputs[1]), truncated));
        }
        let mut pollfds: [libc::pollfd; 2] = std::array::from_fn(|index| libc::pollfd {
            fd: pipes[index].as_ref().map_or(-1, |pipe| pipe.as_raw_fd()),
            events: libc::POLLIN, revents: 0,
        });
        let wait = timeout.saturating_sub(start.elapsed()).min(Duration::from_millis(20));
        let result = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as libc::nfds_t, wait.as_millis().max(1) as i32) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted { continue; }
            return Err(error).context("polling bash output");
        }
        for (index, pipe) in pipes.iter_mut().enumerate() {
            if let Some(reader) = pipe {
                match reader.read(&mut buffer) {
                    Ok(0) => *pipe = None,
                    Ok(n) => append_output(&mut outputs[index], &buffer[..n], &mut remaining, &mut truncated),
                    Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) => {}
                    Err(e) => return Err(e).context("reading bash output"),
                }
            }
        }
    }
}

#[cfg(unix)]
trait ReadFd: Read + std::os::fd::AsRawFd {}
#[cfg(unix)]
impl<T: Read + std::os::fd::AsRawFd> ReadFd for T {}

#[cfg(not(unix))]
fn capture(
    child: &mut Child,
    stdout: std::process::ChildStdout,
    stderr: std::process::ChildStderr,
    timeout: Duration,
    limit: usize,
) -> anyhow::Result<(ExitStatus, Vec<u8>, Vec<u8>, bool)> {
    use std::sync::mpsc;
    let (sender, receiver) = mpsc::sync_channel(2);
    for (index, mut pipe) in [
        Box::new(stdout) as Box<dyn Read + Send>,
        Box::new(stderr) as Box<dyn Read + Send>,
    ].into_iter().enumerate() {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match pipe.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => if sender.send((index, Ok(buffer[..n].to_vec()))).is_err() { return; },
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => { let _ = sender.send((index, Err(e))); break; }
                }
            }
        });
    }
    drop(sender);
    let start = Instant::now();
    let mut outputs = [Vec::new(), Vec::new()];
    let mut remaining = limit;
    let mut truncated = false;
    let mut status = None;
    let mut closed = false;
    loop {
        if start.elapsed() >= timeout {
            bail!("bash command timed out after {:.3} seconds", timeout.as_secs_f64());
        }
        if status.is_none() { status = child.try_wait().context("waiting for bash")?; }
        if closed && status.is_some() {
            return Ok((status.unwrap(), std::mem::take(&mut outputs[0]),
                std::mem::take(&mut outputs[1]), truncated));
        }
        if closed {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        match receiver.recv_timeout(timeout.saturating_sub(start.elapsed()).min(Duration::from_millis(20))) {
            Ok((index, result)) => append_output(&mut outputs[index], &result.context("reading bash output")?, &mut remaining, &mut truncated),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => closed = true,
        }
    }
}

pub struct BashTool;

impl Tool for BashTool {
    fn name(&self) -> &str { "bash" }

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
        run_command(command, TIMEOUT, OUTPUT_LIMIT)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn preserves_stdout_stderr_and_exit_status() {
        let output = run_command("printf out; printf err >&2; exit 7", Duration::from_secs(2), 1024).unwrap();
        assert!(output.contains("exit status: exit status: 7"));
        assert!(output.contains("stdout:\nout\nstderr:\nerr"));
        assert!(!output.contains("truncated"));
    }

    #[test]
    fn bounds_combined_output_and_drains_both_pipes() {
        let output = run_command(
            "for ((i=0;i<3000;i++)); do printf 1234567890; printf abcdefghij >&2; done",
            Duration::from_secs(5), 64,
        ).unwrap();
        let (_, streams) = output.split_once("stdout:\n").unwrap();
        let (stdout, stderr) = streams.split_once("\nstderr:\n").unwrap();
        let stderr = stderr.strip_suffix("\n[output truncated: capture limit reached]").unwrap();
        assert_eq!(stdout.len() + stderr.len(), 64);
    }

    #[test]
    fn times_out_and_kills_descendant_holding_pipe_after_shell_exits() {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "toy-agent-bash-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let command = format!("(sleep 0.5; printf survived > '{}') & exit 0", path.display());
        let result = run_command(&command, Duration::from_millis(100), 1024);
        assert!(result.unwrap_err().to_string().contains("timed out"));
        std::thread::sleep(Duration::from_millis(700));
        assert!(!path.exists(), "timed-out descendant continued running");
        let _ = std::fs::remove_file(path);
    }
}

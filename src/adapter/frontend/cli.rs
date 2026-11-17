use std::{
    io::{self, IsTerminal, Write},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::bail;
use crossterm::{
    cursor::MoveToColumn,
    event,
    execute,
    terminal::{Clear, ClearType},
};

use super::{commands::{parse, Input}, input::{add_messages, draw_editor, erase_editor, print_entries, safe_text, show_history, Editor, EditorAction, Entry, Screen}};
use crate::{
    application::{agent_service::AgentService, role::tool_call_reviewer::{Decision, ToolCallReviewer}},
    domain::message::{Message, ToolCall},
};

pub(crate) struct TerminalReviewer;

impl ToolCallReviewer for TerminalReviewer {
    fn review(&self, call: &ToolCall) -> anyhow::Result<Decision> {
        if !io::stdin().is_terminal() {
            return Ok(Decision::Rejected {
                reason: "approval requires an interactive terminal".into(),
            });
        }
        eprintln!(
            "\nTool: {}\nArguments: {}\nThis command runs with your user permissions, without a sandbox.",
            approval_text(&call.name), approval_text(&call.arguments.to_string())
        );
        eprint!("Allow? [y/N] ");
        io::stderr().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if answer.trim().eq_ignore_ascii_case("y") {
            Ok(Decision::Allowed)
        } else {
            Ok(Decision::Rejected { reason: "user did not approve".into() })
        }
    }
}

fn approval_text(text: &str) -> String {
    let mut result = String::new();
    for c in text.chars() {
        if matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') || (c.is_control() && !matches!(c, '\n' | '\t')) {
            use std::fmt::Write as _;
            write!(result, "\\u{{{:x}}}", c as u32).unwrap();
        } else {
            result.push(c);
        }
    }
    result
}

const HELP: &str = "\
/help                 Show commands and keyboard shortcuts\n\
/new [id]             Start a new session\n\
/resume <id>          Resume and show a saved session\n\
/sessions             List saved sessions\n\
/model [id]           Show or change the model\n\
/clear                Clear the screen\n\
/quit                 Exit\n\
!<command>            Run a local shell command (not sandboxed)\n\
//<text>              Send a message beginning with /\n\
Enter send · Alt+Enter or Ctrl+J newline · ↑/↓ history · PgUp/PgDn scroll · Ctrl+C clear · Ctrl+D exit";

fn session_id() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    format!("{nanos}-{}", std::process::id())
}


pub fn run(mut agent: AgentService, has_api_key: bool) -> anyhow::Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("interactive mode requires a terminal; use `toy-agent chat <session-id> <message>` for non-interactive input");
    }
    let cwd = std::env::current_dir().map(|path| path.to_string_lossy().into_owned()).unwrap_or_else(|_| "(cwd unavailable)".into());
    let mut id = session_id();
    let mut editor = Editor::default();
    let mut scroll = 0usize;
    let mut entries = vec![Entry::Info("Type /help for commands. Model tool calls require approval; shell commands are not sandboxed.".into())];
    entries.push(Entry::Status {
        id: id.clone(),
        model: if agent.model_id().is_empty() { "(model not set)".into() } else { agent.model_id().to_owned() },
        directory: cwd.clone(),
    });
    let mut printed = 0usize;
    let mut history_view = false;
    let mut prompt_visible = false;
    let mut screen = Screen::enter()?;
    loop {
        if (printed < entries.len() || entries.is_empty())
            && !matches!(entries.last(), Some(Entry::Status { .. })) {
            entries.push(Entry::Status {
                id: id.clone(),
                model: if agent.model_id().is_empty() { "(model not set)".into() } else { agent.model_id().to_owned() },
                directory: cwd.clone(),
            });
        }
        if printed < entries.len() {
            screen.suspend()?;
            let result = print_entries(&entries[printed..]);
            screen.resume()?;
            result?;
            printed = entries.len();
        }
        let editor_row = draw_editor(&editor.input, editor.cursor, prompt_visible)?;
        prompt_visible = true;
        let event = event::read()?;
        erase_editor(editor_row)?;
        let submitted = match editor.handle(event) {
            EditorAction::Submit(text) => text,
            EditorAction::Exit => break,
            EditorAction::PageUp => {
                scroll = scroll.saturating_add(10);
                show_history(&entries, scroll)?;
                prompt_visible = false;
                history_view = true;
                continue;
            }
            EditorAction::PageDown => {
                scroll = scroll.saturating_sub(10);
                show_history(&entries, scroll)?;
                prompt_visible = false;
                history_view = scroll != 0;
                continue;
            }
            EditorAction::None => continue,
        };
        execute!(io::stdout(), MoveToColumn(0), Clear(ClearType::CurrentLine))?;
        prompt_visible = false;
        let input = parse(&submitted);
        if matches!(input, Input::Empty) { continue; }
        editor.remember(submitted.clone());
        if history_view {
            show_history(&entries, 0)?;
            history_view = false;
        }
        scroll = 0;
        entries.push(Entry::User(submitted.clone()));
        if matches!(input, Input::Quit) { break; }
        if matches!(input, Input::Help) { entries.push(Entry::Info(HELP.into())); continue; }
        if matches!(input, Input::Clear) {
            entries.clear();
            printed = 0;
            execute!(io::stdout(), Clear(ClearType::All), crossterm::cursor::MoveTo(0, 0))?;
            continue;
        }
        if matches!(input, Input::Sessions) {
            match agent.list_sessions() {
                Ok(ids) if ids.is_empty() => entries.push(Entry::Info("No saved sessions.".into())),
                Ok(ids) => entries.push(Entry::Info(ids.iter().map(|name| format!("{}{}", if *name == id { "* " } else { "  " }, safe_text(name))).collect::<Vec<_>>().join("\n"))),
                Err(error) => entries.push(Entry::Error(format!("{error:#}"))),
            }
            continue;
        }
        if let Input::New(candidate) = input {
            let candidate = if candidate.is_empty() { session_id() } else { candidate.to_owned() };
            match agent.load_session(&candidate) {
                Ok(None) => { id = candidate; entries.push(Entry::Info(format!("New session: {}", safe_text(&id)))); }
                Ok(Some(_)) => entries.push(Entry::Error("Session already exists; use /resume instead.".into())),
                Err(error) => entries.push(Entry::Error(format!("{error:#}"))),
            }
            continue;
        }
        if let Input::Resume(candidate) = input {
            if candidate.is_empty() { entries.push(Entry::Error("Usage: /resume <id>".into())); continue; }
            match agent.load_session(candidate) {
                Ok(Some(session)) => { id = candidate.to_owned(); add_messages(&mut entries, &session.messages); }
                Ok(None) => entries.push(Entry::Error(format!("Session not found: {}", safe_text(candidate)))),
                Err(error) => entries.push(Entry::Error(format!("{error:#}"))),
            }
            continue;
        }
        if let Input::Model(model) = input {
            if !model.is_empty() { agent.set_model(model.to_owned()); }
            entries.push(Entry::Info(format!("Model: {}", safe_text(agent.model_id()))));
            continue;
        }
        if let Input::Shell(command) = input {
            if command.trim().is_empty() { entries.push(Entry::Error("Usage: !<shell command>".into())); continue; }
            screen.suspend()?;
            let output = print_entries(&entries[printed..]);
            screen.resume()?;
            output?;
            printed = entries.len();
            screen.suspend()?;
            println!("$ {}", safe_text(command));
            let result = Command::new("bash").arg("-c").arg(command).status();
            screen.resume()?;
            entries.push(Entry::Info(match result {
                Ok(status) => format!("!{}\nexit {status}", safe_text(command)),
                Err(error) => format!("!{}\nShell error: {error}", safe_text(command)),
            }));
            continue;
        }
        let message = match input {
            Input::Message(message) => message,
            Input::Unknown => {
                entries.push(Entry::Error("Unknown command; type /help. Use // to send a leading slash.".into()));
                continue;
            }
            _ => unreachable!(),
        };
        if !has_api_key || agent.model_id().is_empty() {
            entries.push(Entry::Error("Configure client.api_key and client.model, or set OPENAI_MODEL (or use /model <id>) before chatting.".into()));
            continue;
        }
        let previous = match agent.load_session(&id) {
            Ok(session) => session.map_or(0, |session| session.messages.len()),
            Err(error) => { entries.push(Entry::Error(format!("{error:#}"))); continue; }
        };
        entries.push(Entry::Info("Waiting for model; tool approvals appear in the terminal.".into()));
        screen.suspend()?;
        let output = print_entries(&entries[printed..]);
        if let Err(error) = output {
            screen.resume()?;
            return Err(error);
        }
        printed = entries.len();
        let result = agent.chat(&id, &message);
        screen.resume()?;
        // The submitted message was already printed before the request.
        let mut answer_in_trace = false;
        match agent.load_session(&id) {
            Ok(Some(session)) => {
                let added = &session.messages[previous.min(session.messages.len())..];
                answer_in_trace = matches!((&result, added.last()), (Ok(answer), Some(Message::Assistant { content, tool_calls })) if tool_calls.is_empty() && content == answer);
                if let Some(index) = added.iter().position(|item| matches!(item, Message::User { content } if content == &message)) {
                    add_messages(&mut entries, &added[..index]);
                    add_messages(&mut entries, &added[index + 1..]);
                } else {
                    add_messages(&mut entries, added);
                }
            }
            Ok(None) => {}
            Err(error) => {
                entries.push(Entry::Error(format!("Could not read saved trace: {error:#}")));
            }
        }
        match result {
            Ok(answer) if !answer_in_trace => entries.push(Entry::Assistant(answer)),
            Err(error) => entries.push(Entry::Error(format!("{error:#}"))),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::approval_text;

    #[test]
    fn approval_renders_all_bidi_controls_visibly() {
        let controls = "\u{061c}\u{200e}\u{200f}\u{202a}\u{202b}\u{202c}\u{202d}\u{202e}\u{2066}\u{2067}\u{2068}\u{2069}";
        assert_eq!(
            approval_text(controls),
            "\\u{61c}\\u{200e}\\u{200f}\\u{202a}\\u{202b}\\u{202c}\\u{202d}\\u{202e}\\u{2066}\\u{2067}\\u{2068}\\u{2069}"
        );
        assert_eq!(approval_text("普通 text\n\t✓"), "普通 text\n\t✓");
    }
}

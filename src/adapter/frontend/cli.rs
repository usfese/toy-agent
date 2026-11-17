use std::{
    io::{self, IsTerminal, Write},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::bail;
use crossterm::{
    cursor::{MoveToColumn, MoveUp, Show},
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{style::{Color, Modifier, Style}, text::{Line, Span}};
use unicode_width::UnicodeWidthChar;

use crate::{
    adapter::{config::ClientConfig, frontend::{ansi, markdown}},
    application::{agent_service::AgentService, role::tool_call_reviewer::{Decision, ToolCallReviewer}},
    create_agent,
    domain::message::{Message, ToolCall},
};

pub struct CliApp {
    service: AgentService,
}

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
            safe_text(&call.name), safe_text(&call.arguments.to_string())
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

fn safe_text(text: &str) -> String {
    text.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect()
}

fn status_text(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

struct Screen {
    raw: bool,
}

impl Screen {
    fn enter() -> anyhow::Result<Self> {
        terminal::enable_raw_mode()?;
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste) {
            let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen, Show);
            let _ = terminal::disable_raw_mode();
            return Err(error.into());
        }
        Ok(Self { raw: true })
    }

    // Keep the alternate screen active for shell commands and interactive approvals.
    fn suspend(&mut self) -> anyhow::Result<()> {
        execute!(io::stdout(), DisableBracketedPaste, Show)?;
        terminal::disable_raw_mode()?;
        self.raw = false;
        Ok(())
    }

    fn resume(&mut self) -> anyhow::Result<()> {
        terminal::enable_raw_mode()?;
        self.raw = true;
        execute!(io::stdout(), EnableBracketedPaste)?;
        Ok(())
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableBracketedPaste, Show, LeaveAlternateScreen);
        if self.raw {
            let _ = terminal::disable_raw_mode();
        }
    }
}

enum Entry {
    User(String),
    Assistant(String),
    Info(String),
    Error(String),
    Tool(String),
    Status { id: String, model: String, directory: String },
}

fn add_messages(entries: &mut Vec<Entry>, messages: &[Message]) {
    for message in messages {
        match message {
            Message::User { content } => entries.push(Entry::User(content.clone())),
            Message::Assistant { content, tool_calls } => {
                if !content.is_empty() { entries.push(Entry::Assistant(content.clone())); }
                for call in tool_calls {
                    entries.push(Entry::Tool(format!("call {} {}", safe_text(&call.name), safe_text(&call.arguments.to_string()))));
                }
            }
            Message::Tool { tool_call_id, content } => entries.push(Entry::Tool(format!(
                "result {}\n{}", safe_text(tool_call_id), safe_text(content)
            ))),
        }
    }
}

fn entry_lines(entries: &[Entry]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for entry in entries {
        let (label, color, text) = match entry {
            Entry::User(text) => ("you", Color::Cyan, text),
            Entry::Assistant(text) => ("assistant", Color::Green, text),
            Entry::Info(text) => ("", Color::DarkGray, text),
            Entry::Error(text) => ("error", Color::Red, text),
            Entry::Tool(text) => ("tool", Color::Yellow, text),
            Entry::Status { id, model, directory } => {
                lines.push(Line::from(vec![
                    Span::styled(" ❯ ", Style::default().fg(Color::Black).bg(Color::Blue)),
                    Span::styled(format!(" ⬢ {} ", status_text(model)), Style::default().fg(Color::Black).bg(Color::Green)),
                    Span::styled(format!(" session {} ", status_text(id)), Style::default().fg(Color::Black).bg(Color::Yellow)),
                    Span::styled(format!(" {} ", status_text(directory)), Style::default().fg(Color::White).bg(Color::DarkGray)),
                ]));
                continue;
            }
        };
        lines.push(Line::default());
        if !label.is_empty() {
            lines.push(Line::from(Span::styled(label, Style::default().fg(color).add_modifier(Modifier::BOLD))));
        }
        if matches!(entry, Entry::Assistant(_)) {
            lines.extend(markdown::render(text).lines);
        } else {
            lines.extend(safe_text(text).split('\n').map(|line| Line::styled(line.to_owned(), Style::default().fg(color))));
        }
    }
    lines
}

// Wrap explicitly so scrolling is measured in display rows, including Markdown styles.
fn wrap_lines(lines: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut result = Vec::new();
    for line in lines {
        let mut spans = Vec::new();
        let mut used = 0;
        for span in line.spans {
            let mut part = String::new();
            for c in span.content.chars() {
                let c = if c == '\t' { ' ' } else { c };
                let w = UnicodeWidthChar::width(c).unwrap_or(0);
                if used > 0 && used + w > width {
                    if !part.is_empty() { spans.push(Span::styled(std::mem::take(&mut part), span.style)); }
                    result.push(Line::from(std::mem::take(&mut spans)).style(line.style));
                    used = 0;
                }
                part.push(c);
                used += w;
            }
            if !part.is_empty() { spans.push(Span::styled(part, span.style)); }
        }
        result.push(Line::from(spans).style(line.style));
    }
    result
}

// Build visual editor rows and record the cursor in terminal columns.
fn editor_rows(input: &[char], cursor: usize, width: usize) -> (Vec<String>, usize, usize) {
    let width = width.max(1);
    let mut rows = vec![String::new()];
    let (mut row, mut col) = (0, 0);
    let (mut cursor_row, mut cursor_col) = (0, 0);
    for (index, &c) in input.iter().enumerate() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if c != '\n' && col > 0 && col + w > width {
            rows.push(String::new()); row += 1; col = 0;
        }
        if index == cursor { cursor_row = row; cursor_col = col; }
        if c == '\n' {
            rows.push(String::new()); row += 1; col = 0;
        } else {
            rows[row].push(c); col += w;
        }
    }
    if cursor == input.len() { cursor_row = row; cursor_col = col; }
    if cursor_col >= width {
        cursor_row += 1; cursor_col = 0;
        if cursor_row >= rows.len() { rows.push(String::new()); }
    }
    (rows, cursor_row, cursor_col)
}

fn print_entries(entries: &[Entry]) -> anyhow::Result<()> {
    for entry in entries {
        match entry {
            Entry::Status { id, model, directory } => println!(
                "\x1b[30;44m ❯ \x1b[30;42m ⬢ {} \x1b[30;43m session {} \x1b[37;100m {} \x1b[0m",
                status_text(model), status_text(id), status_text(directory)
            ),
            Entry::Assistant(text) => {
                println!("\n\x1b[1;32massistant\x1b[0m");
                ansi::print_markdown(text)?;
                println!();
            }
            other => {
                let (label, color, text) = match other {
                    Entry::User(text) => ("you", 36, text),
                    Entry::Info(text) => ("", 90, text),
                    Entry::Error(text) => ("error", 31, text),
                    Entry::Tool(text) => ("tool", 33, text),
                    _ => unreachable!(),
                };
                println!();
                if !label.is_empty() { println!("\x1b[1;{color}m{label}\x1b[0m"); }
                println!("\x1b[{color}m{}\x1b[0m", safe_text(text));
            }
        }
    }
    io::stdout().flush()?;
    Ok(())
}

// Only the editor is repainted during normal interaction; prior output stays in
// the terminal's own scrollback.
fn draw_editor(input: &[char], cursor: usize, prompt_visible: bool) -> anyhow::Result<usize> {
    let (width, height) = terminal::size()?;
    let (rows, cursor_row, cursor_col) = editor_rows(input, cursor, (width as usize).saturating_sub(3).max(1));
    let visible = (height as usize).saturating_sub(1).max(1);
    let first = cursor_row.saturating_add(1).saturating_sub(visible);
    let last = rows.len().min(first + visible);
    let mut out = io::stdout();
    if !prompt_visible {
        write!(out, "\x1b[36m❯ \x1b[0m")?;
    } else {
        execute!(out, MoveToColumn(2))?;
    }
    for (i, row) in rows[first..last].iter().enumerate() {
        if i > 0 { write!(out, "\r\n  ")?; }
        write!(out, "{row}")?;
    }
    let relative = cursor_row.saturating_sub(first);
    let up = last.saturating_sub(first + 1 + relative);
    if up > 0 { execute!(out, MoveUp(up as u16))?; }
    execute!(out, MoveToColumn((cursor_col + 2).min(width.saturating_sub(1) as usize) as u16))?;
    out.flush()?;
    Ok(relative)
}

fn erase_editor(cursor_row: usize) -> anyhow::Result<()> {
    let mut out = io::stdout();
    if cursor_row > 0 { execute!(out, MoveUp(cursor_row as u16))?; }
    execute!(out, MoveToColumn(2), Clear(ClearType::FromCursorDown))?;
    Ok(())
}
fn history_color(out: &mut impl Write, color: Color, background: bool) -> io::Result<()> {
    let base = if background { 48 } else { 38 };
    let index = match color {
        Color::Reset => return Ok(()),
        Color::Black => 0, Color::Red => 1, Color::Green => 2, Color::Yellow => 3,
        Color::Blue => 4, Color::Magenta => 5, Color::Cyan => 6, Color::Gray => 7,
        Color::DarkGray => 8, Color::LightRed => 9, Color::LightGreen => 10,
        Color::LightYellow => 11, Color::LightBlue => 12, Color::LightMagenta => 13,
        Color::LightCyan => 14, Color::White => 15,
        Color::Indexed(index) => index,
        Color::Rgb(r, g, b) => return write!(out, "\x1b[{base};2;{r};{g};{b}m"),
    };
    write!(out, "\x1b[{base};5;{index}m")
}


// Some terminals do not retain alternate-screen scrollback. Page keys provide a
// separate view of the recorded conversation without modifying the draft.
fn show_history(entries: &[Entry], scroll: usize) -> anyhow::Result<()> {
    let (width, height) = terminal::size()?;
    let lines = wrap_lines(entry_lines(entries), (width as usize).saturating_sub(1).max(1));
    let visible = (height as usize).saturating_sub(2).max(1);
    let end = lines.len().saturating_sub(scroll.min(lines.len().saturating_sub(visible)));
    let start = end.saturating_sub(visible);
    let mut out = io::stdout();
    execute!(out, Clear(ClearType::All), crossterm::cursor::MoveTo(0, 0))?;
    for line in &lines[start..end] {
        for span in &line.spans {
            let style = line.style.patch(span.style);
            write!(out, "\x1b[0m")?;
            if let Some(color) = style.fg { history_color(&mut out, color, false)?; }
            if let Some(color) = style.bg { history_color(&mut out, color, true)?; }
            if style.add_modifier.contains(Modifier::BOLD) { write!(out, "\x1b[1m")?; }
            if style.add_modifier.contains(Modifier::ITALIC) { write!(out, "\x1b[3m")?; }
            if style.add_modifier.contains(Modifier::UNDERLINED) { write!(out, "\x1b[4m")?; }
            write!(out, "{}", span.content)?;
        }
        write!(out, "\x1b[0m")?;
        write!(out, "\r\n")?;
    }
    out.flush()?;
    Ok(())
}

pub fn run(directory: String, config: ClientConfig) -> anyhow::Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("interactive mode requires a terminal; use `toy-agent chat <session-id> <message>` for non-interactive input");
    }
    let has_api_key = !config.api_key.is_empty();
    let cwd = std::env::current_dir().map(|path| path.to_string_lossy().into_owned()).unwrap_or_else(|_| "(cwd unavailable)".into());
    let mut app = CliApp { service: create_agent(directory, config) };
    let agent = &mut app.service;
    let mut id = session_id();
    let mut history: Vec<String> = Vec::new();
    let mut history_pos = 0;
    let mut draft = Vec::new();
    let mut input = Vec::<char>::new();
    let mut cursor = 0;
    let mut scroll = 0usize;
    let mut entries = vec![Entry::Info("Type /help for commands. Model tool calls require approval; shell commands are not sandboxed.".into())];
    entries.push(Entry::Status {
        id: id.clone(),
        model: if agent.model_setting.model_id.is_empty() { "(model not set)".into() } else { agent.model_setting.model_id.clone() },
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
                model: if agent.model_setting.model_id.is_empty() { "(model not set)".into() } else { agent.model_setting.model_id.clone() },
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
        let editor_row = draw_editor(&input, cursor, prompt_visible)?;
        prompt_visible = true;
        let event = event::read()?;
        erase_editor(editor_row)?;
        let submitted = match event {
            Event::Paste(text) => {
                for c in text.replace("\r\n", "\n").chars().filter_map(|c| match c {
                    '\n' => Some('\n'), '\t' => Some(' '), c if !c.is_control() => Some(c), _ => None,
                }) {
                    input.insert(cursor, c); cursor += 1;
                }
                None
            }
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                let alt = key.modifiers.contains(KeyModifiers::ALT);
                let mut submission = None;
                match key.code {
                    KeyCode::Char('c') if ctrl => {
                        if input.is_empty() { break; }
                        input.clear(); cursor = 0;
                    }
                    KeyCode::Char('d') if ctrl && input.is_empty() => break,
                    KeyCode::Char('j') if ctrl => { input.insert(cursor, '\n'); cursor += 1; }
                    KeyCode::Enter if alt => { input.insert(cursor, '\n'); cursor += 1; }
                    KeyCode::Enter => {
                        let text: String = input.drain(..).collect();
                        cursor = 0;
                        submission = Some(text);
                    }
                    KeyCode::Char(c) if !ctrl && !alt && !c.is_control() => { input.insert(cursor, c); cursor += 1; }
                    KeyCode::Backspace if cursor > 0 => { cursor -= 1; input.remove(cursor); }
                    KeyCode::Delete if cursor < input.len() => { input.remove(cursor); }
                    KeyCode::Left if cursor > 0 => cursor -= 1,
                    KeyCode::Right if cursor < input.len() => cursor += 1,
                    KeyCode::Home => cursor = input[..cursor].iter().rposition(|c| *c == '\n').map_or(0, |p| p + 1),
                    KeyCode::End => cursor += input[cursor..].iter().position(|c| *c == '\n').unwrap_or(input.len() - cursor),
                    KeyCode::Up if history_pos > 0 => {
                        if history_pos == history.len() { draft = input.clone(); }
                        history_pos -= 1; input = history[history_pos].chars().collect(); cursor = input.len();
                    }
                    KeyCode::Down if history_pos < history.len() => {
                        history_pos += 1;
                        input = if history_pos == history.len() { draft.clone() } else { history[history_pos].chars().collect() };
                        cursor = input.len();
                    }
                    KeyCode::PageUp => {
                        scroll = scroll.saturating_add(10);
                        show_history(&entries, scroll)?;
                        prompt_visible = false;
                        history_view = true;
                    }
                    KeyCode::PageDown => {
                        scroll = scroll.saturating_sub(10);
                        show_history(&entries, scroll)?;
                        prompt_visible = false;
                        history_view = scroll != 0;
                    }
                    _ => {}
                }
                submission
            }
            _ => None,
        };
        let Some(submitted) = submitted else { continue };
        execute!(io::stdout(), MoveToColumn(0), Clear(ClearType::CurrentLine))?;
        prompt_visible = false;
        let input = submitted.trim();
        if input.is_empty() { continue; }
        history.push(input.to_owned());
        history_pos = history.len();
        if history_view {
            show_history(&entries, 0)?;
            history_view = false;
        }
        scroll = 0;
        entries.push(Entry::User(input.to_owned()));
        if input == "/quit" || input == "/exit" { break; }
        if input == "/help" { entries.push(Entry::Info(HELP.into())); continue; }
        if input == "/clear" {
            entries.clear();
            printed = 0;
            execute!(io::stdout(), Clear(ClearType::All), crossterm::cursor::MoveTo(0, 0))?;
            continue;
        }
        if input == "/sessions" {
            match agent.list_sessions() {
                Ok(ids) if ids.is_empty() => entries.push(Entry::Info("No saved sessions.".into())),
                Ok(ids) => entries.push(Entry::Info(ids.iter().map(|name| format!("{}{}", if *name == id { "* " } else { "  " }, safe_text(name))).collect::<Vec<_>>().join("\n"))),
                Err(error) => entries.push(Entry::Error(format!("{error:#}"))),
            }
            continue;
        }
        if input == "/new" || input.starts_with("/new ") {
            let candidate = input.strip_prefix("/new").unwrap().trim();
            let candidate = if candidate.is_empty() { session_id() } else { candidate.to_owned() };
            match agent.session_store.load(&candidate) {
                Ok(None) => { id = candidate; entries.push(Entry::Info(format!("New session: {}", safe_text(&id)))); }
                Ok(Some(_)) => entries.push(Entry::Error("Session already exists; use /resume instead.".into())),
                Err(error) => entries.push(Entry::Error(format!("{error:#}"))),
            }
            continue;
        }
        if input == "/resume" || input.starts_with("/resume ") {
            let candidate = input.strip_prefix("/resume").unwrap().trim();
            if candidate.is_empty() { entries.push(Entry::Error("Usage: /resume <id>".into())); continue; }
            match agent.session_store.load(candidate) {
                Ok(Some(session)) => { id = candidate.to_owned(); add_messages(&mut entries, &session.messages); }
                Ok(None) => entries.push(Entry::Error(format!("Session not found: {}", safe_text(candidate)))),
                Err(error) => entries.push(Entry::Error(format!("{error:#}"))),
            }
            continue;
        }
        if input == "/model" || input.starts_with("/model ") {
            let model = input.strip_prefix("/model").unwrap().trim();
            if !model.is_empty() { agent.model_setting.model_id = model.to_owned(); }
            entries.push(Entry::Info(format!("Model: {}", safe_text(&agent.model_setting.model_id))));
            continue;
        }
        if let Some(command) = input.strip_prefix('!') {
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
        let message = if let Some(escaped) = input.strip_prefix("//") {
            format!("/{escaped}")
        } else if input.starts_with('/') {
            entries.push(Entry::Error("Unknown command; type /help. Use // to send a leading slash.".into()));
            continue;
        } else { input.to_owned() };
        if !has_api_key || agent.model_setting.model_id.is_empty() {
            entries.push(Entry::Error("Configure client.api_key and client.model, or set OPENAI_API_KEY and OPENAI_MODEL (or use /model <id>) before chatting.".into()));
            continue;
        }
        let previous = match agent.session_store.load(&id) {
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
        match agent.session_store.load(&id) {
            Ok(Some(session)) => {
                let mut added = &session.messages[previous.min(session.messages.len())..];
                if matches!(added.first(), Some(Message::User { content }) if content == &message) {
                    added = &added[1..];
                }
                answer_in_trace = matches!((&result, added.last()), (Ok(answer), Some(Message::Assistant { content, tool_calls })) if tool_calls.is_empty() && content == answer);
                add_messages(&mut entries, added);
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

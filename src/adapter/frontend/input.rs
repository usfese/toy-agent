use std::io::{self, Write};

use crossterm::{
    cursor::{MoveToColumn, MoveUp, Show},
    event::{DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{style::{Color, Modifier, Style}, text::{Line, Span}};
use unicode_width::UnicodeWidthChar;

use crate::{adapter::frontend::{ansi, markdown}, domain::message::Message};

#[derive(Default)]
pub(super) struct Editor {
    pub(super) input: Vec<char>,
    pub(super) cursor: usize,
    history: Vec<String>,
    history_pos: usize,
    draft: Vec<char>,
}

pub(super) enum EditorAction {
    None,
    Submit(String),
    Exit,
    PageUp,
    PageDown,
}

impl Editor {
    pub(super) fn remember(&mut self, text: String) {
        self.history.push(text);
        self.history_pos = self.history.len();
    }

    pub(super) fn handle(&mut self, event: Event) -> EditorAction {
        match event {
            Event::Paste(text) => {
                for c in text.replace("\r\n", "\n").chars().filter_map(|c| match c {
                    '\n' | '\t' => Some(c), c if !c.is_control() => Some(c), _ => None,
                }) {
                    self.input.insert(self.cursor, c);
                    self.cursor += 1;
                }
            }
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                let alt = key.modifiers.contains(KeyModifiers::ALT);
                match key.code {
                    KeyCode::Char('c') if ctrl => {
                        if self.input.is_empty() { return EditorAction::Exit; }
                        self.input.clear(); self.cursor = 0;
                    }
                    KeyCode::Char('d') if ctrl && self.input.is_empty() => return EditorAction::Exit,
                    KeyCode::Char('j') if ctrl => { self.input.insert(self.cursor, '\n'); self.cursor += 1; }
                    KeyCode::Enter if alt => { self.input.insert(self.cursor, '\n'); self.cursor += 1; }
                    KeyCode::Enter => {
                        self.cursor = 0;
                        return EditorAction::Submit(self.input.drain(..).collect());
                    }
                    KeyCode::Char(c) if !ctrl && !alt && !c.is_control() => { self.input.insert(self.cursor, c); self.cursor += 1; }
                    KeyCode::Backspace if self.cursor > 0 => { self.cursor -= 1; self.input.remove(self.cursor); }
                    KeyCode::Delete if self.cursor < self.input.len() => { self.input.remove(self.cursor); }
                    KeyCode::Left if self.cursor > 0 => self.cursor -= 1,
                    KeyCode::Right if self.cursor < self.input.len() => self.cursor += 1,
                    KeyCode::Home => self.cursor = self.input[..self.cursor].iter().rposition(|c| *c == '\n').map_or(0, |p| p + 1),
                    KeyCode::End => self.cursor += self.input[self.cursor..].iter().position(|c| *c == '\n').unwrap_or(self.input.len() - self.cursor),
                    KeyCode::Up if self.history_pos > 0 => {
                        if self.history_pos == self.history.len() { self.draft = self.input.clone(); }
                        self.history_pos -= 1;
                        self.input = self.history[self.history_pos].chars().collect();
                        self.cursor = self.input.len();
                    }
                    KeyCode::Down if self.history_pos < self.history.len() => {
                        self.history_pos += 1;
                        self.input = if self.history_pos == self.history.len() { self.draft.clone() } else { self.history[self.history_pos].chars().collect() };
                        self.cursor = self.input.len();
                    }
                    KeyCode::PageUp => return EditorAction::PageUp,
                    KeyCode::PageDown => return EditorAction::PageDown,
                    _ => {}
                }
            }
            _ => {}
        }
        EditorAction::None
    }
}
pub(super) fn safe_text(text: &str) -> String {
    text.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect()
}

fn status_text(text: &str) -> String {
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

pub(super) struct Screen {
    raw: bool,
}

impl Screen {
    pub(super) fn enter() -> anyhow::Result<Self> {
        terminal::enable_raw_mode()?;
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste) {
            let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen, Show);
            let _ = terminal::disable_raw_mode();
            return Err(error.into());
        }
        Ok(Self { raw: true })
    }

    // Keep the alternate screen active for shell commands and interactive approvals.
    pub(super) fn suspend(&mut self) -> anyhow::Result<()> {
        execute!(io::stdout(), DisableBracketedPaste, Show)?;
        terminal::disable_raw_mode()?;
        self.raw = false;
        Ok(())
    }

    pub(super) fn resume(&mut self) -> anyhow::Result<()> {
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

pub(super) enum Entry {
    User(String),
    Assistant(String),
    Info(String),
    Error(String),
    Tool(String),
    Status { id: String, model: String, directory: String },
}

pub(super) fn add_messages(entries: &mut Vec<Entry>, messages: &[Message]) {
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
        let display = if c == '\t' { ' ' } else { c };
        let w = UnicodeWidthChar::width(display).unwrap_or(0);
        if c != '\n' && col > 0 && col + w > width {
            rows.push(String::new()); row += 1; col = 0;
        }
        if index == cursor { cursor_row = row; cursor_col = col; }
        if c == '\n' {
            rows.push(String::new()); row += 1; col = 0;
        } else {
            rows[row].push(display); col += w;
        }
    }
    if cursor == input.len() { cursor_row = row; cursor_col = col; }
    if cursor_col >= width {
        cursor_row += 1; cursor_col = 0;
        if cursor_row >= rows.len() { rows.push(String::new()); }
    }
    (rows, cursor_row, cursor_col)
}

pub(super) fn print_entries(entries: &[Entry]) -> anyhow::Result<()> {
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
pub(super) fn draw_editor(input: &[char], cursor: usize, prompt_visible: bool) -> anyhow::Result<usize> {
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

pub(super) fn erase_editor(cursor_row: usize) -> anyhow::Result<()> {
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
pub(super) fn show_history(entries: &[Entry], scroll: usize) -> anyhow::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::{Editor, EditorAction};
    use crate::adapter::frontend::commands::{parse, Input};
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn pasted_message_reaches_submission_and_parser_without_trimming() {
        let mut editor = Editor::default();
        editor.handle(Event::Paste("  hello \n".into()));
        let EditorAction::Submit(submitted) = editor.handle(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))) else {
            panic!("expected submission");
        };
        assert_eq!(parse(&submitted), Input::Message("  hello \n".into()));
    }
}

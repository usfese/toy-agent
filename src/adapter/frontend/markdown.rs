use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};

/// Render Markdown as styled terminal text without passing control characters to the terminal.
pub fn render(input: &str) -> Text<'static> {
    // Filter before parsing as well as rendering: an escape sequence must never reach a widget,
    // including through a link destination or raw HTML. Preserve newlines and expand tabs.
    let mut safe = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\n' => safe.push('\n'),
            '\t' => safe.push_str("    "),
            c if !c.is_control() => safe.push(c),
            _ => {}
        }
    }

    let parser = Parser::new_ext(&safe, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS);
    let mut renderer = Renderer::default();
    for event in parser {
        renderer.event(event);
    }
    renderer.finish()
}

#[derive(Default)]
struct Renderer {
    lines: Vec<Line<'static>>,
    line: Vec<Span<'static>>,
    styles: Vec<Style>,
    lists: Vec<List>,
    quote_depth: usize,
    pending_marker: Option<String>,
    links: Vec<String>,
    code_block: bool,
    table_cell: usize,
}

struct List {
    next: Option<u64>,
    indent: usize,
}

impl Renderer {
    fn style(&self) -> Style {
        self.styles.iter().copied().fold(Style::default(), Style::patch)
    }

    fn push(&mut self, text: &str) {
        // Entity decoding can introduce controls even when the source was filtered.
        let text: String = text.chars().filter(|c| *c == '\n' || !c.is_control()).collect();
        for (index, part) in text.split('\n').enumerate() {
            if index > 0 {
                self.newline();
            }
            if part.is_empty() {
                continue;
            }
            if self.line.is_empty() {
                let mut prefix = "│ ".repeat(self.quote_depth);
                if let Some(marker) = self.pending_marker.take() {
                    let indent = self.lists.iter().take(self.lists.len().saturating_sub(1)).map(|list| list.indent).sum::<usize>();
                    prefix.push_str(&" ".repeat(indent));
                    prefix.push_str(&marker);
                } else {
                    prefix.push_str(&" ".repeat(self.lists.iter().map(|list| list.indent).sum()));
                }
                if !prefix.is_empty() {
                    self.line.push(Span::styled(prefix, Style::default().fg(Color::DarkGray)));
                }
            }
            self.line.push(Span::styled(part.to_owned(), self.style()));
        }
    }

    fn newline(&mut self) {
        self.lines.push(Line::from(std::mem::take(&mut self.line)));
    }

    fn end_line(&mut self) {
        if !self.line.is_empty() {
            self.newline();
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { .. } => {
                    self.end_line();
                    self.styles.push(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
                }
                Tag::BlockQuote(_) => {
                    self.end_line();
                    self.quote_depth += 1;
                }
                Tag::CodeBlock(_) => {
                    self.end_line();
                    self.code_block = true;
                    self.styles.push(Style::default().fg(Color::Green));
                }
                Tag::List(start) => {
                    self.end_line();
                    self.lists.push(List { next: start, indent: 2 });
                }
                Tag::Item => {
                    self.end_line();
                    if let Some(list) = self.lists.last_mut() {
                        let marker = match &mut list.next {
                            Some(number) => {
                                let marker = format!("{}. ", number);
                                *number = number.saturating_add(1);
                                marker
                            }
                            None => "• ".to_owned(),
                        };
                        list.indent = marker.chars().count();
                        self.pending_marker = Some(marker);
                    }
                }
                Tag::Emphasis => self.styles.push(Style::default().add_modifier(Modifier::ITALIC)),
                Tag::Strong => self.styles.push(Style::default().add_modifier(Modifier::BOLD)),
                Tag::Strikethrough => self.styles.push(Style::default().add_modifier(Modifier::CROSSED_OUT)),
                Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                    self.links.push(dest_url.into_string());
                    self.styles.push(Style::default().fg(Color::Blue).add_modifier(Modifier::UNDERLINED));
                }
                Tag::Table(_) => self.end_line(),
                Tag::TableHead | Tag::TableRow => {
                    self.end_line();
                    self.table_cell = 0;
                }
                Tag::TableCell => {
                    if self.table_cell > 0 {
                        self.push("  │  ");
                    }
                    self.table_cell += 1;
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph | TagEnd::Heading(_) => {
                    self.end_line();
                    if matches!(tag, TagEnd::Heading(_)) {
                        self.styles.pop();
                    }
                }
                TagEnd::BlockQuote(_) => {
                    self.end_line();
                    self.quote_depth = self.quote_depth.saturating_sub(1);
                }
                TagEnd::CodeBlock => {
                    self.end_line();
                    self.code_block = false;
                    self.styles.pop();
                }
                TagEnd::List(_) => {
                    self.end_line();
                    self.lists.pop();
                }
                TagEnd::Item => {
                    if self.pending_marker.is_some() {
                        self.push(" ");
                    }
                    self.end_line();
                    self.pending_marker = None;
                }
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => { self.styles.pop(); }
                TagEnd::Link | TagEnd::Image => {
                    self.styles.pop();
                    if let Some(url) = self.links.pop() {
                        if !url.is_empty() {
                            self.push(&format!(" ({url})"));
                        }
                    }
                }
                TagEnd::TableHead | TagEnd::TableRow | TagEnd::Table => self.end_line(),
                _ => {}
            },
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text)
            | Event::InlineMath(text) | Event::DisplayMath(text) => self.push(&text),
            Event::Code(text) => {
                self.styles.push(Style::default().fg(Color::Yellow));
                self.push(&text);
                self.styles.pop();
            }
            Event::SoftBreak => {
                if self.code_block { self.newline(); } else { self.push(" "); }
            }
            Event::HardBreak => self.newline(),
            Event::Rule => {
                self.end_line();
                self.push("────────────────────");
                self.end_line();
            }
            Event::TaskListMarker(checked) => self.push(if checked { "[x] " } else { "[ ] " }),
            Event::FootnoteReference(label) => self.push(&format!("[{label}]")),
        }
    }

    fn finish(mut self) -> Text<'static> {
        self.end_line();
        Text::from(self.lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_markdown_as_styled_readable_text() {
        let output = render("# Heading\n\n**bold** and `code` [link](https://example.com)\n\n- first\n- second\n\n```rust\nlet x = 1;\n```\n\n| A | B |\n|---|---|\n| one | two |");
        let lines: Vec<String> = output.lines.iter()
            .map(|line| line.spans.iter().map(|span| span.content.as_ref()).collect())
            .collect();
        let text = lines.join("\n");
        for expected in ["Heading", "bold", "code", "link (https://example.com)", "• first", "• second", "let x = 1;", "A  │  B", "one  │  two"] {
            assert!(text.contains(expected), "missing {expected:?} in {text:?}");
        }
        let bold = output.lines.iter().flat_map(|line| &line.spans)
            .find(|span| span.content == "bold").expect("bold span");
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn drops_control_characters_from_markdown_and_links() {
        let output = render("\u{1b}[31m text [link](https://example.com/&#27;payload)");
        for span in output.lines.iter().flat_map(|line| &line.spans) {
            assert!(!span.content.chars().any(char::is_control));
        }
    }
}

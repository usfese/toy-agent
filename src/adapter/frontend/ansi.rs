use std::io::{self, Write};

use crossterm::{
    queue,
    style::{Attribute, Color as AnsiColor, SetAttribute, SetBackgroundColor, SetForegroundColor},
};
use ratatui::style::{Color, Modifier, Style};

/// Render Markdown to the current cooked-mode terminal without emitting input controls.
pub fn print_markdown(text: &str) -> anyhow::Result<()> {
    let rendered = super::markdown::render(text);
    let stdout = io::stdout();
    let mut output = stdout.lock();

    for line in &rendered.lines {
        for span in &line.spans {
            let style = rendered.style.patch(line.style).patch(span.style);
            set_style(&mut output, style)?;
            // Defend the terminal even if the Markdown renderer later changes its filtering.
            let mut start = 0;
            for (index, ch) in span.content.char_indices() {
                if ch.is_control() {
                    output.write_all(span.content[start..index].as_bytes())?;
                    start = index + ch.len_utf8();
                }
            }
            output.write_all(span.content[start..].as_bytes())?;
        }
        output.write_all(b"\n")?;
    }
    queue!(output, SetAttribute(Attribute::Reset))?;
    output.flush()?;
    Ok(())
}

fn set_style(output: &mut impl Write, style: Style) -> io::Result<()> {
    queue!(output, SetAttribute(Attribute::Reset))?;
    if let Some(color) = style.fg {
        queue!(output, SetForegroundColor(ansi_color(color)))?;
    }
    if let Some(color) = style.bg {
        queue!(output, SetBackgroundColor(ansi_color(color)))?;
    }
    for (modifier, attribute) in [
        (Modifier::BOLD, Attribute::Bold),
        (Modifier::ITALIC, Attribute::Italic),
        (Modifier::UNDERLINED, Attribute::Underlined),
        (Modifier::CROSSED_OUT, Attribute::CrossedOut),
    ] {
        if style.add_modifier.contains(modifier) {
            queue!(output, SetAttribute(attribute))?;
        }
    }
    Ok(())
}

fn ansi_color(color: Color) -> AnsiColor {
    match color {
        Color::Reset => AnsiColor::Reset,
        Color::Black => AnsiColor::Black,
        Color::Red => AnsiColor::DarkRed,
        Color::Green => AnsiColor::DarkGreen,
        Color::Yellow => AnsiColor::DarkYellow,
        Color::Blue => AnsiColor::DarkBlue,
        Color::Magenta => AnsiColor::DarkMagenta,
        Color::Cyan => AnsiColor::DarkCyan,
        Color::Gray => AnsiColor::Grey,
        Color::DarkGray => AnsiColor::DarkGrey,
        Color::LightRed => AnsiColor::Red,
        Color::LightGreen => AnsiColor::Green,
        Color::LightYellow => AnsiColor::Yellow,
        Color::LightBlue => AnsiColor::Blue,
        Color::LightMagenta => AnsiColor::Magenta,
        Color::LightCyan => AnsiColor::Cyan,
        Color::White => AnsiColor::White,
        Color::Rgb(r, g, b) => AnsiColor::Rgb { r, g, b },
        Color::Indexed(index) => AnsiColor::AnsiValue(index),
    }
}

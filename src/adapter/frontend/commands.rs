#[derive(Debug, PartialEq, Eq)]
pub(super) enum Input<'a> {
    Empty,
    Quit,
    Help,
    Clear,
    Sessions,
    New(&'a str),
    Resume(&'a str),
    Model(&'a str),
    Shell(&'a str),
    Unknown,
    Message(String),
}

pub(super) fn parse(submitted: &str) -> Input<'_> {
    let trimmed = submitted.trim();
    if trimmed.is_empty() { return Input::Empty; }
    match trimmed {
        "/quit" | "/exit" => return Input::Quit,
        "/help" => return Input::Help,
        "/clear" => return Input::Clear,
        "/sessions" => return Input::Sessions,
        _ => {}
    }
    if trimmed == "/new" || trimmed.starts_with("/new ") {
        return Input::New(trimmed["/new".len()..].trim());
    }
    if trimmed == "/resume" || trimmed.starts_with("/resume ") {
        return Input::Resume(trimmed["/resume".len()..].trim());
    }
    if trimmed == "/model" || trimmed.starts_with("/model ") {
        return Input::Model(trimmed["/model".len()..].trim());
    }
    if let Some(command) = trimmed.strip_prefix('!') { return Input::Shell(command); }
    if trimmed.starts_with("//") {
        let slash = submitted.len() - submitted.trim_start().len();
        let mut message = submitted.to_owned();
        message.remove(slash);
        return Input::Message(message);
    }
    if trimmed.starts_with('/') { return Input::Unknown; }
    Input::Message(submitted.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{parse, Input};

    #[test]
    fn preserves_message_whitespace_and_escaped_slash() {
        assert_eq!(parse("  hello \n"), Input::Message("  hello \n".into()));
        assert_eq!(parse("  //hello \n"), Input::Message("  /hello \n".into()));
        assert_eq!(parse(" \n"), Input::Empty);
    }
}

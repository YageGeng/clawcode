/// Parse a first-line slash command of the form `/name <rest>`.
/// Returns `(name, rest_after_name, rest_offset)` if the line begins with `/`
/// and contains a non-empty name; otherwise returns `None`.
///
/// `rest_offset` is the byte index into the original line where `rest_after_name`
/// starts after trimming leading whitespace (so `line[rest_offset..] == rest_after_name`).
pub fn parse_slash_name(line: &str) -> Option<(&str, &str, usize)> {
    let stripped = line.strip_prefix('/')?;
    let (name, rest_untrimmed) = stripped
        .char_indices()
        .find_map(|(idx, ch)| ch.is_whitespace().then(|| stripped.split_at(idx)))
        .unwrap_or((stripped, ""));
    if name.is_empty() {
        return None;
    }
    let rest = rest_untrimmed.trim_start();
    let name_end_in_stripped = name.len();
    let rest_start_in_stripped = name_end_in_stripped + (rest_untrimmed.len() - rest.len());
    let rest_offset = rest_start_in_stripped + 1;
    Some((name, rest, rest_offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_command_no_args() {
        assert_eq!(parse_slash_name("/raw"), Some(("raw", "", 4)));
        assert_eq!(parse_slash_name("/sessions"), Some(("sessions", "", 9)));
    }

    #[test]
    fn command_with_args() {
        assert_eq!(parse_slash_name("/raw on"), Some(("raw", "on", 5)));
    }

    #[test]
    fn no_slash_prefix() {
        assert_eq!(parse_slash_name("hello"), None);
        assert_eq!(parse_slash_name("sessions"), None);
    }

    #[test]
    fn double_slash_is_valid_name() {
        assert_eq!(parse_slash_name("//raw"), Some(("/raw", "", 5)));
    }

    #[test]
    fn leading_whitespace() {
        assert_eq!(parse_slash_name(" /sessions"), None);
    }
}

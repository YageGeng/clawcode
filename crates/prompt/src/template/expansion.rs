/// Parsed command arguments preserving quoted whitespace as one value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CommandArguments(Vec<String>);

impl From<&str> for CommandArguments {
    /// Parses pi's quote-aware arguments with Unicode whitespace separators.
    fn from(value: &str) -> Self {
        let mut arguments = Vec::new();
        let mut current = String::new();
        let mut quote = None;

        for character in value.chars() {
            if let Some(delimiter) = quote {
                if character == delimiter {
                    quote = None;
                } else {
                    current.push(character);
                }
            } else if matches!(character, '\'' | '"') {
                quote = Some(character);
            } else if character.is_whitespace() {
                if !current.is_empty() {
                    arguments.push(std::mem::take(&mut current));
                }
            } else {
                current.push(character);
            }
        }

        if !current.is_empty() {
            arguments.push(current);
        }

        Self(arguments)
    }
}

impl CommandArguments {
    /// Expands supported placeholders in one scan over the template source.
    pub(super) fn expand(&self, template: &str) -> String {
        let mut expanded = String::with_capacity(template.len());
        let mut cursor = 0;

        while let Some(remaining) = template.get(cursor..) {
            let Some(relative_dollar) = remaining.find('$') else {
                expanded.push_str(remaining);
                break;
            };
            let dollar = cursor + relative_dollar;
            if let Some(prefix) = template.get(cursor..dollar) {
                expanded.push_str(prefix);
            }
            let Some(candidate) = template.get(dollar..) else {
                break;
            };
            if let Some((consumed, replacement)) =
                self.placeholder_replacement(candidate)
            {
                expanded.push_str(&replacement);
                cursor = dollar.saturating_add(consumed);
            } else {
                expanded.push('$');
                cursor = dollar.saturating_add(1);
            }
        }

        expanded
    }

    /// Parses and resolves the highest-priority placeholder at one dollar sign.
    fn placeholder_replacement(
        &self,
        candidate: &str,
    ) -> Option<(usize, String)> {
        if let Some(expression) = candidate.strip_prefix("${")
            && let Some(closing) = expression.find('}')
            && let Some(body) = expression.get(..closing)
        {
            if let Some((target, default_value)) = body.split_once(":-")
                && (target == "@"
                    || target == "ARGUMENTS"
                    || target
                        .chars()
                        .all(|character| character.is_ascii_digit()))
                && !target.is_empty()
            {
                let all_arguments = self.0.join(" ");
                let value = if matches!(target, "@" | "ARGUMENTS") {
                    all_arguments.as_str()
                } else {
                    target
                        .parse::<usize>()
                        .ok()
                        .and_then(|position| position.checked_sub(1))
                        .and_then(|index| self.0.get(index))
                        .map(String::as_str)
                        .unwrap_or_default()
                };
                let replacement = if value.is_empty() {
                    default_value.to_string()
                } else {
                    value.to_string()
                };
                return Some((closing.saturating_add(3), replacement));
            }

            if let Some(slice) = body.strip_prefix("@:") {
                let mut parts = slice.split(':');
                let start = parts.next()?.parse::<usize>().ok()?;
                let length =
                    parts.next().map(str::parse::<usize>).transpose().ok()?;
                if parts.next().is_none() {
                    let start_index = start.saturating_sub(1);
                    let values = self.0.get(start_index..).unwrap_or_default();
                    let selected = length
                        .map(|value| value.min(values.len()))
                        .and_then(|value| values.get(..value))
                        .unwrap_or(values);
                    return Some((
                        closing.saturating_add(3),
                        selected.join(" "),
                    ));
                }
            }
        }

        if candidate.strip_prefix("$ARGUMENTS").is_some() {
            return Some(("$ARGUMENTS".len(), self.0.join(" ")));
        }
        if candidate.strip_prefix("$@").is_some() {
            return Some((2, self.0.join(" ")));
        }
        let digits = candidate
            .strip_prefix('$')?
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>();
        if digits.is_empty() {
            return None;
        }
        let value = digits
            .parse::<usize>()
            .ok()
            .and_then(|position| position.checked_sub(1))
            .and_then(|index| self.0.get(index))
            .cloned()
            .unwrap_or_default();

        Some((digits.len().saturating_add(1), value))
    }
}

/// Parsed slash-command invocation borrowing its template name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TemplateInvocation<'a> {
    pub(super) name: &'a str,
    pub(super) arguments: CommandArguments,
}

impl<'a> TryFrom<&'a str> for TemplateInvocation<'a> {
    type Error = ();

    /// Recognizes one leading slash command and separates its argument text.
    fn try_from(value: &'a str) -> Result<Self, Self::Error> {
        let command = value.strip_prefix('/').ok_or(())?;
        if command.is_empty() {
            return Err(());
        }
        let separator = command
            .char_indices()
            .find(|(_, character)| character.is_whitespace())
            .map(|(index, _)| index);
        let (name, arguments) = match separator {
            Some(index) => (
                command.get(..index).ok_or(())?,
                command.get(index..).unwrap_or_default(),
            ),
            None => (command, ""),
        };
        if name.is_empty() {
            return Err(());
        }

        Ok(Self {
            name,
            arguments: CommandArguments::from(arguments),
        })
    }
}

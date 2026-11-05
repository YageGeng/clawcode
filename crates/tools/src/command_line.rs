//! Shell command parsing shared by command policies, plus plan mode's read-only check.
//!
//! Nothing here evaluates the command: segments are split outside quotes and words
//! come from `shlex`, so expansions and substitutions are seen only as text.

/// Programs that only read, with no option that writes files or runs other programs.
const PLAIN_READERS: &[&str] = &[
    "cat", "cd", "cut", "date", "df", "diff", "du", "echo", "grep", "head",
    "jq", "ls", "nl", "ps", "pwd", "realpath", "stat", "tail", "tr", "uname",
    "wc", "which", "whoami",
];

/// `git` subcommands that inspect a repository without changing it.
const GIT_READERS: &[&str] = &[
    "blame",
    "branch",
    "describe",
    "diff",
    "grep",
    "log",
    "ls-files",
    "remote",
    "rev-parse",
    "shortlog",
    "show",
    "status",
];

/// `git branch` arguments that only list branches; anything else may create or delete one.
const GIT_BRANCH_LISTING: &[&str] =
    &["-a", "-r", "-v", "-vv", "--list", "--show-current"];

/// `git` options that write a file or run another program.
const GIT_WRITERS: &[&str] =
    &["--output", "--ext-diff", "--open-files-in-pager"];

/// `find` primaries that delete, execute, or write files.
const FIND_WRITERS: &[&str] = &[
    "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprint0",
    "-fprintf", "-fls",
];

/// Why a command is refused in plan mode; it names the rule, never the command text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReadOnlyViolation {
    /// A segment could not be split into words, such as an unclosed quote.
    #[error("the command could not be parsed in plan mode")]
    Unparsable,
    /// Command or process substitution would run another command.
    #[error("command substitution is not allowed in plan mode")]
    Substitution,
    /// Output redirection would write a file.
    #[error("output redirection is not allowed in plan mode")]
    Redirect,
    /// The program is not on the read-only list.
    #[error("{0} is not allowed in plan mode")]
    Program(String),
    /// An allowed program was given an option that writes or executes.
    #[error("{program} {argument} is not allowed in plan mode")]
    Argument {
        /// Program, or `git <subcommand>`, that received the option.
        program: String,
        /// Option name without its value.
        argument: String,
    },
    /// `git` options before the subcommand can change how git runs.
    #[error("git global options are not allowed in plan mode: {0}")]
    GitGlobalOption(String),
}

/// Parsed direct commands separated outside shell quotes.
pub struct ShellCommandLine {
    invocations: Vec<CommandInvocation>,
}

impl ShellCommandLine {
    /// Returns the commands that could be parsed, in source order.
    #[must_use]
    pub fn invocations(&self) -> &[CommandInvocation] {
        &self.invocations
    }

    /// Accepts a command only when every part of it is a known read-only invocation.
    ///
    /// Unlike the lenient [`From`] parse, a segment that cannot be parsed refuses
    /// the whole command, because it could hide anything.
    pub fn read_only(command: &str) -> Result<(), ReadOnlyViolation> {
        let scan = ShellScan::from(command);
        if let Some(violation) = scan.violation {
            return Err(violation);
        }
        for segment in &scan.segments {
            CommandInvocation::try_from(segment.as_str())
                .map_err(|()| ReadOnlyViolation::Unparsable)?
                .read_only()?;
        }
        Ok(())
    }
}

impl From<&str> for ShellCommandLine {
    /// Keeps every segment that parses; command-guard only looks for what it rejects.
    fn from(command: &str) -> Self {
        Self {
            invocations: ShellScan::from(command)
                .segments
                .iter()
                .filter_map(|segment| {
                    CommandInvocation::try_from(segment.as_str()).ok()
                })
                .collect(),
        }
    }
}

/// One quote-aware pass over a command: its segments and the first read-only breach.
struct ShellScan {
    segments: Vec<String>,
    violation: Option<ReadOnlyViolation>,
}

impl ShellScan {
    /// Records the first breach; later ones add nothing to the reason.
    fn breach(&mut self, violation: ReadOnlyViolation) {
        self.violation.get_or_insert(violation);
    }

    /// Skips the redirection whose first `>` ends just before `index` and returns
    /// where the command resumes.
    ///
    /// `>&N` duplicates a descriptor and `/dev/null` discards output; any other
    /// target is a file the command would write.
    fn consume_redirection(
        &mut self,
        characters: &[char],
        mut index: usize,
    ) -> usize {
        if characters.get(index) == Some(&'>') {
            index += 1;
        }
        if characters.get(index) == Some(&'&') {
            index += 1;
            let start = index;
            while characters.get(index).is_some_and(char::is_ascii_digit) {
                index += 1;
            }
            if index == start {
                self.breach(ReadOnlyViolation::Redirect);
            }
            return index;
        }
        while characters
            .get(index)
            .is_some_and(|c| *c == ' ' || *c == '\t')
        {
            index += 1;
        }
        let start = index;
        while characters
            .get(index)
            .is_some_and(|c| !c.is_whitespace() && !";|&<>".contains(*c))
        {
            index += 1;
        }
        let target = characters
            .get(start..index)
            .map(|target| target.iter().collect::<String>())
            .unwrap_or_default();
        if target != "/dev/null" {
            self.breach(ReadOnlyViolation::Redirect);
        }
        index
    }

    /// Judges the character after an active `$`.
    ///
    /// `${...}` and `$[...]` can evaluate a variable's text as arithmetic or a
    /// prompt, which runs substitutions stored in it, so they refuse like `$(`.
    /// Unquoted `$'...'` is ANSI-C quoting, whose escaped `'` this scan does not
    /// follow, so it refuses rather than misjudge where the quote ends.
    fn dollar(&mut self, next: Option<&char>, double_quoted: bool) {
        match next {
            Some('{' | '[') => self.breach(ReadOnlyViolation::Substitution),
            Some('\'') if !double_quoted => {
                self.breach(ReadOnlyViolation::Unparsable);
            }
            _ => {}
        }
    }

    /// Ends the current segment at a control operator.
    fn split(&mut self, current: &mut String) {
        if !current.trim().is_empty() {
            self.segments.push(std::mem::take(current));
        }
        current.clear();
    }
}

impl From<&str> for ShellScan {
    /// Splits on unquoted `;`, `|`, `&`, and newlines, and removes redirections from
    /// the words while judging them: only fd duplication and `/dev/null` targets pass.
    fn from(command: &str) -> Self {
        let mut scan = Self {
            segments: Vec::new(),
            violation: None,
        };
        // Substitution runs even inside double quotes, so any occurrence refuses.
        if ["$(", "`", "<(", ">("]
            .iter()
            .any(|marker| command.contains(marker))
        {
            scan.breach(ReadOnlyViolation::Substitution);
        }
        let characters = command.chars().collect::<Vec<_>>();
        let mut current = String::new();
        let mut quote = None;
        let mut index = 0;
        while let Some(&character) = characters.get(index) {
            index += 1;
            if let Some(active) = quote {
                current.push(character);
                if character == '\\' && active == '"' {
                    if let Some(&next) = characters.get(index) {
                        current.push(next);
                        index += 1;
                    }
                } else if character == '$' && active == '"' {
                    scan.dollar(characters.get(index), true);
                } else if character == active {
                    quote = None;
                }
                continue;
            }
            match character {
                '\\' => {
                    current.push(character);
                    if let Some(&next) = characters.get(index) {
                        current.push(next);
                        index += 1;
                    }
                }
                '\'' | '"' => {
                    quote = Some(character);
                    current.push(character);
                }
                '$' => {
                    scan.dollar(characters.get(index), false);
                    current.push(character);
                }
                // A comment runs to the end of the line, quotes included.
                '#' if current
                    .chars()
                    .last()
                    .is_none_or(char::is_whitespace) =>
                {
                    while characters.get(index).is_some_and(|c| *c != '\n') {
                        index += 1;
                    }
                }
                '&' if characters.get(index) == Some(&'>') => {
                    // `&>` sends both streams to a file.
                    index = scan.consume_redirection(&characters, index + 1);
                }
                ';' | '|' | '&' | '\n' => scan.split(&mut current),
                '>' => {
                    // A file descriptor written right before `>` belongs to the redirection.
                    let digits = current
                        .chars()
                        .rev()
                        .take_while(char::is_ascii_digit)
                        .count();
                    let before = current
                        .chars()
                        .rev()
                        .nth(digits)
                        .is_none_or(char::is_whitespace);
                    if digits > 0 && before {
                        current.truncate(current.len() - digits);
                    }
                    index = scan.consume_redirection(&characters, index);
                }
                _ => current.push(character),
            }
        }
        scan.split(&mut current);
        scan
    }
}

/// One directly executable shell command and its parsed arguments.
pub struct CommandInvocation {
    /// The first word, possibly a path.
    pub executable: String,
    /// The remaining words after shell quoting is removed.
    pub arguments: Vec<String>,
}

impl TryFrom<&str> for CommandInvocation {
    type Error = ();

    /// Parses one shell segment without evaluating expansions or substitutions.
    fn try_from(segment: &str) -> Result<Self, Self::Error> {
        let mut words = shlex::split(segment).ok_or(())?.into_iter();
        let executable = words.next().ok_or(())?;
        Ok(Self {
            executable,
            arguments: words.collect(),
        })
    }
}

impl CommandInvocation {
    /// Returns the executable's file name, so `/bin/rm` and `rm` match alike.
    #[must_use]
    pub fn program(&self) -> &str {
        std::path::Path::new(&self.executable)
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or(&self.executable)
    }

    /// Applies the plan-mode allowlist and each program's forbidden options.
    fn read_only(&self) -> Result<(), ReadOnlyViolation> {
        let program = self.program();
        let refuse = |argument: &str| ReadOnlyViolation::Argument {
            program: program.to_string(),
            argument: OptionName(argument).to_string(),
        };
        match program {
            "git" => return self.git_read_only(),
            "file" => self.refuse_options(&['C'], &["--compile"], refuse)?,
            "rg" => {
                self.refuse_options(&[], &["--pre", "--pre-glob"], refuse)?
            }
            "fd" => self.refuse_options(
                &['x', 'X'],
                &["--exec", "--exec-batch"],
                refuse,
            )?,
            "sort" => self.refuse_options(
                &['o'],
                &["--output", "--compress-program"],
                refuse,
            )?,
            // `-v` assigns a shell variable, such as `PATH`, instead of printing.
            "printf" => self.refuse_options(&['v'], &[], refuse)?,
            "tree" => self.refuse_options(&['o'], &[], refuse)?,
            "find" => {
                if let Some(argument) = self
                    .arguments
                    .iter()
                    .find(|argument| FIND_WRITERS.contains(&argument.as_str()))
                {
                    return Err(refuse(argument));
                }
            }
            "uniq" => {
                // A second operand is the output file uniq writes.
                if self
                    .arguments
                    .iter()
                    .filter(|argument| !argument.starts_with('-'))
                    .count()
                    > 1
                {
                    return Err(refuse("OUTPUT"));
                }
            }
            _ if PLAIN_READERS.contains(&program) => {}
            _ => return Err(ReadOnlyViolation::Program(program.to_string())),
        }
        Ok(())
    }

    /// Refuses short flags (also inside clusters such as `-of`) and long options
    /// (also as `--name=value` and as abbreviations such as `--out`, which getopt
    /// and git accept when unambiguous).
    fn refuse_options(
        &self,
        short: &[char],
        long: &[&str],
        refuse: impl Fn(&str) -> ReadOnlyViolation,
    ) -> Result<(), ReadOnlyViolation> {
        for argument in &self.arguments {
            if let Some(name) = argument.strip_prefix("--") {
                let name = name.split('=').next().unwrap_or(name);
                if !name.is_empty()
                    && long.iter().any(|option| {
                        option.trim_start_matches('-').starts_with(name)
                    })
                {
                    return Err(refuse(argument));
                }
            } else if let Some(cluster) = argument.strip_prefix('-')
                && let Some(flag) =
                    cluster.chars().find(|flag| short.contains(flag))
            {
                return Err(refuse(&format!("-{flag}")));
            }
        }
        Ok(())
    }

    /// Allows only inspecting subcommands, with no global options or writing flags.
    fn git_read_only(&self) -> Result<(), ReadOnlyViolation> {
        let Some((subcommand, arguments)) = self.arguments.split_first() else {
            return Err(ReadOnlyViolation::Program("git".to_string()));
        };
        if subcommand.starts_with('-') {
            return Err(ReadOnlyViolation::GitGlobalOption(
                OptionName(subcommand).to_string(),
            ));
        }
        let program = format!("git {subcommand}");
        if !GIT_READERS.contains(&subcommand.as_str()) {
            return Err(ReadOnlyViolation::Program(program));
        }
        let refuse = |argument: &str| ReadOnlyViolation::Argument {
            program: program.clone(),
            argument: OptionName(argument).to_string(),
        };
        let listed = match subcommand.as_str() {
            "branch" => GIT_BRANCH_LISTING,
            "remote" => &["-v"],
            // `-O` opens matches in a pager program only for grep; elsewhere it reads an order file.
            "grep" => return self.refuse_options(&['O'], GIT_WRITERS, refuse),
            _ => return self.refuse_options(&[], GIT_WRITERS, refuse),
        };
        match arguments
            .iter()
            .find(|argument| !listed.contains(&argument.as_str()))
        {
            Some(argument) => Err(refuse(argument)),
            None => Ok(()),
        }
    }
}

/// Borrows one argument to report only its option name, never its value.
struct OptionName<'a>(&'a str);

impl std::fmt::Display for OptionName<'_> {
    /// Drops `=value` from long options and attached values from short ones.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let argument = self.0;
        if argument.starts_with("--") {
            formatter.write_str(argument.split('=').next().unwrap_or(argument))
        } else if let Some(cluster) = argument.strip_prefix('-')
            && let Some(flag) = cluster.chars().next()
        {
            write!(formatter, "-{flag}")
        } else {
            formatter.write_str(argument)
        }
    }
}

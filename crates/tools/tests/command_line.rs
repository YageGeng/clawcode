//! Plan-mode read-only shell policy shared with command-guard parsing.

use tools::ShellCommandLine;

/// Commands that write files, run other programs, or cannot be parsed are refused.
#[test]
fn read_only_rejects_writes_and_execution() {
    for command in [
        "ls; rm x",
        "cat a > b",
        "echo \"$(rm x)\"",
        "echo `rm x`",
        "diff <(ls) a",
        "find . -delete",
        "find . -exec rm {} ;",
        "env rm x",
        "xargs rm",
        "git branch -D x",
        "git diff --output=x",
        "git -c core.pager=x log",
        "git --no-pager log",
        "git diff --ext-diff",
        "git grep -O x",
        "git push",
        "rg --pre=sh x",
        "rg --pre sh x",
        "fd -x rm",
        "fd --exec-batch rm",
        "sort -o f a",
        "sort -of a",
        "sort --output=f a",
        "tree -o f",
        "uniq a b",
        "file -C",
        "sed -i s/a/b/ f",
        "tee f",
        "cargo test",
        "printenv",
        "ls 'unclosed",
    ] {
        let _violation =
            ShellCommandLine::read_only(command).expect_err(command);
    }
}

/// Inspection commands, pipes, and harmless redirections stay available.
#[test]
fn read_only_allows_inspection() {
    for command in [
        "cd src && ls",
        "git log | head",
        "git status",
        "git branch --show-current",
        "git remote -v",
        "grep \">\" f",
        "ls 2>&1",
        "ls >/dev/null 2>&1 || ls",
        "uniq a",
        "rg TODO src",
        "find . -name '*.rs'",
        "jq . a.json",
        "pwd",
    ] {
        assert_eq!(ShellCommandLine::read_only(command), Ok(()), "{command}");
    }
}

/// Reasons name the rule and the flag, never the command text or argument values.
#[test]
fn violation_names_the_reason_not_the_command() {
    let redirect = ShellCommandLine::read_only("cat secret > out")
        .expect_err("redirect")
        .to_string();
    assert!(!redirect.contains("secret"), "{redirect}");
    assert!(redirect.contains("redirection"), "{redirect}");

    let output = ShellCommandLine::read_only("sort --output=/tmp/secret a")
        .expect_err("output flag")
        .to_string();
    assert!(output.contains("--output"), "{output}");
    assert!(!output.contains("/tmp/secret"), "{output}");

    let pager = ShellCommandLine::read_only("git --no-pager log")
        .expect_err("global option")
        .to_string();
    assert!(pager.contains("--no-pager"), "{pager}");
    assert!(pager.contains("global option"), "{pager}");
}

/// The lenient parse keeps every parsable segment for command-guard.
#[test]
fn lenient_parse_keeps_parsable_segments() {
    let line = ShellCommandLine::from("ls -la | /bin/rm -rf x; echo 'open");
    let programs = line
        .invocations()
        .iter()
        .map(|invocation| invocation.program().to_string())
        .collect::<Vec<_>>();
    assert_eq!(programs, ["ls", "rm"]);
}

/// Abbreviated long options and clustered short flags reach the same refusals.
#[test]
fn read_only_rejects_abbreviations_and_clusters() {
    for command in [
        "git grep --open-files-in-pager=/tmp/x.sh x",
        "git grep --open=/tmp/x.sh x",
        "git grep -nO x",
        "git grep -nOvim x",
        "git log --out=x",
        "git diff --ext",
        "sort --out=f a",
        "sort --o f a",
        "file --comp",
        "rg --pre-g=x y",
    ] {
        let _violation =
            ShellCommandLine::read_only(command).expect_err(command);
    }
}

/// Options that only share a prefix with a refused one still pass.
#[test]
fn read_only_keeps_unrelated_long_options() {
    for command in [
        "git log --oneline",
        "git log -n 5",
        "git diff -O order.txt",
        "rg --pretty x",
        "sort --numeric-sort a",
    ] {
        assert_eq!(ShellCommandLine::read_only(command), Ok(()), "{command}");
    }
}

/// Shell syntax the scanner cannot follow, or that evaluates variable text, refuses.
#[test]
fn read_only_rejects_hidden_syntax() {
    for command in [
        // ANSI-C quoting hides the `'` that would otherwise close a quote.
        "echo $'\\'' > /tmp/pwned #'",
        // A comment's quote must not swallow the next line.
        "echo hi #'\ntouch /tmp/pwned #'",
        // Prompt expansion and arithmetic run substitutions stored in variables.
        "printf -v x '\\x24(touch /tmp/p)'; echo ${x@P}",
        "echo \"${x@P}\"",
        "echo $[x]",
        "printf -v PATH /tmp",
        "sort --compress-program=sh a",
    ] {
        let _violation =
            ShellCommandLine::read_only(command).expect_err(command);
    }
}

/// Comments, end-of-line anchors, and plain variables keep working.
#[test]
fn read_only_allows_comments_and_plain_variables() {
    for command in [
        "ls -la # list files",
        "ls\n# done",
        "grep 'foo$' f",
        "grep '${literal}' f",
        "echo \"$HOME\" $PWD",
        "echo a#b",
    ] {
        assert_eq!(ShellCommandLine::read_only(command), Ok(()), "{command}");
    }
}

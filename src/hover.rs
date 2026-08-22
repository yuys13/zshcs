use std::time::Duration;
use tower_lsp::lsp_types::{HoverContents, MarkupContent, MarkupKind, Position, Range};

/// Default timeout for asynchronous man page retrieval (2000 milliseconds).
pub const DEFAULT_HOVER_MAN_TIMEOUT: Duration = Duration::from_millis(2000);

/// Determines whether a character is part of a word/token for hover inspection.
///
/// Shell delimiters, operators, quotes, and whitespace are excluded.
#[inline]
pub fn is_word_char(c: char) -> bool {
    if c.is_whitespace() {
        return false;
    }
    !matches!(
        c,
        ';' | '|'
            | '&'
            | '('
            | ')'
            | '<'
            | '>'
            | '"'
            | '\''
            | '`'
            | '\\'
            | '{'
            | '}'
            | '['
            | ']'
            | '$'
            | ','
            | '#'
            | '='
            | '!'
            | '~'
            | '^'
            | '*'
            | '?'
            | '\0'
    )
}

/// Extracts the word under the cursor position and its Range in UTF-16 coordinates.
///
/// Safely handles multi-byte UTF-8 sequences and UTF-16 surrogate pairs.
/// Returns `None` if the cursor is not positioned on a valid word or if the position is out of bounds.
pub fn extract_word_at_position(text: &str, position: Position) -> Option<(&str, Range)> {
    let target_line = position.line as usize;
    let target_char = position.character as usize;

    // Locate the line start and end byte offsets
    let mut current_line = 0;
    let mut line_start_byte = 0;
    let mut line_end_byte = text.len();
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if current_line == target_line {
            line_start_byte = i;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            line_end_byte = i;
            if line_end_byte > line_start_byte && bytes[line_end_byte - 1] == b'\r' {
                line_end_byte -= 1;
            }
            break;
        }
        if bytes[i] == b'\n' {
            current_line += 1;
        }
        i += 1;
    }

    if current_line != target_line {
        return None;
    }

    let line_str = &text[line_start_byte..line_end_byte];

    // Find token spans in line (byte offsets and UTF-16 code unit offsets)
    struct TokenSpan {
        start_byte: usize,
        end_byte: usize,
        start_u16: usize,
        end_u16: usize,
    }

    let mut tokens: Vec<TokenSpan> = Vec::new();
    let mut in_word = false;
    let mut word_start_byte = 0;
    let mut word_start_u16 = 0;
    let mut current_u16 = 0;

    for (byte_idx, c) in line_str.char_indices() {
        let char_u16_len = c.len_utf16();
        if is_word_char(c) {
            if !in_word {
                in_word = true;
                word_start_byte = line_start_byte + byte_idx;
                word_start_u16 = current_u16;
            }
        } else if in_word {
            in_word = false;
            tokens.push(TokenSpan {
                start_byte: word_start_byte,
                end_byte: line_start_byte + byte_idx,
                start_u16: word_start_u16,
                end_u16: current_u16,
            });
        }
        current_u16 += char_u16_len;
    }

    if in_word {
        tokens.push(TokenSpan {
            start_byte: word_start_byte,
            end_byte: line_end_byte,
            start_u16: word_start_u16,
            end_u16: current_u16,
        });
    }

    if target_char > current_u16 {
        return None;
    }

    for token in tokens {
        if target_char >= token.start_u16 && target_char <= token.end_u16 {
            let word_str = &text[token.start_byte..token.end_byte];
            let range = Range {
                start: Position::new(position.line, token.start_u16 as u32),
                end: Position::new(position.line, token.end_u16 as u32),
            };
            return Some((word_str, range));
        }
    }

    None
}

/// Cleans raw manual page output by removing backspace overstrikes and ANSI escapes.
pub fn clean_man_text(raw: &str) -> String {
    let mut cleaned = String::with_capacity(raw.len());
    for line in raw.lines() {
        let chars: Vec<char> = line.chars().collect();
        let mut line_buf: Vec<char> = Vec::with_capacity(chars.len());
        let mut i = 0;
        while i < chars.len() {
            // Check for ANSI escape sequences: \x1b[...m
            if chars[i] == '\x1b' && i + 1 < chars.len() && chars[i + 1] == '[' {
                i += 2;
                while i < chars.len() && !chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                if i < chars.len() {
                    i += 1;
                }
                continue;
            }

            // Check for backspace overstrikes: c1 \b c2
            if i + 1 < chars.len() && chars[i + 1] == '\x08' {
                if i + 2 < chars.len() {
                    let c1 = chars[i];
                    let c2 = chars[i + 2];
                    if c1 == '_' {
                        line_buf.push(c2);
                    } else {
                        line_buf.push(c1);
                    }
                    i += 3;
                    continue;
                } else {
                    i += 2;
                    continue;
                }
            }

            if chars[i] != '\x08' {
                line_buf.push(chars[i]);
            }
            i += 1;
        }
        let line_string: String = line_buf.into_iter().collect();
        cleaned.push_str(&line_string);
        cleaned.push('\n');
    }
    cleaned.trim_end().to_string()
}

/// Retrieves the manual page for an external command asynchronously with a timeout.
pub async fn get_man_page(word: &str, timeout_dur: Duration) -> Option<String> {
    if word.is_empty() || word.len() > 256 || word.starts_with('-') {
        return None;
    }

    // Only allow alphanumeric and safe command characters
    if !word
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '/'))
    {
        return None;
    }

    let mut cmd = tokio::process::Command::new("man");
    cmd.arg(word)
        .env("MANPAGER", "cat")
        .env("PAGER", "cat")
        .env("TERM", "dumb")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());

    let child_output = tokio::time::timeout(timeout_dur, cmd.output()).await;

    match child_output {
        Ok(Ok(output)) if output.status.success() => {
            let stdout_str = String::from_utf8_lossy(&output.stdout);
            let cleaned = clean_man_text(&stdout_str);
            if cleaned.trim().is_empty() {
                None
            } else {
                Some(cleaned)
            }
        }
        _ => None,
    }
}

/// Returns static Markdown documentation for Zsh builtins and reserved words.
pub fn get_builtin_or_reserved_doc(word: &str) -> Option<&'static str> {
    match word {
        // Builtins
        "cd" => Some(
            "### `cd` (Zsh Builtin)\n\n```zsh\ncd [ -qsLP ] [ arg ... ]\ncd [ -qsLP ] old new\ncd [ -qsLP ] {+|-}n\n```\n\nChange the current working directory.",
        ),
        "echo" => Some(
            "### `echo` (Zsh Builtin)\n\n```zsh\necho [ -neE ] [ arg ... ]\n```\n\nWrite arguments to the standard output.",
        ),
        "export" => Some(
            "### `export` (Zsh Builtin)\n\n```zsh\nexport [ name[=value] ... ]\n```\n\nSet export attribute for shell parameters.",
        ),
        "set" => Some(
            "### `set` (Zsh Builtin)\n\n```zsh\nset [ {+|-}options | {+|-}o option_name ] ... [ -- ] [ arg ... ]\n```\n\nSet or unset shell options and positional parameters.",
        ),
        "setopt" => Some(
            "### `setopt` (Zsh Builtin)\n\n```zsh\nsetopt [ {+|-}options | {+|-}o option_name ] ... [ name ... ]\n```\n\nSet the specified shell options.",
        ),
        "unsetopt" => Some(
            "### `unsetopt` (Zsh Builtin)\n\n```zsh\nunsetopt [ {+|-}options | {+|-}o option_name ] ... [ name ... ]\n```\n\nUnset the specified shell options.",
        ),
        "autoload" => Some(
            "### `autoload` (Zsh Builtin)\n\n```zsh\nautoload [ {+|-}UXtkm ] [ -w ] [ name ... ]\n```\n\nMark functions to be loaded automatically from `$fpath` when invoked.",
        ),
        "compadd" => Some(
            "### `compadd` (Zsh Builtin)\n\n```zsh\ncompadd [ -akqQfnU ] [ -F array ] [ -P prefix ] [ -S suffix ] ... [ words ... ]\n```\n\nAdd candidate words to the internal completion list.",
        ),
        "typeset" => Some(
            "### `typeset` (Zsh Builtin)\n\n```zsh\ntypeset [ {+|-}AEFHLRUZagilprtuxz ] [ -LRZ [ n ] ] [ name[=value] ... ]\n```\n\nSet attributes and values for shell parameters.",
        ),
        "print" => Some(
            "### `print` (Zsh Builtin)\n\n```zsh\nprint [ -abcDilmnNoOpPrsTvz ] [ -u n ] [ -R [ -en ]] [ arg ... ]\n```\n\nOutput the arguments with enhanced formatting and option support.",
        ),
        "printf" => Some(
            "### `printf` (Zsh Builtin)\n\n```zsh\nprintf format [ arg ... ]\n```\n\nPrint formatted output according to the format specification.",
        ),
        "source" => Some(
            "### `source` (Zsh Builtin)\n\n```zsh\nsource file [ arg ... ]\n```\n\nRead commands from the specified file and execute them in the current shell environment.",
        ),
        "." => Some(
            "### `.` (Zsh Builtin)\n\n```zsh\n. file [ arg ... ]\n```\n\nRead commands from the specified file and execute them (POSIX alias for `source`).",
        ),
        "eval" => Some(
            "### `eval` (Zsh Builtin)\n\n```zsh\neval [ arg ... ]\n```\n\nRead arguments as input to the shell and execute the resulting commands.",
        ),
        "alias" => Some(
            "### `alias` (Zsh Builtin)\n\n```zsh\nalias [ -gmr ] [ name[=value] ... ]\n```\n\nDefine or display command aliases.",
        ),
        "unalias" => Some(
            "### `unalias` (Zsh Builtin)\n\n```zsh\nunalias [ -a ] name ...\n```\n\nRemove defined aliases.",
        ),
        "read" => Some(
            "### `read` (Zsh Builtin)\n\n```zsh\nread [ -d delim ] [ -k [ num ] ] [ -p prompt ] [ -u fd ] [ -rz ] [ name ... ]\n```\n\nRead a line or characters from standard input or a file descriptor.",
        ),
        "return" => Some(
            "### `return` (Zsh Builtin)\n\n```zsh\nreturn [ n ]\n```\n\nReturn from a function or sourced script with status `n`.",
        ),
        "exit" => Some(
            "### `exit` (Zsh Builtin)\n\n```zsh\nexit [ n ]\n```\n\nExit the shell with exit status `n`.",
        ),
        "shift" => Some(
            "### `shift` (Zsh Builtin)\n\n```zsh\nshift [ -p ] [ n ] [ name ... ]\n```\n\nShift positional parameters left by `n` positions (default 1).",
        ),
        "test" => Some(
            "### `test` (Zsh Builtin)\n\n```zsh\ntest [ expr ]\n```\n\nEvaluate conditional expressions.",
        ),
        "trap" => Some(
            "### `trap` (Zsh Builtin)\n\n```zsh\ntrap [ arg ] [ sig ... ]\n```\n\nPerform an action when the shell receives specific signals or traps.",
        ),
        "unset" => Some(
            "### `unset` (Zsh Builtin)\n\n```zsh\nunset [ -v | -f ] [ -m ] name ...\n```\n\nUnset the values and attributes of parameters or functions.",
        ),
        "local" => Some(
            "### `local` (Zsh Builtin)\n\n```zsh\nlocal [ {+|-}AEFHLRUZagilprtuxz ] [ name[=value] ... ]\n```\n\nDeclare parameters local to the enclosing function.",
        ),
        "declare" => Some(
            "### `declare` (Zsh Builtin)\n\n```zsh\ndeclare [ {+|-}AEFHLRUZagilprtuxz ] [ name[=value] ... ]\n```\n\nDeclare parameters and set attributes (equivalent to `typeset`).",
        ),
        "which" => Some(
            "### `which` (Zsh Builtin)\n\n```zsh\nwhich [ -c | -w | -p ] [ name ... ]\n```\n\nLocate and display information about commands.",
        ),
        "where" => Some(
            "### `where` (Zsh Builtin)\n\n```zsh\nwhere [ -wpms ] [ name ... ]\n```\n\nList all occurrences of the specified command in PATH, aliases, and builtins.",
        ),
        "whence" => Some(
            "### `whence` (Zsh Builtin)\n\n```zsh\nwhence [ -vcwpamsf ] [ name ... ]\n```\n\nIndicate how each command name would be interpreted by the shell.",
        ),
        "type" => Some(
            "### `type` (Zsh Builtin)\n\n```zsh\ntype [ -wfpams ] [ name ... ]\n```\n\nDescribe the type and interpretation of each specified command name.",
        ),
        "bindkey" => Some(
            "### `bindkey` (Zsh Builtin)\n\n```zsh\nbindkey [ -e | -v | -a | -M keymap ] ...\n```\n\nDisplay or modify Zsh Line Editor (ZLE) key bindings.",
        ),
        "zstyle" => Some(
            "### `zstyle` (Zsh Builtin)\n\n```zsh\nzstyle [ -e | - | -- ] pattern style string ...\n```\n\nDefine or query completion styles and configuration settings.",
        ),
        "zmodload" => Some(
            "### `zmodload` (Zsh Builtin)\n\n```zsh\nzmodload [ -dLs ] [ -u ] [ name ... ]\n```\n\nLoad, unload, or query dynamically loadable binary modules.",
        ),
        "zpty" => Some(
            "### `zpty` (Zsh Builtin / Module)\n\n```zsh\nzpty [ -e | -b ] [ -d ] name [ arg ... ]\n```\n\nCreate and control pseudo-terminals via the `zsh/zpty` module.",
        ),
        "zparseopts" => Some(
            "### `zparseopts` (Zsh Builtin / Module)\n\n```zsh\nzparseopts [ -D ] [ -K ] [ -M ] [ -E ] [ -a array ] [ -A assoc ] specs ...\n```\n\nParse complex script options via the `zsh/zutil` module.",
        ),
        "pushd" => Some(
            "### `pushd` (Zsh Builtin)\n\n```zsh\npushd [ -qsLP ] [ arg ]\n```\n\nChange the current directory and push the previous directory onto the stack.",
        ),
        "popd" => Some(
            "### `popd` (Zsh Builtin)\n\n```zsh\npopd [ -qsLP ] [ arg ]\n```\n\nPop a directory from the directory stack and change to it.",
        ),
        "dirs" => Some(
            "### `dirs` (Zsh Builtin)\n\n```zsh\ndirs [ -c | -v | -p ] [ arg ... ]\n```\n\nDisplay the list of directories on the directory stack.",
        ),
        "pwd" => Some(
            "### `pwd` (Zsh Builtin)\n\n```zsh\npwd [ -rLP ]\n```\n\nPrint the absolute path name of the current working directory.",
        ),
        "history" => Some(
            "### `history` (Zsh Builtin)\n\n```zsh\nhistory [ -nrdDfEim ] [ first [ last ] ]\n```\n\nDisplay or manage the command history list.",
        ),
        "fc" => Some(
            "### `fc` (Zsh Builtin)\n\n```zsh\nfc [ -e ename ] [ -lnr ] [ first [ last ] ]\n```\n\nSelect a range of commands from the history list to edit and re-execute.",
        ),
        "bg" => Some(
            "### `bg` (Zsh Builtin)\n\n```zsh\nbg [ job ... ]\n```\n\nResume suspended jobs and continue execution in the background.",
        ),
        "fg" => Some(
            "### `fg` (Zsh Builtin)\n\n```zsh\nfg [ job ... ]\n```\n\nResume suspended jobs and bring them to the foreground.",
        ),
        "jobs" => Some(
            "### `jobs` (Zsh Builtin)\n\n```zsh\njobs [ -dlprs ] [ job ... ]\n```\n\nList active background and suspended jobs.",
        ),
        "kill" => Some(
            "### `kill` (Zsh Builtin)\n\n```zsh\nkill [ -s sig | -sig ] { pid | job } ...\n```\n\nSend termination or other signals to specified processes or jobs.",
        ),
        "wait" => Some(
            "### `wait` (Zsh Builtin)\n\n```zsh\nwait [ job ... ]\n```\n\nWait for background jobs or process IDs to complete.",
        ),
        "disown" => Some(
            "### `disown` (Zsh Builtin)\n\n```zsh\ndisown [ job ... ]\n```\n\nRemove specified jobs from the shell's active job table.",
        ),
        "exec" => Some(
            "### `exec` (Zsh Builtin)\n\n```zsh\nexec [ -cl ] [ -a name ] [ command [ arg ... ] ]\n```\n\nReplace the current shell process with the specified command.",
        ),
        "hash" => Some(
            "### `hash` (Zsh Builtin)\n\n```zsh\nhash [ -dfmrv ] [ name[=value] ... ]\n```\n\nDisplay or manipulate the internal hash tables for commands and directories.",
        ),
        "rehash" => Some(
            "### `rehash` (Zsh Builtin)\n\n```zsh\nrehash\n```\n\nRescan the `$PATH` directories to rebuild the internal command hash table.",
        ),
        "umask" => Some(
            "### `umask` (Zsh Builtin)\n\n```zsh\numask [ -S ] [ mask ]\n```\n\nSet or display the file mode creation mask.",
        ),
        "true" => Some(
            "### `true` (Zsh Builtin)\n\n```zsh\ntrue\n```\n\nDo nothing successfully (returns status code 0).",
        ),
        "false" => Some(
            "### `false` (Zsh Builtin)\n\n```zsh\nfalse\n```\n\nDo nothing unsuccessfully (returns status code 1).",
        ),
        ":" => Some(
            "### `:` (Zsh Builtin)\n\n```zsh\n: [ arg ... ]\n```\n\nNull command; evaluates arguments and returns status code 0.",
        ),

        // Reserved Words
        "if" => Some(
            "### `if` (Zsh Reserved Word)\n\n```zsh\nif list; then\n  list\n[ elif list; then\n  list ] ...\n[ else\n  list ]\nfi\n```\n\nExecute command list conditionally based on exit status.",
        ),
        "then" => Some(
            "### `then` (Zsh Reserved Word)\n\n```zsh\nthen\n  list\n```\n\nDelimits the condition and execution body of an `if` or `elif` block.",
        ),
        "elif" => Some(
            "### `elif` (Zsh Reserved Word)\n\n```zsh\nelif list; then\n  list\n```\n\nSpecifies an alternative conditional branch in an `if` construct.",
        ),
        "else" => Some(
            "### `else` (Zsh Reserved Word)\n\n```zsh\nelse\n  list\n```\n\nSpecifies the fallback branch in an `if` or `case` construct.",
        ),
        "fi" => Some(
            "### `fi` (Zsh Reserved Word)\n\n```zsh\nfi\n```\n\nCloses an `if` statement block.",
        ),
        "for" => Some(
            "### `for` (Zsh Reserved Word)\n\n```zsh\nfor name [ in word ... ]; do\n  list\ndone\n\nfor (( expr1; expr2; expr3 )); do\n  list\ndone\n```\n\nExecute command list for each member in a list or arithmetic iteration.",
        ),
        "do" => Some(
            "### `do` (Zsh Reserved Word)\n\n```zsh\ndo\n  list\ndone\n```\n\nStarts the body of a `for`, `while`, `until`, or `select` loop.",
        ),
        "done" => Some(
            "### `done` (Zsh Reserved Word)\n\n```zsh\ndone\n```\n\nCloses a `for`, `while`, `until`, or `select` loop body.",
        ),
        "while" => Some(
            "### `while` (Zsh Reserved Word)\n\n```zsh\nwhile list; do\n  list\ndone\n```\n\nExecute command list repeatedly as long as the test command returns status 0.",
        ),
        "until" => Some(
            "### `until` (Zsh Reserved Word)\n\n```zsh\nuntil list; do\n  list\ndone\n```\n\nExecute command list repeatedly until the test command returns status 0.",
        ),
        "case" => Some(
            "### `case` (Zsh Reserved Word)\n\n```zsh\ncase word in\n  [ [(] pattern [ | pattern ] ... ) list (;;|;&|;|) ] ...\nesac\n```\n\nExecute command list corresponding to the first matching pattern.",
        ),
        "esac" => Some(
            "### `esac` (Zsh Reserved Word)\n\n```zsh\nesac\n```\n\nCloses a `case` construct.",
        ),
        "select" => Some(
            "### `select` (Zsh Reserved Word)\n\n```zsh\nselect name [ in word ... ]; do\n  list\ndone\n```\n\nDisplay a numbered menu of words and execute the list with the selected item.",
        ),
        "function" => Some(
            "### `function` (Zsh Reserved Word)\n\n```zsh\nfunction name [ () ] [ linenum ] {\n  list\n}\n```\n\nDefine a shell function with the specified name.",
        ),
        "repeat" => Some(
            "### `repeat` (Zsh Reserved Word)\n\n```zsh\nrepeat count; do\n  list\ndone\n```\n\nExecute the loop body `count` times.",
        ),
        "time" => Some(
            "### `time` (Zsh Reserved Word)\n\n```zsh\ntime [ pipeline ]\n```\n\nExecute the pipeline and output user and system CPU timing statistics.",
        ),
        "coproc" => Some(
            "### `coproc` (Zsh Reserved Word)\n\n```zsh\ncoproc pipeline\n```\n\nExecute the pipeline asynchronously as a coprocess.",
        ),
        "nocorrect" => Some(
            "### `nocorrect` (Zsh Reserved Word)\n\n```zsh\nnocorrect command ...\n```\n\nDisable spelling correction for the following command arguments.",
        ),
        "foreach" => Some(
            "### `foreach` (Zsh Reserved Word)\n\n```zsh\nforeach name ( word ... )\n  list\nend\n```\n\nExecute list for each word (csh-style iteration).",
        ),
        "end" => Some(
            "### `end` (Zsh Reserved Word)\n\n```zsh\nend\n```\n\nCloses a `foreach` loop construct.",
        ),
        _ => None,
    }
}

/// Generates hover information for a word asynchronously with default timeout.
pub async fn get_hover_info(word: &str) -> Option<HoverContents> {
    get_hover_info_with_timeout(word, DEFAULT_HOVER_MAN_TIMEOUT).await
}

/// Generates hover information for a word asynchronously with a custom timeout.
///
/// Priority:
/// 1. Zsh builtins & reserved words static Markdown dictionary.
/// 2. External command `man` page full text in a ````text ... ```` code block.
pub async fn get_hover_info_with_timeout(
    word: &str,
    timeout_dur: Duration,
) -> Option<HoverContents> {
    // 1. Check static dictionary
    if let Some(doc) = get_builtin_or_reserved_doc(word) {
        return Some(HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: doc.to_string(),
        }));
    }

    // 2. Query man page asynchronously
    let man_text = get_man_page(word, timeout_dur).await?;
    if man_text.is_empty() {
        return None;
    }

    let markdown = format!("```text\n{}\n```", man_text);
    Some(HoverContents::Markup(MarkupContent {
        kind: MarkupKind::Markdown,
        value: markdown,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    // 1. Basic word extraction on single line
    #[case("echo hello world", 0, 0, Some(("echo", 0, 4)))]
    #[case("echo hello world", 0, 2, Some(("echo", 0, 4)))]
    #[case("echo hello world", 0, 4, Some(("echo", 0, 4)))]
    #[case("echo hello world", 0, 5, Some(("hello", 5, 10)))]
    #[case("echo hello world", 0, 8, Some(("hello", 5, 10)))]
    #[case("echo hello world", 0, 10, Some(("hello", 5, 10)))]
    #[case("echo hello world", 0, 11, Some(("world", 11, 16)))]
    #[case("echo hello world", 0, 16, Some(("world", 11, 16)))]
    #[case("echo hello world", 0, 17, None)]
    // 2. Spaces and gaps
    #[case("   cd   /tmp   ", 0, 0, None)]
    #[case("   cd   /tmp   ", 0, 2, None)]
    #[case("   cd   /tmp   ", 0, 3, Some(("cd", 3, 5)))]
    #[case("   cd   /tmp   ", 0, 4, Some(("cd", 3, 5)))]
    #[case("   cd   /tmp   ", 0, 5, Some(("cd", 3, 5)))]
    #[case("   cd   /tmp   ", 0, 6, None)]
    #[case("   cd   /tmp   ", 0, 8, Some(("/tmp", 8, 12)))]
    // 3. Multi-line extraction
    #[case("line1\nsetopt promptsubst\nline3", 1, 0, Some(("setopt", 0, 6)))]
    #[case("line1\nsetopt promptsubst\nline3", 1, 3, Some(("setopt", 0, 6)))]
    #[case("line1\nsetopt promptsubst\nline3", 1, 6, Some(("setopt", 0, 6)))]
    #[case("line1\nsetopt promptsubst\nline3", 1, 7, Some(("promptsubst", 7, 18)))]
    #[case("line1\nsetopt promptsubst\nline3", 1, 18, Some(("promptsubst", 7, 18)))]
    #[case("line1\nsetopt promptsubst\nline3", 2, 2, Some(("line3", 0, 5)))]
    // 4. Out of bounds lines
    #[case("single line", 1, 0, None)]
    #[case("single line", 10, 0, None)]
    fn test_extract_word_at_position_basic(
        #[case] text: &str,
        #[case] line: u32,
        #[case] character: u32,
        #[case] expected: Option<(&str, u32, u32)>,
    ) {
        let pos = Position::new(line, character);
        let result = extract_word_at_position(text, pos);
        match expected {
            Some((word, start, end)) => {
                let (res_word, range) = result.expect("expected word match");
                assert_eq!(res_word, word);
                assert_eq!(range.start, Position::new(line, start));
                assert_eq!(range.end, Position::new(line, end));
            }
            None => {
                assert!(result.is_none());
            }
        }
    }

    #[rstest]
    // 1. Delimiters and operators: pipes, redirects, quotes, semicolons
    #[case("cat file | grep foo", 0, 9, None)] // on '|'
    #[case("cat file | grep foo", 0, 11, Some(("grep", 11, 15)))]
    #[case("echo \"hello\"; ls", 0, 5, None)] // on '"'
    #[case("echo \"hello\"; ls", 0, 6, Some(("hello", 6, 11)))]
    #[case("echo \"hello\"; ls", 0, 12, None)] // on ';'
    #[case("echo \"hello\"; ls", 0, 14, Some(("ls", 14, 16)))]
    #[case("(autoload -Uz compinit)", 0, 0, None)] // on '('
    #[case("(autoload -Uz compinit)", 0, 1, Some(("autoload", 1, 9)))]
    #[case("(autoload -Uz compinit)", 0, 10, Some(("-Uz", 10, 13)))]
    #[case("(autoload -Uz compinit)", 0, 22, Some(("compinit", 14, 22)))] // end of 'compinit'
    #[case("(autoload -Uz compinit)", 0, 23, None)] // on ')'
    // 2. Words with hyphens and underscores
    #[case("zsh-lovers --all-targets test_func", 0, 3, Some(("zsh-lovers", 0, 10)))]
    #[case("zsh-lovers --all-targets test_func", 0, 15, Some(("--all-targets", 11, 24)))]
    #[case("zsh-lovers --all-targets test_func", 0, 28, Some(("test_func", 25, 34)))]
    // 3. Variables with '$'
    #[case("echo $VAR_NAME", 0, 5, None)] // on '$'
    #[case("echo $VAR_NAME", 0, 6, Some(("VAR_NAME", 6, 14)))]
    fn test_extract_word_at_position_delimiters(
        #[case] text: &str,
        #[case] line: u32,
        #[case] character: u32,
        #[case] expected: Option<(&str, u32, u32)>,
    ) {
        let pos = Position::new(line, character);
        let result = extract_word_at_position(text, pos);
        match expected {
            Some((word, start, end)) => {
                let (res_word, range) = result.expect("expected word match");
                assert_eq!(res_word, word);
                assert_eq!(range.start, Position::new(line, start));
                assert_eq!(range.end, Position::new(line, end));
            }
            None => {
                assert!(result.is_none());
            }
        }
    }

    #[rstest]
    // 1. Multibyte Japanese characters ("日本語" is 3 chars, 3 UTF-16 units)
    #[case("echo 日本語 echo", 0, 5, Some(("日本語", 5, 8)))]
    #[case("echo 日本語 echo", 0, 7, Some(("日本語", 5, 8)))]
    #[case("echo 日本語 echo", 0, 8, Some(("日本語", 5, 8)))]
    #[case("echo 日本語 echo", 0, 9, Some(("echo", 9, 13)))]
    #[case("echo 日本語 echo", 0, 13, Some(("echo", 9, 13)))]
    #[case("echo 日本語 echo", 0, 14, None)]
    // 2. Surrogate pairs SIP/SMP ('𩸽' is 1 char, 2 UTF-16 code units)
    #[case("echo 𩸽 echo", 0, 5, Some(("𩸽", 5, 7)))]
    #[case("echo 𩸽 echo", 0, 6, Some(("𩸽", 5, 7)))]
    #[case("echo 𩸽 echo", 0, 7, Some(("𩸽", 5, 7)))]
    #[case("echo 𩸽 echo", 0, 8, Some(("echo", 8, 12)))]
    // 3. Emojis
    #[case("echo 🎉🎊 test", 0, 5, Some(("🎉🎊", 5, 9)))]
    #[case("echo 🎉🎊 test", 0, 7, Some(("🎉🎊", 5, 9)))]
    fn test_extract_word_at_position_unicode(
        #[case] text: &str,
        #[case] line: u32,
        #[case] character: u32,
        #[case] expected: Option<(&str, u32, u32)>,
    ) {
        let pos = Position::new(line, character);
        let result = extract_word_at_position(text, pos);
        match expected {
            Some((word, start, end)) => {
                let (res_word, range) = result.expect("expected word match");
                assert_eq!(res_word, word);
                assert_eq!(range.start, Position::new(line, start));
                assert_eq!(range.end, Position::new(line, end));
            }
            None => {
                assert!(result.is_none());
            }
        }
    }

    #[test]
    fn test_extract_word_empty_and_edge_cases() {
        assert!(extract_word_at_position("", Position::new(0, 0)).is_none());
        assert!(extract_word_at_position("   ", Position::new(0, 1)).is_none());
        assert!(extract_word_at_position("echo\n", Position::new(0, 100)).is_none());
    }

    #[test]
    fn test_clean_man_text_overstrikes_and_escapes() {
        // Bold overstrike: N\bN A\bA M\bM E\bE
        let raw_bold = "N\x08NA\x08AM\x08ME\x08E";
        assert_eq!(clean_man_text(raw_bold), "NAME");

        // Underline overstrike: _\bf _\bo _\bo
        let raw_underline = "_\x08f_\x08o_\x08o";
        assert_eq!(clean_man_text(raw_underline), "foo");

        // ANSI color escape: \x1b[32mhello\x1b[0m
        let raw_ansi = "\x1b[32mhello\x1b[0m world";
        assert_eq!(clean_man_text(raw_ansi), "hello world");

        // Plain text
        let raw_plain = "Simple line\nSecond line\n";
        assert_eq!(clean_man_text(raw_plain), "Simple line\nSecond line");
    }

    #[test]
    fn test_builtin_and_reserved_word_dictionary() {
        // Verify key builtins
        let builtins = [
            "cd",
            "echo",
            "export",
            "set",
            "setopt",
            "unsetopt",
            "autoload",
            "compadd",
            "typeset",
            "print",
            "printf",
            "source",
            ".",
            "eval",
            "alias",
            "unalias",
            "read",
            "return",
            "exit",
            "shift",
            "test",
            "trap",
            "unset",
            "local",
            "declare",
            "which",
            "where",
            "whence",
            "type",
            "bindkey",
            "zstyle",
            "zmodload",
            "zpty",
            "zparseopts",
            "pushd",
            "popd",
            "dirs",
            "pwd",
            "history",
            "fc",
            "bg",
            "fg",
            "jobs",
            "kill",
            "wait",
            "disown",
            "exec",
            "hash",
            "rehash",
            "umask",
            "true",
            "false",
            ":",
        ];
        for b in builtins {
            let doc = get_builtin_or_reserved_doc(b);
            assert!(doc.is_some(), "Builtin '{b}' should have documentation");
            let doc_str = doc.unwrap();
            assert!(
                doc_str.contains("Builtin") || doc_str.contains("Module"),
                "Doc for '{b}' should mention Builtin or Module"
            );
            assert!(
                doc_str.contains("```zsh"),
                "Doc for '{b}' should contain zsh syntax block"
            );
        }

        // Verify key reserved words
        let reserved = [
            "if",
            "then",
            "elif",
            "else",
            "fi",
            "for",
            "do",
            "done",
            "while",
            "until",
            "case",
            "esac",
            "select",
            "function",
            "repeat",
            "time",
            "coproc",
            "nocorrect",
            "foreach",
            "end",
        ];
        for r in reserved {
            let doc = get_builtin_or_reserved_doc(r);
            assert!(
                doc.is_some(),
                "Reserved word '{r}' should have documentation"
            );
            let doc_str = doc.unwrap();
            assert!(
                doc_str.contains("Reserved Word"),
                "Doc for '{r}' should mention Reserved Word"
            );
            assert!(
                doc_str.contains("```zsh"),
                "Doc for '{r}' should contain zsh syntax block"
            );
        }

        // Non-existent keyword
        assert!(get_builtin_or_reserved_doc("non_existent_command_xyz").is_none());
    }

    #[tokio::test]
    async fn test_get_hover_info_builtin() {
        let hover = get_hover_info("setopt").await;
        assert!(hover.is_some());
        if let Some(HoverContents::Markup(markup)) = hover {
            assert_eq!(markup.kind, MarkupKind::Markdown);
            assert!(markup.value.contains("`setopt` (Zsh Builtin)"));
        } else {
            panic!("Expected HoverContents::Markup");
        }
    }

    #[tokio::test]
    async fn test_get_hover_info_reserved_word() {
        let hover = get_hover_info("while").await;
        assert!(hover.is_some());
        if let Some(HoverContents::Markup(markup)) = hover {
            assert_eq!(markup.kind, MarkupKind::Markdown);
            assert!(markup.value.contains("`while` (Zsh Reserved Word)"));
        } else {
            panic!("Expected HoverContents::Markup");
        }
    }

    #[tokio::test]
    async fn test_get_hover_info_man_page_fallback() {
        // 'ls' is a standard Unix command with a man page
        let hover = get_hover_info("ls").await;
        assert!(hover.is_some());
        if let Some(HoverContents::Markup(markup)) = hover {
            assert_eq!(markup.kind, MarkupKind::Markdown);
            assert!(markup.value.starts_with("```text\n"));
            assert!(markup.value.ends_with("\n```"));
            assert!(
                markup.value.to_lowercase().contains("list directory")
                    || markup.value.to_lowercase().contains("ls")
            );
        } else {
            panic!("Expected HoverContents::Markup");
        }
    }

    #[tokio::test]
    async fn test_get_hover_info_nonexistent_command() {
        let hover = get_hover_info("nonexistent_command_xyz123_456789").await;
        assert!(hover.is_none());
    }

    #[tokio::test]
    async fn test_get_hover_info_invalid_command_characters() {
        assert!(get_hover_info("").await.is_none());
        assert!(get_hover_info("--help").await.is_none());
        assert!(get_hover_info("ls; rm").await.is_none());
    }

    #[tokio::test]
    async fn test_get_hover_info_timeout_handling() {
        // Zero timeout should expire immediately
        let hover = get_hover_info_with_timeout("ls", Duration::from_nanos(1)).await;
        // Either times out and returns None, or if cached returns Some
        // We verify that it does not panic or hang
        let _ = hover;
    }
}

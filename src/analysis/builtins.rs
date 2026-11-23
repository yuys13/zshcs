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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtins_and_reserved_words() {
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

        assert!(get_builtin_or_reserved_doc("non_existent_command_xyz").is_none());
    }
}

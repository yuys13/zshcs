#!/usr/bin/env zsh
# ==============================================================================
# tests/zsh/run_tests.zsh
#
# Unit test harness for zshcs Zsh helper scripts (bin/capture.zsh & bin/zptyrc.zsh).
# Validates syntax, zmodload modules, cache creation, hook interceptors, and
# interactive zpty completion capture.
# ==============================================================================

set -euo pipefail

SCRIPT_DIR=${0:a:h}
REPO_ROOT=${SCRIPT_DIR:h:h}
BIN_DIR="$REPO_ROOT/bin"
CAPTURE_SCRIPT="$BIN_DIR/capture.zsh"
ZPTYRC_SCRIPT="$BIN_DIR/zptyrc.zsh"

TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

TEST_TMPDIR=$(mktemp -d "${TMPDIR:-/tmp}/zshcs_test.XXXXXX")
cleanup() {
    rm -rf "$TEST_TMPDIR"
}
trap cleanup EXIT INT TERM

log_info() {
    print -P "%F{blue}[INFO]%f $*"
}

log_pass() {
    TESTS_PASSED=$((TESTS_PASSED + 1))
    print -P "%F{green}[PASS]%f $*"
}

log_fail() {
    TESTS_FAILED=$((TESTS_FAILED + 1))
    print -P "%F{red}[FAIL]%f $*" >&2
}

run_test_case() {
    local test_name=$1
    local test_fn=$2
    TESTS_RUN=$((TESTS_RUN + 1))
    log_info "Running test: $test_name..."
    if ($test_fn); then
        log_pass "$test_name"
    else
        log_fail "$test_name"
    fi
}

# ------------------------------------------------------------------------------
# Test 1: Syntax Validation (zsh -n)
# ------------------------------------------------------------------------------
test_syntax_check() {
    [[ -f "$CAPTURE_SCRIPT" ]] || { print -u2 "capture.zsh not found at $CAPTURE_SCRIPT"; return 1; }
    [[ -f "$ZPTYRC_SCRIPT" ]] || { print -u2 "zptyrc.zsh not found at $ZPTYRC_SCRIPT"; return 1; }

    zsh -n "$CAPTURE_SCRIPT" || { print -u2 "Syntax error in $CAPTURE_SCRIPT"; return 1; }
    zsh -n "$ZPTYRC_SCRIPT" || { print -u2 "Syntax error in $ZPTYRC_SCRIPT"; return 1; }
    return 0
}

# ------------------------------------------------------------------------------
# Test 2: Module Availability (zsh/zpty and zsh/zutil)
# ------------------------------------------------------------------------------
test_module_loading() {
    zsh --no-rcs -c '
        zmodload zsh/zpty || { print -u2 "Failed to load zsh/zpty"; exit 1; }
        zmodload zsh/zutil || { print -u2 "Failed to load zsh/zutil"; exit 1; }
    '
}

# ------------------------------------------------------------------------------
# Test 3: Cache Directory Creation and Isolation
# ------------------------------------------------------------------------------
test_cache_dir_creation() {
    local custom_cache="$TEST_TMPDIR/custom_cache/sub"
    [[ ! -d "$custom_cache" ]] || { print -u2 "Cache directory should not exist yet"; return 1; }

    ZSHCS_CACHE_DIR="$custom_cache" zsh --no-rcs -c '
        source "'"$ZPTYRC_SCRIPT"'"
        [[ -d "'"$custom_cache"'" ]] || { print -u2 "Cache directory was not created"; exit 1; }
        [[ -f "'"$custom_cache"'/compdump" ]] || { print -u2 "Compdump file was not created in custom cache"; exit 1; }
    '
}

test_cache_dir_xdg_fallback() {
    local xdg_cache="$TEST_TMPDIR/xdg_home"
    local expected_cache="$xdg_cache/zshcs/zsh"

    env -u ZSHCS_CACHE_DIR XDG_CACHE_HOME="$xdg_cache" zsh --no-rcs -c '
        source "'"$ZPTYRC_SCRIPT"'"
        [[ -d "'"$expected_cache"'" ]] || { print -u2 "XDG cache dir was not created: '"$expected_cache"'"; exit 1; }
        [[ -f "'"$expected_cache"'/compdump" ]] || { print -u2 "Compdump was not created in XDG cache: '"$expected_cache"'"; exit 1; }
    '
}

# ------------------------------------------------------------------------------
# Test 4: Directory Synchronization (_zshcs_chdir)
# ------------------------------------------------------------------------------
test_chdir_sync_helper() {
    local target_dir="$TEST_TMPDIR/target_sync_dir"
    mkdir -p "$target_dir"

    local output
    output=$(ZSHCS_CACHE_DIR="$TEST_TMPDIR/cache" zsh --no-rcs -c '
        source "'"$ZPTYRC_SCRIPT"'"
        _zshcs_chdir "'"$target_dir"'"
        [[ "$PWD" -ef "'"$target_dir"'" ]] || exit 2
    ')
    local ret=$?
    [[ $ret -eq 0 ]] || { print -u2 "_zshcs_chdir failed with exit code $ret"; return 1; }
    [[ "$output" == *$'\0__cd_done__\0'* ]] || { print -u2 "_zshcs_chdir did not emit cd_done delimiter"; return 1; }
}

test_chdir_non_existent_directory() {
    local non_existent="$TEST_TMPDIR/non_existent_directory_test"
    local output
    output=$(ZSHCS_CACHE_DIR="$TEST_TMPDIR/cache" zsh --no-rcs -c '
        source "'"$ZPTYRC_SCRIPT"'"
        _zshcs_chdir "'"$non_existent"'"
        exit 0
    ')
    [[ "$output" == *$'\0__cd_done__\0'* ]] || { print -u2 "_zshcs_chdir did not emit delimiter on missing dir"; return 1; }
}

# ------------------------------------------------------------------------------
# Test 5: Completion Hooks and Settings Validation
# ------------------------------------------------------------------------------
test_completion_hooks_and_options() {
    ZSHCS_CACHE_DIR="$TEST_TMPDIR/cache" zsh --no-rcs -c '
        source "'"$ZPTYRC_SCRIPT"'"

        # Verify compadd is overridden as function
        typeset -f compadd >/dev/null || { print -u2 "compadd is not defined as a function"; exit 1; }

        # Verify completion options and parameters
        [[ $HISTSIZE -le 1 ]] || { print -u2 "HISTSIZE is unexpectedly high ($HISTSIZE)"; exit 1; }
        [[ -z ${HISTFILE:-} ]] || { print -u2 "HISTFILE is set"; exit 1; }
        [[ ! -o beep ]] || { print -u2 "beep option is enabled"; exit 1; }
        [[ -o ignore_eof ]] || { print -u2 "ignore_eof option is not set"; exit 1; }

        # Verify compprefuncs and comppostfuncs
        (( ${compprefuncs[(I)null-line]} )) || { print -u2 "null-line missing from compprefuncs"; exit 1; }
        (( ${comppostfuncs[(I)null-line]} )) || { print -u2 "null-line missing from comppostfuncs"; exit 1; }
        (( ${comppostfuncs[(I)reset-compfuncs]} )) || { print -u2 "reset-compfuncs missing from comppostfuncs"; exit 1; }
    '
}

# ------------------------------------------------------------------------------
# Test 6: Capture Script Interactive Zpty Completion End-to-End
# ------------------------------------------------------------------------------
test_capture_script_e2e() {
    local capture_cache="$TEST_TMPDIR/capture_e2e_cache"
    mkdir -p "$capture_cache"

    # Spawn capture.zsh via zsh subshell and feed commands via coproc / pipe
    local test_sub_dir="$TEST_TMPDIR/workspace"
    mkdir -p "$test_sub_dir"
    touch "$test_sub_dir/alpha.txt" "$test_sub_dir/beta.sh"

    local result
    result=$(ZSHCS_CACHE_DIR="$capture_cache" zsh --no-rcs -c '
        coproc zsh "'"$CAPTURE_SCRIPT"'"
        print -p "chdir:'"$test_sub_dir"'"
        print -p "input:ls "
        local line=""
        local collected=""
        while IFS= read -t 10 -r -p line; do
            collected+="$line"$'"'\n'"'
            if [[ "$line" == *$'"'\x01EOC\x01'"'* ]]; then
                break
            fi
        done
        print -r -- "$collected"
    ')

    [[ "$result" == *$'\x01EOC\x01'* ]] || { print -u2 "capture.zsh output missing EOC marker: $result"; return 1; }
    [[ "$result" == *"alpha.txt"* ]] || { print -u2 "capture.zsh output missing alpha.txt candidate: $result"; return 1; }
    [[ "$result" == *"beta.sh"* ]] || { print -u2 "capture.zsh output missing beta.sh candidate: $result"; return 1; }
    return 0
}

# ------------------------------------------------------------------------------
# Test 7: Capture Script Invalid Message Error Handling
# ------------------------------------------------------------------------------
test_capture_script_invalid_message() {
    local capture_cache="$TEST_TMPDIR/invalid_msg_cache"
    mkdir -p "$capture_cache"

    local err_output
    err_output=$(ZSHCS_CACHE_DIR="$capture_cache" zsh --no-rcs -c '
        coproc zsh "'"$CAPTURE_SCRIPT"'" 2>&1
        print -p "invalid_message_prefix"
        local line=""
        while IFS= read -t 10 -r -p line; do
            print -r -- "$line"
        done
    ' 2>&1 || true)

    [[ "$err_output" == *"error: invalid message"* ]] || { print -u2 "Expected invalid message error, got: $err_output"; return 1; }
    return 0
}

# ------------------------------------------------------------------------------
# Test 8: Sequential Multiple Queries Over Single Pty Session
# ------------------------------------------------------------------------------
test_capture_script_sequential_queries() {
    local capture_cache="$TEST_TMPDIR/seq_cache"
    mkdir -p "$capture_cache"

    local result
    result=$(ZSHCS_CACHE_DIR="$capture_cache" zsh --no-rcs -c '
        coproc zsh "'"$CAPTURE_SCRIPT"'"
        print -p "input:ls "
        local line=""
        local eoc_count=0
        while IFS= read -t 10 -r -p line; do
            if [[ "$line" == *$'"'\x01EOC\x01'"'* ]]; then
                eoc_count=$((eoc_count + 1))
                break
            fi
        done
        print -p "input:cd "
        while IFS= read -t 10 -r -p line; do
            if [[ "$line" == *$'"'\x01EOC\x01'"'* ]]; then
                eoc_count=$((eoc_count + 1))
                break
            fi
        done
        print -r -- "$eoc_count"
    ')

    [[ "$result" == *"2"* ]] || { print -u2 "Expected 2 sequential EOC responses, got: $result"; return 1; }
    return 0
}

# ------------------------------------------------------------------------------
# Test 9: compadd Direct Delegation and Candidate Interception
# ------------------------------------------------------------------------------
test_compadd_hook_delegation() {
    local zptyrc="$ZPTYRC_SCRIPT"
    ZSHCS_CACHE_DIR="$TEST_TMPDIR/cache" zsh --no-rcs -c '
        source "$1"

        # Verify compadd is defined as a shell function
        typeset -f compadd >/dev/null || exit 1

        # Mock builtin to intercept and verify direct function invocation
        typeset -a delegated_calls
        builtin() {
            delegated_calls+=("$*")
        }

        # 1. Verify -A delegates to builtin compadd
        compadd -A hits -- "cand1" "cand2"
        [[ "${delegated_calls[-1]}" == "compadd -A hits -- cand1 cand2" ]] || exit 2

        # 2. Verify -O delegates to builtin compadd
        compadd -O hits -- "cand1" "cand2"
        [[ "${delegated_calls[-1]}" == "compadd -O hits -- cand1 cand2" ]] || exit 3

        # 3. Verify -D delegates to builtin compadd
        compadd -D dscr -- "cand1" "cand2"
        [[ "${delegated_calls[-1]}" == "compadd -D dscr -- cand1 cand2" ]] || exit 4

        # 4. Verify -M match -A hits delegates
        compadd -M match -A hits -- "cand1"
        [[ "${delegated_calls[-1]}" == "compadd -M match -A hits -- cand1" ]] || exit 5

        # 5. Verify non-delegating compadd intercepts and formats candidates
        builtin() {
            if [[ "$1" == "compadd" && "$*" == *"-A __hits"* ]]; then
                __hits=("item1" "item2")
                __dscr=("First description" "Second description")
            fi
        }

        local out
        out=$(compadd -d desc -f -- "item1" "item2")
        [[ "$out" == *item1*$'\t'*First\ description* ]] || exit 6
        [[ "$out" == *item2*$'\t'*Second\ description* ]] || exit 7

        exit 0
    ' _zshcs_subshell "$zptyrc"
}

# ------------------------------------------------------------------------------
# Test 10: Directory Sync with Spaces and Quotes
# ------------------------------------------------------------------------------
test_chdir_with_spaces_and_quotes() {
    local capture_cache="$TEST_TMPDIR/spaces_cache"
    mkdir -p "$capture_cache"

    local special_dir="$TEST_TMPDIR/special dir with spaces and 'quotes'"
    mkdir -p "$special_dir"
    touch "$special_dir/special_file.txt"

    local result
    result=$(ZSHCS_CACHE_DIR="$capture_cache" zsh --no-rcs -c '
        coproc zsh "'"$CAPTURE_SCRIPT"'"
        print -p "chdir:'"$special_dir"'"
        print -p "input:ls "
        local line=""
        local collected=""
        while IFS= read -t 10 -r -p line; do
            collected+="$line"$'"'\n'"'
            if [[ "$line" == *$'"'\x01EOC\x01'"'* ]]; then
                break
            fi
        done
        print -r -- "$collected"
    ')

    [[ "$result" == *$'\x01EOC\x01'* ]] || { print -u2 "Special path missing EOC marker: $result"; return 1; }
    [[ "$result" == *"special_file.txt"* ]] || { print -u2 "Special path missing candidate: $result"; return 1; }
    return 0
}

# ------------------------------------------------------------------------------
# Main Runner
# ------------------------------------------------------------------------------
main() {
    print -P "%F{cyan}=====================================================%f"
    print -P "%F{cyan}         zshcs Zsh Script Unit Test Suite            %f"
    print -P "%F{cyan}=====================================================%f"

    run_test_case "Syntax validation (zsh -n)" test_syntax_check
    run_test_case "Module availability (zsh/zpty, zsh/zutil)" test_module_loading
    run_test_case "Custom cache dir creation" test_cache_dir_creation
    run_test_case "XDG cache dir fallback" test_cache_dir_xdg_fallback
    run_test_case "Directory sync helper (_zshcs_chdir)" test_chdir_sync_helper
    run_test_case "Directory sync non-existent target" test_chdir_non_existent_directory
    run_test_case "Completion hook and options setup" test_completion_hooks_and_options
    run_test_case "Interactive zpty completion capture (E2E)" test_capture_script_e2e
    run_test_case "Invalid message error handling" test_capture_script_invalid_message
    run_test_case "Sequential queries over pty" test_capture_script_sequential_queries
    run_test_case "compadd delegation and interception" test_compadd_hook_delegation
    run_test_case "Directory sync with spaces and quotes" test_chdir_with_spaces_and_quotes

    print -P "%F{cyan}-----------------------------------------------------%f"
    print -P "Total:  $TESTS_RUN"
    print -P "%F{green}Passed: $TESTS_PASSED%f"
    if [[ $TESTS_FAILED -gt 0 ]]; then
        print -P "%F{red}Failed: $TESTS_FAILED%f"
        print -P "%F{red}Some tests failed!%f"
        return 1
    else
        print -P "%F{green}All Zsh unit tests passed successfully!%f"
        return 0
    fi
}

main "$@"

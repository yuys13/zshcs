#!/usr/bin/env zsh

zmodload zsh/zpty || { echo 'error: missing module zsh/zpty' >&2; exit 1 }

local zpty_rcfile=${0:h}/zptyrc.zsh
[[ -r $zpty_rcfile ]] || { echo "error: rcfile not found: $zpty_rcfile" >&2; exit 1; }

# Spawn shell with non-blocking (-b) output
zpty -b z zsh --no-rcs --interactive

# Line buffer for pty output
local line=

# Initialize shell settings before processing
zpty -w z "source ${(qq)zpty_rcfile} && echo ok || exit 2"
zpty -r -m z line '*ok'$'\r' || { echo "error: pty initialization failure" >&2; exit 2 }

# Constants
MSG_CHDIR="chdir:"
MSG_INPUT="input:"

END_OF_COMPLETION=$'\n\x01EOC\x01\n'

ACCEPT_LINE=$'\C-J'
COMPLETE_WORD=$'\C-I'
KILL_BUFFER=$'\C-U'
NULL_BYTE=$'\0'

# Main loop to read from stdin and process completion
local message=
local user_input=
while true; do
    IFS= read -r message || break

    case $message in
        $MSG_CHDIR*)
            # Change the current working directory in the pty
            local new_cwd=${message#$MSG_CHDIR}
            zpty -w -n z "${KILL_BUFFER}cd ${(qq)new_cwd}${ACCEPT_LINE}"
            continue
            ;;
        $MSG_INPUT*)
            # Do completion
            user_input=${message#$MSG_INPUT}
            ;;
        *)
            echo "error: invalid message: $message" >&2
            exit 1
            ;;
    esac

    # Trigger completion and send it to the pty
    zpty -w -n z "${KILL_BUFFER}${user_input}${COMPLETE_WORD}"

    # Completion results are output between null bytes
    zpty -r -m z line "*${NULL_BYTE}*${NULL_BYTE}" || { echo "error: pty read failure" >&2; exit 1 }
    echo -nE - "${${(@0)line}[2]}${END_OF_COMPLETION}"

    user_input=
done

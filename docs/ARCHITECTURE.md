# Architecture

This document describes the internal design and mechanics of `zshcs`, a Zsh
Language Server.

## System Overview

The project consists of two main components:

1. **LSP Server**: A Rust application acting as the interface between the
   editor/IDE and the Zsh environment.
2. **Completion Engine**: A Zsh script (`bin/capture.zsh`) that interacts with
   the Zsh shell to retrieve valid completion candidates.

## Component Details

### 1. LSP Server (`src/main.rs`)

- **Library**: Built using `tower-lsp` to handle the Language Server Protocol
  communication.
- **State Management**:
  - Maintains open documents in memory using `DashMap`.
  - Tracks document versions to handle out-of-order updates.
- **Resource Lifecycle & Actor Pattern**:
  - The embedded Zsh scripts (`capture.zsh` and `zptyrc.zsh`) are written to a temporary directory created on startup.
  - A persistent background daemon is spawned via `tokio::spawn` to run `capture.zsh`.
  - The `Backend` struct holds an `mpsc::Sender` channel to communicate with this daemon instead of spawning new processes.
- **Synchronization**: Supports `TextDocumentSyncKind::INCREMENTAL`.
  - `didChange` events update the in-memory document state by applying changes
    to the byte offsets calculated from LSP `Position` (line/character).

### 2. Completion Engine (`bin/capture.zsh` and `bin/zptyrc.zsh`)

This is the core logic for providing authentic Zsh completions.

- **Embedding**: The scripts are embedded into the Rust binary at compile
  time using `include_str!`.
- **Execution Flow**:
  1. At startup, a temporary directory is created with `capture.zsh` and `zptyrc.zsh`.
  2. The server spawns a persistent daemon running `capture.zsh`.
  3. The daemon initializes an interactive Zsh session inside `zpty` using configurations from `zptyrc.zsh`.
  4. It then enters an infinite loop, reading standard input for RPC-like messages (`input:<text>`).
- **Mechanism**:
  - **`zpty`**: The script uses the `zsh/zpty` module to spawn a pseudo-terminal
    session. This simulates an interactive Zsh environment.
  - **Hooking**: It overrides the `compadd` builtin function in `zptyrc.zsh`. Instead of
    displaying completions to the user, the overridden `compadd` captures the raw
    candidates and descriptions passed by Zsh's completion system.
  - **Output**: The script prints the captured completion items to `stdout` in a
    tab-separated format (`candidate\tdescription`). The completion finishes with an EOC marker (`\x01EOC\x01`).

## Data Flow for Completion

1. **Client**: Sends `textDocument/completion` with cursor position.
2. **Server (Rust)**:
   - Extracts the relevant line or context from the in-memory document.
   - Sends a request (the prefix context and a `oneshot::Sender`) through the mpsc channel to the background daemon task.
3. **Background Daemon Task (Rust)**:
   - Writes `input:<prefix>` to the `stdin` of the running `capture.zsh` subprocess.
   - Reads `stdout` lines from the subprocess until it sees the `\x01EOC\x01` marker.
4. **Subprocess (Zsh within `zpty`)**:
   - Triggers `Ctrl+U` to clear buffer, inputs the prefix, and simulates hitting `Tab` (`Ctrl+I`).
   - `compadd` hook intercepts results, formatted as internal JSON-like strings.
   - Prints results to `stdout`, terminating the output with `\x01EOC\x01`.
5. **Server (Rust)**:
   - Parses the tab-separated stdout lines into LSP `CompletionItem` objects.
   - Returns the parsed result through the `oneshot` channel back to the LSP completion handler.
   - Returns the final list to the Client.

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
- **Resource Lifecycle**:
  - Manages the lifecycle of the embedded `capture.zsh` execution script,
    ensuring it is created once at startup and cleaned up on shutdown.
  - Uses `tempfile::TempPath` for automatic file deletion.
- **Synchronization**: Supports `TextDocumentSyncKind::INCREMENTAL`.
  - `didChange` events update the in-memory document state by applying changes
    to the byte offsets calculated from LSP `Position` (line/character).

### 2. Completion Engine (`bin/capture.zsh`)

This is the core logic for providing authentic Zsh completions.

- **Embedding**: The script content is embedded into the Rust binary at compile
  time using `include_str!("../bin/capture.zsh")`.
- **Execution Flow**:
  1. When the server starts, it creates a temporary file containing the
     `capture.zsh` script and keeps its path.
  2. For each `completion` request, the server determines the context (current
     line/cursor position) from the document.
  3. It executes the existing temporary script as a subprocess.
- **Mechanism**:
  - **`zpty`**: The script uses the `zsh/zpty` module to spawn a pseudo-terminal
    session. This allows it to simulate an interactive Zsh environment.
  - **Hooking**: It overrides the `compadd` builtin function. Instead of
    displaying completions to the user, the overridden `compadd` captures the raw
    candidates and descriptions passed by Zsh's completion system.
  - **Output**: The script prints the captured completion items to `stdout` in a
    format parsed by the Rust server (e.g., `candidate -- description`).

## Data Flow for Completion

1. **Client**: Sends `textDocument/completion` with cursor position.
2. **Server (Rust)**:
   - Extracts the relevant line or context from the in-memory document.
   - Spawns a subprocess using the pre-created `capture.zsh` temp file:
     `<temp_script> <prefix>`.
3. **Subprocess (Zsh)**:
   - Initializes a clean Zsh environment.
   - Invokes Zsh completion system (`compinit`).
   - Triggers completion for the provided context.
   - `compadd` hook intercepts results.
   - Prints results to `stdout`.
4. **Server (Rust)**:
   - Parses `stdout` from the subprocess.
   - Converts lines into LSP `CompletionItem` objects.
   - Returns the list to the Client.

# Zsh Completion Server (zshcs)

[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/yuys13/zshcs)

`Zsh Completion Server` (`zshcs`) is a Language Server Protocol (LSP)
implementation for Zsh shell scripts.
It aims to provide high-performance and accurate completion features by
leveraging Zsh's own completion system.

## Features

- **Accurate Completion**: Directly utilizes Zsh's internal completion
  mechanisms (`compinit`, `compadd`, etc.) to achieve the same completion
  accuracy as Zsh itself.
- **LSP Compliant**: Built using the `tower-lsp` crate, making it compatible
  with various LSP-supported editors such as Neovim.
- **High Performance**: Implemented in Rust for efficient document management
  and Zsh process control.

## Installation

### Using cargo install

You can install `zshcs` directly from the GitHub repository:

```bash
cargo install --git https://github.com/yuys13/zshcs.git
```

### Building from source

Alternatively, you can clone the repository and build it manually:

```bash
git clone https://github.com/yuys13/zshcs.git
cd zshcs
cargo build --release
# The binary will be located at target/release/zshcs
```

## Usage

### Neovim Configuration

For Neovim 0.11 or later, you can use the built-in `vim.lsp.enable` function.

1. Create a configuration file at `~/.config/nvim/lsp/zshcs.lua`:

```lua
return {
  cmd = { "zshcs" },
  filetypes = { "zsh" },
  root_markers = { ".git" },
  -- Optional: Enable experimental features (definition, diagnostics, hover)
  settings = {
    zshcs = {
      experimental = {
        definition = true,
        diagnostics = true,
        hover = true,
      },
    },
  },
}
```

2. Enable it in your `init.lua`:

```lua
vim.lsp.enable("zshcs")
```

### Experimental Features

- **Definition Provider (`textDocument/definition`)**: By default, definition jumping is disabled. You can opt-in by configuring `settings.zshcs.experimental.definition = true` in your LSP client configuration or `initializationOptions`. It resolves definitions for shell functions (`func() { ... }`, `function func { ... }`), shell variable declarations and assignments (`VAR=...`, `export VAR=...`, `typeset -g VAR=...`, `local VAR=...`), and external script file references (`source <path>`, `. <path>`).
- **Syntax Diagnostics (`zsh -n`)**: By default, diagnostics are disabled to avoid unnecessary process overhead. You can opt-in by configuring `settings.zshcs.experimental.diagnostics = true` in your LSP client configuration or `initializationOptions`.
- **Hover Documentation (`textDocument/hover`)**: By default, hover documentation is disabled. You can opt-in by configuring `settings.zshcs.experimental.hover = true` in your LSP client configuration or `initializationOptions`. It provides structured Markdown documentation for Zsh builtins and reserved words, and automatically falls back to full manual pages (`man`) formatted in code blocks for external commands (with 5000ms timeout protection).


## How It Works

`zshcs` consists of two main components:

1. **LSP Server (Rust)**: Handles communication with the editor, document
   synchronization, and spawning/communicating with the background Zsh daemon.
2. **Completion Engine (Zsh)**: An embedded `capture.zsh` script runs as a persistent
   background daemon. It uses the `zpty` module to simulate an interactive Zsh session
   and hooks the `compadd` built-in to capture completion candidates efficiently without
   the overhead of repeated initializations.

For more details on the architecture, please refer to
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## For Developers

### Prerequisites

- Rust (latest stable)
- Zsh

### Build and Test

```bash
# Build
cargo build

# Run tests
cargo test

# Check formatting
cargo fmt --check

# Run linter
cargo clippy --no-deps --all-targets -- -D warnings
```

## License

[MIT License](LICENSE)

## Original code

This project includes code derived from the following repositories, and we extend our gratitude to their original authors and contributors for their great work.

- [ddc-source-shell_native](https://github.com/Shougo/ddc-source-shell_native)
- [zsh-capture-completion](https://github.com/Valodim/zsh-capture-completion)
- [deoplete-zsh](https://github.com/deoplete-plugins/deoplete-zsh)

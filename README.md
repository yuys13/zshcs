# Zsh Completion Server (zshcs)

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
}
```

1. Enable it in your `init.lua`:

```lua
vim.lsp.enable("zshcs")
```

## How It Works

`zshcs` consists of two main components:

1. **LSP Server (Rust)**: Handles communication with the editor, document
   synchronization, and Zsh process management.
2. **Completion Engine (Zsh)**: An embedded `capture.zsh` script uses the `zpty`
   module to simulate an interactive Zsh session and hooks the `compadd`
   built-in to capture completion candidates.

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

This project includes code derived from the following repositories.
We would like to express our gratitude to the original authors and
contributors for their great work.

- [zsh-capture-completion](https://github.com/Valodim/zsh-capture-completion)
- [deoplete-zsh](https://github.com/deoplete-plugins/deoplete-zsh)

# Agent Instructions

This document provides instructions for agents working on this repository.

## Project Overview

This project is an implementation of the Language Server Protocol (LSP) for zsh.
The primary goal is to provide completion features. It is built using the
`tower-lsp` crate.

## Documentation

For detailed information about the internal design, mechanics of the completion
engine, and data flow, please refer to:

- [**docs/ARCHITECTURE.md**](docs/ARCHITECTURE.md)

## Development Methodology

All new code should be written following the principles of Test-Driven
Development (TDD) as described by Kent Beck. This involves the following cycle:

1. **Red**: Write a failing test for a new feature. Run only the targeted test
   (e.g., `cargo test <test_name>`) to confirm failure.
2. **Green**: Write the minimum amount of code required to make the test pass.
   Verify with the targeted test.
3. **Refactor**: Improve code design while keeping tests green. Run relevant
   targeted tests to verify changes. Do NOT run full test suites repeatedly
   during local refactoring loops; rely on targeted tests and leave repository-wide
   verification to the pre-commit hook.

### Testing and Context Efficiency Guidelines

To conserve LLM context window and prevent redundant execution:

- **Targeted Testing**: Always run specific tests instead of full test suites
  during development (e.g., `cargo test <test_name>`, `cargo test --lib -- <module>`).
  Avoid running `cargo test --all-targets` for iterative development.
- **Quiet Mode**: Use quiet flags (e.g., `cargo test -q <test_name>`) to suppress
  lengthy output lists that consume context.

## Project Structure

- `src/main.rs`: The main CLI entry point of the application.
- `src/lib.rs`: The root library crate exposing modules.
- `src/server.rs`: The LSP server backend implementing `LanguageServer`.
- `src/completion.rs`: Completion daemon runner and candidate parser.
- `src/document.rs`: Document position and byte offset utilities.
- `tests/`: Integration tests for server lifecycle and completions.
- `bin/capture.zsh`: The Zsh script used to hook into Zsh's completion system
  and capture candidates. Embedded and managed by the LSP server.
- `docs/`: Contains project documentation, including architectural details.
- `Cargo.toml`: The manifest file for this Rust project, containing metadata
  and dependencies.
- `.github/`: Contains GitHub Actions workflows, such as the CI pipeline.

## Pre-commit Checks

All pre-commit verification (formatting, Clippy linting, build, Rust test suite,
and Zsh script unit tests) is fully automated via the repository's native Git hook.
Agents must ensure the hook is active:

```bash
git config core.hooksPath .githooks
```

Once configured, simply run `git commit`. The hook will automatically execute the
full verification suite before accepting the commit. Do NOT manually run the full
check suite prior to `git commit`, as the hook guarantees verification and running
it beforehand duplicates work and wastes quota.

## Commit Messages

All commit messages should follow the
[Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/)
specification. This helps in automating changelog generation and makes the
commit history more readable.

A commit message should be structured as follows:

```text
<type>[optional scope]: <description>

[optional body]

[optional footer(s)]
```

**Example:**

```text
feat: allow provided config object to extend other configs
```

## Documentation and Settings Ordering

When defining or documenting configuration keys (such as `settings.zshcs.experimental.*`), schema options, or lists of features:
- Always sort keys, example configuration snippets (e.g. Lua tables), and their corresponding descriptive documentation in **alphabetical order (A-Z)**.
- Ensure the explanatory sections strictly follow the same ordering as the configuration examples.

## Nix Build and Sandbox Testing Discipline

- When external tools or dependencies are declared in `flake.nix` (`nativeBuildInputs`), corresponding tests must **strictly assert** their presence and functionality instead of silently skipping.
- Avoid loose skips in tests when running under `nix build`; tests should fail fast if the required environment is incomplete.
- If a test command or implementation is modified and an external package in `nativeBuildInputs` is no longer needed, remove it immediately to keep derivations minimal.

## Pull Request Guidelines

- **Single Concern per PR**: Each pull request must focus on solving a single, well-defined concern or feature. Avoid combining multiple unrelated features, refactorings, or subsystem changes into a single PR. Keep PRs granular, self-contained, and easy to review and revert if necessary.
- **No Internal Proposal References**: Do not reference uncommitted or untracked proposal/scratch files (e.g., `docs/IMPROVEMENT_PROPOSALS.md`) in PR titles, PR descriptions, or commit messages. Describe features strictly in terms of their public functionality and architectural changes.

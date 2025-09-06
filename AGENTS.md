# Agent Instructions

This document provides instructions for agents working on this repository.

## Project Overview

This project is an implementation of the Language Server Protocol (LSP) for zsh.
The primary goal is to provide completion features. It is built using the
`tower-lsp` crate.

## Development Methodology

All new code should be written following the principles of Test-Driven
Development (TDD) as described by Kent Beck. This involves the following cycle:

1. **Red**: Write a failing test for a new feature.
2. **Green**: Write the minimum amount of code required to make the test pass.
3. **Refactor**: Improve the code design while ensuring all tests still pass.

## Project Structure

- `src/main.rs`: The main entry point of the application.
- `Cargo.toml`: The manifest file for this Rust project, containing metadata
  and dependencies.
- `.github/`: Contains GitHub Actions workflows, such as the CI pipeline.

## Pre-commit Checks

Before committing any changes, please ensure that the following checks pass
locally. These are the same checks that run in our CI pipeline.

1. **Check formatting:**

   ```bash
   cargo fmt --check
   ```

2. **Run Clippy (linter):**

   ```bash
   cargo clippy --no-deps --all-targets -- -D warnings
   ```

3. **Build the project:**

   ```bash
   cargo build
   ```

4. **Run tests:**

   ```bash
   cargo test
   ```

Running these commands will help ensure that your changes are consistent with the
project's standards and that all tests pass.

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

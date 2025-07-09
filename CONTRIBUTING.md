# Contributing

We welcome contributions to this project! Please follow these guidelines to ensure a smooth development process.

## Development Workflow

### Setting up the Environment

1.  **Install Rust:** If you don't have Rust installed, follow the official instructions at [https://www.rust-lang.org/tools/install](https://www.rust-lang.org/tools/install).
2.  **Clone the repository:**
    ```bash
    git clone <repository-url>
    cd <repository-name>
    ```
3.  **Install dependencies:** The necessary dependencies will be handled by Cargo, Rust's package manager.

### Running Checks Locally

Before submitting your changes, please ensure that all checks pass locally.

1.  **Check formatting:**
    ```bash
    cargo fmt --check
    ```
    To automatically format your code, run:
    ```bash
    cargo fmt
    ```
2.  **Run Clippy (Linter):**
    ```bash
    cargo clippy -- -D warnings
    ```
3.  **Build the project:**
    ```bash
    cargo build
    ```
4.  **Run tests:**
    ```bash
    cargo test
    ```

## Commit Messages

All commit messages must adhere to the **Conventional Commits** specification. This helps in automating changelog generation and makes the commit history more readable.

You can find the full specification here: [https://www.conventionalcommits.org/en/v1.0.0/](https://www.conventionalcommits.org/en/v1.0.0/)

Thank you for contributing!

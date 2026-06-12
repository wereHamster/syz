# syz

A fast, local-first project dependency tracking system.

## What is it?

`syz` helps you track and monitor the dependencies used in your software projects.
It consists of two parts:

- **`syzd`**: The background server that continuously monitors and syncs dependency data.
- **`syzctl`**: The command-line tool you use to query and manage your dependencies.

## Getting Started

### 1. Environment Setup

You must set the following environment variables:

- `GITHUB_TOKEN`: Your GitHub personal access token (required for GitHub ecosystem support).
- `SYZD_AUTH_TOKEN`: A secret string used to authenticate clients to the server.

If you use [direnv](https://direnv.net/), you can simply create a `.env` file in the project root:

```bash
echo "GITHUB_TOKEN=your_github_token" > .env
echo "SYZD_AUTH_TOKEN=your_random_secret_string" >> .env
```

### 2. Running the Server

Start the `syzd` background server in one terminal:

```bash
cargo run --bin syzd
```

### 3. Using `syzctl`

Open another terminal to manage your projects via the CLI.

**Add a new project:**

```bash
# Syntax: github:<owner>/<repo>
cargo run --bin syzctl -- add github:owner/repo
```

**Launch the Interactive TUI:**

To browse your dependencies in a terminal user interface, run:

```bash
cargo run --bin syzctl
```

- **Navigation**: Use the **arrow keys** to navigate the interface.
- **Help**: Available hotkeys are displayed at the bottom of the screen.

## Supported Ecosystems

Currently, `syz` can track dependencies for:

- **Cargo** (Rust)
- **NPM** (JavaScript / TypeScript)
- **GitHub Actions**

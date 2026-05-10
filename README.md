# syz

A fast, local-first project dependency tracking system.

## What is it?

`syz` helps you track and monitor the dependencies used in your software projects.
It consists of two parts:

- **`syzd`**: The background server that continuously monitors and syncs dependency data.
- **`syzctl`**: The command-line tool you use to query and manage your dependencies.

## Supported Ecosystems

Currently, `syz` can track dependencies for:

- **Cargo** (Rust)
- **NPM** (JavaScript / TypeScript)
- **GitHub Actions**

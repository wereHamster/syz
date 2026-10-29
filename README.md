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

- `SYZD_AUTH_TOKEN`: A secret string used to authenticate clients to the server.
- `GITHUB_WEBHOOK_SECRET`: The secret configured on the GitHub webhook, used to verify incoming deliveries.

`syzd` also needs GitHub credentials, either for a GitHub App or as a personal access token.

#### GitHub App

- `GITHUB_APP_ID`: The ID of the GitHub App `syzd` authenticates as.
- `GITHUB_APP_PRIVATE_KEY`: The app's private key in PEM format. Newlines may be written as `\n`.

The app's webhook delivers the pushes of every repository it is installed on.

#### Personal Access Token

Use this when you can't or don't want to create a GitHub App.
It is used only when `GITHUB_APP_ID` is not set.

- `SYZD_GITHUB_TOKEN`: A personal access token that `syzd` uses for every repository.

A fine-grained token needs these repository permissions:

- **Contents**: Read and write
- **Pull requests**: Read and write
- **Workflows**: Read and write, to update GitHub Actions
- **Metadata**: Read-only

A fine-grained token covers the repositories of a single owner.
To track repositories of several owners, use a classic token with the `repo` and `workflow` scopes.

Compared to a GitHub App, keep in mind:

- Branches, commits, and pull requests are created as the token's user.
  A dedicated machine account keeps them apart from your own.
- All repositories share the token's rate limit of 5,000 requests per hour.
- Tools that talk to GitHub themselves, such as `nix` when it fetches flake inputs, get the token with all of its permissions.
  With a GitHub App, they get a short-lived token that can only read repository contents.
- The token expires and must be replaced by hand.
- Each repository, or its organization, needs a webhook that sends `push` events to `/github/webhook` with the `GITHUB_WEBHOOK_SECRET`.

#### `.env`

If you use [direnv](https://direnv.net/), you can simply create a `.env` file in the project root:

```bash
echo "GITHUB_APP_ID=your_app_id" > .env
echo "GITHUB_APP_PRIVATE_KEY=your_app_private_key" >> .env
echo "SYZD_AUTH_TOKEN=your_random_secret_string" >> .env
echo "GITHUB_WEBHOOK_SECRET=your_webhook_secret" >> .env
```

With a personal access token, replace the first two lines with:

```bash
echo "SYZD_GITHUB_TOKEN=your_personal_access_token" > .env
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

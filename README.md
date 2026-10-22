# ecsterm

Browse ECS clusters › services › containers › tasks and run the matching AWS CLI commands. Inspired by the ECS explorer in the [AWS Toolkit for VS Code](https://github.com/aws/aws-toolkit-vscode).

![ecsterm](docs/screenshot.png)

Requires the [AWS CLI v2](https://docs.aws.amazon.com/cli/latest/userguide/getting-started-install.html) and the [Session Manager plugin](https://docs.aws.amazon.com/systems-manager/latest/userguide/session-manager-working-with-install-plugin.html) for container shells.

## Install

### macOS

```
brew install aronjohanns/tap/ecsterm
```

### Linux / macOS

```
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/aronjohanns/ecsterm/releases/latest/download/ecsterm-installer.sh | sh
```

### Windows

```
powershell -ExecutionPolicy Bypass -c "irm https://github.com/aronjohanns/ecsterm/releases/latest/download/ecsterm-installer.ps1 | iex"
```

### Cargo

```
cargo install ecsterm
```

Binaries are on the [releases page](https://github.com/aronjohanns/ecsterm/releases).

```
ecsterm [--profile NAME] [--region REGION] [--config PATH]
```

When a profile is opened, its credentials are checked first. If they have expired, ecsterm runs the login from the profile: `aws sso login` for profiles with `sso_session`/`sso_start_url`, `aws login` for profiles with `login_session`. If the session expires while ecsterm is open, a failed command offers the login before returning. Press `L` to log in again at any time; either way the explorer keeps its place.

## Config

Linux/macOS: `~/.config/ecsterm/config.toml` (or `$XDG_CONFIG_HOME`). Windows: `%APPDATA%\ecsterm\config.toml`.

```
mkdir -p ~/.config/ecsterm && ecsterm --dump-config > ~/.config/ecsterm/config.toml
```

All actions and placeholders are in [default-config.toml](default-config.toml). Defining a level, e.g. `[[task]]`, replaces its defaults.

Example: swap `/bin/sh` for bash with completion and TERM set, keep the other task actions.

```toml
[[task]]
title = "Open shell in container"
command = "aws ecs execute-command --cluster {cluster} --task {task} --container {container} --interactive --command \"/bin/sh -c 'export TERM=xterm-256color; exec /bin/bash -l || exec /bin/sh'\" {aws}"

[[task]]
title = "Run a command in container"
command = "aws ecs execute-command --cluster {cluster} --task {task} --container {container} --interactive --command \"<COMMAND>\" {aws}"

[[task]]
title = "Describe task"
command = "aws ecs describe-tasks --cluster {cluster} --tasks {task} {aws}"

[[task]]
title = "Enable command execution (redeploys service)"
command = "aws ecs update-service --cluster {cluster} --service {service} --enable-execute-command --force-new-deployment {aws}"
when = "exec_disabled"
```

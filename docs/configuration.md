# Configuration

Slopbox currently uses a host-owned configuration file at:

```text
$XDG_CONFIG_HOME/slopbox/config.toml
```

or, when `XDG_CONFIG_HOME` is unset:

```text
~/.config/slopbox/config.toml
```

This file is policy authority. Do not place it inside the project. The current schema is intentionally low-level while the higher-level account/project setup described in [experience.md](experience.md) is developed.

## Project setup and launch

From a host terminal in the project:

```bash
slopbox
```

The first launch asks whether project changes should be immediate, staged for review, or read-only. Pi is currently the only supported default agent. Slopbox shows the resulting access and saves it only after an explicit confirmation. Later launches reuse the saved policy and print the access summary before starting Pi.

Pass Pi arguments after `--`, or reconfigure without launching:

```bash
slopbox -- --continue
slopbox init
slopbox init --changes staged --no-host-pi-resources --yes
```

`--yes` is the explicit noninteractive approval path and requires `--changes`. Without it, setup requires a terminal; piped agent input is never consumed as setup approval. If model brokering is enabled but no model account is configured, launch stops with host-side login instructions.

Setup writes a private file under `~/.config/slopbox/projects/<workspace-hash>.toml` (or the corresponding `$XDG_CONFIG_HOME`), never inside the repository. It snapshots the confirmed policy axes as an additional ceiling. `run`, `policy`, and `status` honor that ceiling too, so removing a restrictive `.slopbox.toml` cannot silently broaden future launches.

Re-running `init` re-evaluates current host and project policy and replaces the saved ceiling after confirmation. Review the whole summary: this can change more than the workspace mode. Host-managed routes, identities, and resource declarations still come from global configuration; they are not frozen copies in the setup record. Read-only project files do not imply read-only account access.

The existing `run -- <command>` interface remains available without first-run setup. Where setup exists, it cannot bypass the saved policy ceiling.

## Profile and policy

```toml
profile = "developer"

[policy]
workspace = "live"
network = "allowlist"
runtime = "host"
harness = "trusted"
persistence = "project"
credentials = "brokered"
backend = "native"
```

The profile supplies defaults. Host policy, the saved project setup, and repository policy can reduce them. Unsupported combinations fail at launch.

`network = "none"` disables general egress, not fixed model or configured authenticated account routes. `credentials = "none"` disables model routes; it does not disable account routes or Git signing. Likewise, `--tool-network=none` leaves configured authenticated account routes available to project commands. These settings alone must not be described as an offline session.

The developer runtime exposes the host Nix store read-only, and selected Pi resources may expose other host data. Limiting project writes does not mean that only project files are readable.

Online `developer` sessions let Pi refresh model catalogs through the ordinary deny-by-default gateway. Approve the catalog destination, then reopen `/model` or run `pi update --models`. Stricter profiles keep Pi's catalog and package refresh logic offline.

A project may contain a narrowing-only `.slopbox.toml`:

```toml
[policy]
workspace = "staged"
harness = "data"
network = "none"
```

Project policy cannot grant capabilities beyond the host ceiling.

## Pi resources

```toml
[[pi.read_only_mounts]]
source = "~/.local/share/example-data"
target = "~/.local/share/example-data"

[[pi.temporary_overlay_mounts]]
source = "~/.cache/camoufox"
target = "~/.cache/camoufox"
```

Read-only mounts expose host data without allowing changes. Temporary overlays use the host directory as a read-only lower layer and discard sandbox writes at session exit.

Targets are restricted to sandbox cache, configuration, and data directories. Slopbox rejects known credential roots and resource trees containing Unix sockets. These mounts are disabled when host Pi resources are disabled.

## Secrets

Environment-backed secrets are useful for bootstrap and testing:

```toml
[secrets.example]
source = "environment"
variable = "EXAMPLE_TOKEN"
```

A host credential command can read an existing login without exporting its token to the sandbox:

```toml
[secrets.github]
source = "command"
argv = ["gh", "auth", "token", "--hostname", "github.com", "--user", "ACCOUNT"]
```

Authenticate with `gh` on the host first. This command reads the stored login, not an inherited `GH_TOKEN` override. Slopbox resolves a trusted packaged executable, passes literal arguments with no implicit shell, runs outside the workspace with a cleared environment, and captures at most 64 KiB of stdout. Stderr and failed output are not logged. Host configuration/keychain access stays host-side; status, doctor and policy inspection do not run credential commands.

SOPS remains available for host-managed encrypted files:

```toml
[secrets.forge]
source = "sops"
file = "/absolute/path/outside/project/secrets.yaml"
key = '["FORGE_TOKEN"]'
```

A relative SOPS file path is resolved against the workspace associated with the route that uses it. The SOPS executable and identity stay host-side. Decrypted values are retained only in supervisor memory. Linux selects a Nix-store executable from PATH; macOS also accepts executable files in recognized Homebrew Cellar packages, rejecting installations that overlap the workspace. No SOPS identity or additional filesystem grant is passed to either sandbox role.

## Authenticated HTTP routes

Routes reconstruct authentication for one fixed upstream:

```toml
[[http_routes]]
name = "example-api"
workspace = "~/Projects/example"
upstream = "https://api.example.com/v1/project"
methods = ["GET", "POST"]
authentication = { type = "bearer", secret = "example" }
```

Supported authentication forms are:

```toml
authentication = { type = "basic", username = "bot", secret = "forge" }
authentication = { type = "bearer", secret = "example" }
authentication = { type = "token", secret = "forge" }
```

Private, loopback, and link-local destinations are rejected by default. A host-controlled route for a private service may opt in:

```toml
allow_private_addresses = true
```

A route activates only when its canonical configured workspace matches the current project. The guest receives a local base URL and route name. It never receives the real secret.

Routes fix the upstream origin, base path, methods, and authentication scheme. Slopbox strips guest authentication, rejects redirects and unsafe traversal, pins DNS resolution, requests identity encoding, and redacts exact reflected secret bytes. Transformed secret reflection remains outside the guarantee.

### Git smart HTTP

```toml
[[http_routes]]
name = "project-git"
workspace = "~/Projects/example"
upstream = "https://forge.example.com/org/example.git"
methods = ["GET", "POST"]
authentication = { type = "basic", username = "bot", secret = "forge" }
git_urls = [
  "https://forge.example.com/org/example.git",
  "ssh://git@forge.example.com/org/example.git",
  "git@forge.example.com:org/example.git",
]
```

`git_urls` lists repository URLs to send through this route. Slopbox generates a session-private, read-only Git configuration using `url.*.insteadOf`. It applies to both the agent and inner tool commands, including when general internet is disabled. No signing identity is required.

Inside Slopbox, ordinary commands then use the broker:

```bash
git fetch origin main
git pull --ff-only
git push origin my-branch
```

The repository's `.git/config` and host SSH behavior are unchanged. Branch tracking and push defaults are also unchanged; `pull` still requires a configured upstream. Host credentials remain outside the sandbox.

List the actual URL spellings used by your remotes, including explicit push URLs or SSH aliases. `git config --local --get remote.origin.url` shows the stored fetch URL. Git uses longest-prefix matching, so list complete repository URLs rather than forge-wide or organization-wide prefixes. Other repositories and submodules need their own routes and mappings. Assigning the same URL to different routes fails at launch.

`slopbox status` and `slopbox policy` show the mappings without resolving secrets. Routes without `git_urls` keep their existing behavior; they remain directly usable:

```bash
git push "$SLOPBOX_AUTHENTICATED_HTTP_BASE_URL/project-git" my-branch
```

Large pushes requiring chunked HTTP uploads are not yet supported by the authenticated gateway.

### Provider CLI compatibility

Some provider CLIs require an API route rooted at a conventional path. For example, `fj` always addresses `/api/v1`, so a route named `api` can map to the Forgejo `/api` base:

```toml
[[http_routes]]
name = "api"
workspace = "~/Projects/example"
upstream = "https://forge.example.com/api"
methods = ["GET", "POST", "PATCH", "PUT", "DELETE"]
authentication = { type = "token", secret = "forge" }
```

This exposes every API operation authorized to the configured account. Keep provider-side account permissions narrow. Prefer repository-prefix routes when broad CLI compatibility is unnecessary.

### GitHub CLI

[github.toml](github.toml) is a generic host-configuration template for Git smart HTTP, repository-scoped REST calls and commit signing. Keep the completed configuration outside the repository.

An account route can opt into origin-addressed requests with `direct = true`. Slopbox generates private `gh` transport settings using its documented `http_unix_socket` option. Tool processes receive a synthetic token, not the host login. Pi does not receive the `GH_TOKEN` marker. The socket matches the request's host and path against opted-in routes; overlapping routes fail at launch. This is brokered HTTP over a local socket, not direct Internet access or TLS interception.

With the template's repository-prefix route:

```bash
gh api repos/OWNER/REPO
gh api repos/OWNER/REPO/actions/runs
```

High-level commands using GraphQL need a separately approved `https://api.github.com/graphql` route with `POST`. **An HTTP path cannot restrict GraphQL to one repository.** Such a route grants everything the token permits; use a suitably restricted account/token rather than silently adding a broad API route. Uploads, redirects to log/artifact storage, and other origins also require separate capabilities. Unsupported requests fail closed.

## Git identities and signing

```toml
[[git.identities]]
workspace = "~/Projects/example"
name = "bot"
email = "bot@example.com"
signing_key_fingerprint = "SHA256:..."
```

The matching public key must be available through the host SSH agent when Slopbox starts. Slopbox:

- selects exactly one key matching the fingerprint;
- generates a private sandbox Git configuration;
- enables SSH commit signing;
- exposes the public key and a narrow signing helper;
- keeps the SSH agent socket host-side;
- signs only bounded Git commit objects whose author and committer match the configured identity.

This capability signs commits. It does not provide Git-over-SSH authentication. Use authenticated smart HTTP for fetch and push.

The experimental native macOS port uses the same configuration, with a session-specific socket and read-only helper instead of Linux mount paths. Its helper only signs; verify signatures on the host. Native enforcement and real-account validation are tracked in [macos.md](macos.md#git-signing-and-account-routes).

## Network approvals

From a host terminal:

```bash
slopbox network events
slopbox network events --follow
slopbox network approve <request-id>
slopbox network approve <request-id> --project
slopbox network approvals
slopbox network revoke <rule-id>
```

Commands use the current project; add `--workspace /path/to/project` to select another. Approval defaults to the request's live session. `--project` explicitly persists the destination across sessions. A rule permits one normalized hostname/IP and port, not a URL, HTTP method, subdomain pattern, or safe use of the destination. General network policy and reserved-address checks still apply; these rules do not grant model or account authority.

`events` includes retained denials with project, session, liveness, and timestamp context. `--follow` polls and prints each distinct request ID once, including requests from sessions that end between polls. Repeated identical denials in one session share an ID and first-seen timestamp. `events --json` emits one JSON object per event; `approvals --json` emits an array of rule records and their current states. Timestamps are Unix milliseconds.

Rules have stable IDs, originating request IDs, creation timestamps, and optional revocation timestamps. `approvals` includes active, expired, and revoked rules. Re-approving an active rule is idempotent; approving after revocation creates a new ID. Revocation affects subsequent authorization checks, not existing tunnels or requests already authorized. Other matching rules may still permit the destination. Stop the session when an immediate connection cutoff is needed.

Approving does not replay the denied operation: retry it explicitly. DNS failures and reserved-address denials are not approvable. Commands that bypass the proxy may fail without producing an event.

Rule changes use a host-owned, mode-`0600` `network-rules.json` file, a per-project lock, and atomic replacement. Session events use `events.jsonl`. Neither is exposed to guests; the guest `slopbox denials` command only reads its own events through the gateway.

### Interactive terminals

Foreground interactive launches use a private PTY, including bare `slopbox`. The supervisor forwards terminal dimensions so sandboxed applications receive resize events despite session isolation. It also handles Ctrl-Z suspension and restores host terminal settings on exit. Pipes and noninteractive launches keep their standard streams unchanged.

The approval hotkey is separate and opt-in. Without `--approval-view`, Ctrl-] reaches Pi normally. Host broker diagnostics are buffered and escaped rather than interleaved with Pi's display.

### Clipboard images on Wayland

In an interactive Pi launch (`slopbox` or `slopbox run ... -- pi`), **Ctrl+V** imports the current clipboard image through host `wl-paste`. Slopbox pastes its sandbox-local path into the editor, matching Pi's native image-paste behavior. It does not submit the prompt. Ghostty's classic and Kitty-protocol Ctrl+V encodings are supported; `--approval-view` is not required.

Only PNG, JPEG, WebP, and GIF are imported. Use the terminal's normal paste shortcut (usually Ctrl+Shift+V in Ghostty) for text. Slopbox owns Ctrl+V for direct interactive Pi launches; Pi keybinding remaps do not change the host shortcut. Generic commands, pipelines, and noninteractive launches do not get this bridge.

The packaged command and development shell include `wl-paste`. Raw debug binaries need it on the host Nix PATH; launch with `nix develop -c ./target/debug/slopbox`. Missing Wayland access, empty/non-image clipboards, and failed reads show a host notice. Enter or Escape returns to Pi; no failed paste is retried automatically.

Reads have short deadlines and stay cancellable with Escape or Ctrl+C. Later input is held until the paste finishes, so an immediately following Enter cannot overtake the image. A failed or cancelled read discards queued later input. Limits are 20 MiB per image and 64 images or 128 MiB per launch.

Imports are private, session-local files mounted read-only at `/run/slopbox-clipboard`, including in inner tools. They do not modify the workspace and work with staged/read-only workspaces. Normal shutdown deletes the files; abrupt termination can leave private runtime files behind. Images already read or submitted may remain in Pi history. The sandbox receives no Wayland/X11 socket or continuous clipboard access.

### Host approval view (prototype)

```bash
slopbox --approval-view
slopbox --approval-view -- --continue
# Explicit commands can opt in too:
slopbox run --approval-view --dev-env none -- pi
```

On Linux, use `nix develop -c ./target/debug/slopbox --approval-view` for the debug binary. The packaged command adds bubblewrap to `PATH`; the raw Cargo binary needs the development shell to supply it. Launch checks for bubblewrap before resolving account credentials or realizing the project environment.

The experimental native macOS launcher uses the same host view: `./target/debug/slopbox run --approval-view --dev-env none -- pi`, with the [native runtime configured](macos.md). No additional guest permissions or approval endpoint are exposed. Native host validation commands are in [macos.md](macos.md#host-approval-view).

This is opt-in, not part of bare `slopbox` yet. It requires a foreground host terminal on all three standard streams and at least 80 columns by 24 rows for approvals. RPC, pipelines, and automation should use the normal launch and separate `network` CLI.

Press **Ctrl-]** to open the host view. It shows this session's denial history and active session/project rules; other sessions' private rules are excluded. Related destinations are adjacent. Entry numbers refer to a snapshot, not a moving list:

- `s N`: approve denial N for this session (recommended).
- `p N`: approve denial N persistently for this project.
- `r N`: revoke rule N.
- `refresh`: reload events and rules; `n` / `b`: change pages.
- `q`: return to the guest. `]` then Enter returns and sends a literal Ctrl-].

Every mutation displays the full destination and scope, then requires a fresh 12-character confirmation code. A wrong response cancels; queued input is discarded at view transitions. Commands and confirmation input are not forwarded to Pi. Approval says “retry”; it does not replay the operation. The guest continues executing while the view is open, though its terminal output is paused and may back up.

As with ordinary interactive launches, the supervisor gives the guest a separate PTY and retains the real terminal. Ctrl-Z in the guest view suspends the supervisor and its foreground sandbox process group; use the shell's `fg` to resume. Normal exit and handled termination signals restore host terminal settings. SIGKILL cannot run cleanup.

This is not a screen-preserving terminal multiplexer: returning clears the view and briefly resizes the guest PTY to request a redraw. Pi redraw, suspension, termination, and approvals are tested through PTYs, but visual behavior across terminal emulators still needs manual validation. Resize the window if rendering remains stale, or relaunch without `--approval-view`. Broker diagnostics are isolated from the approval display; their last 16 KiB are escaped and printed when the terminal session ends.

## Concurrent sessions

Multiple Slopbox sessions can run against the same project. Generated Pi resources, settings, synthetic auth files, wrappers, Git signing configuration, and development-environment files belong to each run. Starting or exiting another session does not replace their mounted files. Normal exit and setup failures clean up only that run's generated files.

Pi history, model-catalog state, and package caches remain project-persistent. Live sessions also share the workspace: Slopbox does not prevent two agents from editing the same file or Git branch. Use separate workspaces or staged mode when changes should remain separate. Do not resume the same Pi conversation concurrently.

`slopbox network events` shows retained denials from all project sessions. Request IDs identify the originating session, so `network approve <request-id>` applies only there by default. Session liveness is tracked by host-held locks; an exited supervisor cannot retain session approval authority through a stale marker file.

Restart existing sessions after upgrading from the shared-runtime implementation. A supervisor killed without cleanup may leave generated runtime files behind; automatic stale-file cleanup is not yet implemented.

## Inspecting effective configuration

```bash
slopbox status
slopbox status --verbose
slopbox status --no-host-pi-resources --tool-network none
slopbox policy
slopbox run --dry-run -- /bin/sh
```

`status` explains the next launch's project writes, readable host resources, general and tool networking, configured model providers, account routes, Git mappings, signing identity, and isolation limits. Use the same profile and resource flags you intend to launch with. `--verbose` adds policy axes, mount targets, trusted extension paths, and configuration/state locations.

Inspection does not decrypt secrets, load signing keys, evaluate Nix environments, contact providers, or create project state. Provider entries report configuration presence, not valid credentials. This is a launch-policy summary, not a claim about the state or readiness of an already-running session.

`policy` remains available for raw policy axes, route names, Git URL mappings, and identity. `--dry-run` prints the native backend command without starting it. The default-environment row describes automatic launch: a project `flake.nix` is evaluated and built on the host before its environment is activated in the sandbox.

### Diagnose launch problems

Run from a host terminal, not from the agent's shell:

```bash
slopbox doctor
slopbox doctor --workspace /path/to/project --profile contained
slopbox doctor --no-host-pi-resources
```

`doctor` checks default Pi launch prerequisites using the same saved and repository policy ceilings. It reports unsupported policies, workspace sockets/submounts, missing host tools or resources, missing model configuration, invalid account references, and missing development flakes. It also runs a five-second bubblewrap probe with outer and nested tool namespaces, an empty environment, no workspace mounts, and no broker access.

A failed check returns a nonzero exit status with guidance. Missing project setup is a notice, not a failure: initialize it in a host terminal. Doctor does not create project state, modify approvals, start Pi, execute extensions or shell hooks, evaluate Nix, decrypt secrets, query signing keys, or contact providers. Temporary probe diagnostics are removed afterward.

A passing result covers only the reported checks. Full launch mounts, Pi compatibility, credential validity, signing, upstream connectivity, Nix realization/daemon access, and disk capacity remain unchecked.

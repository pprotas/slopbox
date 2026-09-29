# Configuration

Slopbox currently uses a host-owned configuration file at:

```text
$XDG_CONFIG_HOME/slopbox/config.toml
```

or, when `XDG_CONFIG_HOME` is unset:

```text
~/.config/slopbox/config.toml
```

This file is policy authority. Do not place it inside the project. Identities and accounts can use [shared defaults and directory rules](#shared-host-defaults-and-directory-rules); legacy workspace bindings remain supported. Linux also supports [host-selected executables without Nix](#selected-linux-executables-without-nix), following the [authoritative project direction](direction.md).

## Command launch

Use `slopbox run -- COMMAND` without project setup. A host default makes bare `slopbox` use the same execution path:

```toml
default_command = ["bash"]

[policy]
harness = "none"
credentials = "none"

[runtime]
executables = ["bash", "cat", "ls"]
```

`slopbox -- ARGS` appends literal arguments to `default_command`. Neither command nor environment configuration is accepted from repository policy. Existing saved policy ceilings still apply. Generic subprocesses share the outer role's authority; this does not enable automatic tool/model separation.

Selected runtimes may also use host-configured guest environment values:

```toml
[environment]
CLIENT_BASE_URL = "${SLOPBOX_AUTHENTICATED_HTTP_BASE_URL}/example"
CLIENT_AUTH_TOKEN = "slopbox:example"
CLIENT_STATE = "${HOME}/client"
```

The example requires an attached account route. `${NAME}` expands only public session broker variables, `HOME` and `TMPDIR`; it never reads ambient host variables or named secrets. Missing references fail. Other text, including `$()` and unbraced `$NAME`, stays literal. Values are not recursively expanded. Use this for client settings and placeholders, not real credentials. Managed home/path/proxy/Git/Slopbox variables cannot be overridden. Input and expanded environments are bounded to 64 KiB; inspection lists names without values.

## Saved project ceilings

Older host-side project records in `$XDG_CONFIG_HOME/slopbox/projects/` remain readable as restrictive policy ceilings. Default launch, explicit `run`, `policy` and `status` honor them. Removing a repository manifest or upgrading Slopbox does not discard the saved ceiling. The built-in `init`/Pi setup is removed; review and remove an obsolete record deliberately on the host if you intend to change its ceiling.

## Profile and policy

```toml
profile = "developer"

[policy]
workspace = "live"
network = "allowlist"
runtime = "host"
harness = "none"
persistence = "project"
credentials = "brokered"
backend = "native"
```

The profile supplies defaults. Host policy, the saved project setup, and repository policy can reduce them. Unsupported combinations fail at launch.

`network = "none"` disables general egress, not fixed model or configured authenticated account routes. `credentials = "none"` disables model routes; it does not disable account routes or Git signing. An explicit Linux `slopbox tool-run --network none` also retains configured authenticated account routes. These settings alone must not be described as an offline session.

The default Nix-backed developer runtime exposes the host Nix store read-only. Limiting project writes does not mean that only project files are readable.

A project may contain a narrowing-only `.slopbox.toml`:

```toml
[policy]
workspace = "staged"
harness = "none"
network = "none"
```

Project policy cannot grant capabilities beyond the host ceiling.

## Selected Linux executables without Nix

This opt-in host setting replaces the ambient Nix runtime with selected executables and their discovered dependencies:

```toml
[policy]
harness = "none"
network = "none"
credentials = "none"

[runtime]
executables = ["curl", "git"]
```

Use `slopbox run --dev-env none -- COMMAND`; no built-in harness launch/setup exists. `runtime=project` and contained profiles do not fall back to it. Model/account/signing authority remains governed independently.

Names select protected host installations. Absolute and `~/` executable paths select user installations; authorize their dependency prefix with `dependency_roots = ["~/.local/tools/example"]`. Dependency roots authorize discovered files, not whole-prefix mounts. Repository configuration cannot declare runtime grants. `status --verbose` reports the host selection without running discovery or resolving secrets.

For application data, plugins and installed language environments, explicitly select dedicated trees:

```toml
[runtime]
executables = ["~/.local/tools/example/bin/example"]
bundles = ["~/.local/tools/example", "~/.local/tools/python"]
```

Unlike `dependency_roots`, each bundle grants its **entire tree as read-only code and data**, at its host path. Select application installations, not homes or package-manager caches, and keep secrets out. Native ELF files in bundles receive dependency discovery; Slopbox does not execute package managers or activation scripts. External data/directory links require another explicit bundle. Native ELF links use the existing dependency authorization. Bundles remain unavailable to repository configuration and cannot overlap the workspace or credential/control paths.

See [executable limits](poc-nixless-linux.md) and [bundle behavior and non-Pi harness validation](poc-runtime-bundles.md). Resources outside selected bundles are not automatically inferred. Generic execution does not imply harness/tool separation.

## Selected native macOS executables

The same host-owned `[runtime].executables` selection supports generic native commands without backend-specific harness configuration. It requires `runtime=host` and `harness=none`. An explicit runtime selection makes `--dev-env auto` use those host resources without evaluating a project flake; an explicit `--dev-env flake` remains an error. Bare names select system commands; other installations need absolute or `~/` paths.

Native discovery accepts host-architecture Mach-O executables and dylib dependencies, plus simple scripts with explicitly selected interpreters. `bundles` grants whole read-only application trees; `dependency_roots` authorizes discovered dylib files without exposing those directories. Loader-relative dependencies and known runpath stacks are resolved without executing selected programs. Generic subprocesses retain outer account/model authority; there is no native `tool-run` role in this mode. See [runtime grants and limitations](poc-native-runtime.md).

## Secrets

On macOS, a host-owned route can use an exact generic-password item from the host's default Keychain search list:

```toml
[secrets.example]
source = "keychain"
service = "my-service"
account = "my-account"
```

Lookup happens only on the host when an attached route starts. Slopbox does not export the password, change Keychain permissions, or grant guests Keychain services. Inspection does not read the item. Keychain sources fail explicitly on other platforms.

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

A relative SOPS file path is resolved against the workspace associated with the route that uses it. The SOPS executable and identity stay host-side. Decrypted values are retained only in supervisor memory. Linux accepts Nix-store helpers or root-owned system executables whose complete ancestry is not group/world-writable; a selected guest executable is not thereby a host helper. macOS accepts Nix-store and recognized Homebrew Cellar executables, rejecting installations that overlap the workspace. No SOPS identity or additional filesystem grant is passed to either sandbox role.

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

A route with `workspace` activates only for that canonical workspace, unless an explicit host account selection excludes it. Routes without `workspace` are dormant until selected through host defaults or directory rules. The guest receives a local base URL and route name, never the real secret.

Routes fix the upstream origin, base path, methods, and authentication scheme. Slopbox strips guest authentication, does not follow redirects, rejects unsafe traversal, pins DNS resolution, requests identity encoding, and redacts exact reflected secret bytes. Transformed secret reflection remains outside the guarantee.

### Shared host defaults and directory rules

Define reusable identities and accounts once in host-owned `config.toml`:

```toml
[defaults]
git_identity = "agent"
accounts = ["forge"]

[[git.identities]]
id = "agent"
name = "Agent"
email = "agent@example.com"
signing_key_fingerprint = "SHA256:REPLACE_WITH_APPROVED_FINGERPRINT"

[secrets.forge]
source = "environment"
variable = "FORGE_TOKEN"

[[http_routes]]
name = "forge"
upstream = "https://forge.example.com/api"
methods = ["GET", "POST"]
proxy = true
authentication = { type = "bearer", secret = "forge" }

[[workspaces]]
paths = ["~/Projects/untrusted", "~/Downloads"]
git_identity = false
accounts = []
```

Both `~/Projects/first` and `~/Work/second` select the same definitions without repository configuration. Directory rules match canonical directory trees and apply broadest first, regardless of declaration order. An omitted setting inherits; `git_identity = false` disables signing and `accounts = []` disables accounts. Account lists replace rather than append. Equally specific overlapping rules fail closed.

Existing `workspace` bindings remain exact-workspace ceilings even when selected by name. Without an explicit selection, legacy workspace-bound entries still activate; unbound entries do not. Unknown selections fail before resolving secrets. Repository `.slopbox.toml` cannot define these grants. `slopbox status --verbose` shows selected access, matching host rule indexes, and each selection's source (defaults, directory rule or legacy binding) without resolving secrets. A configured `default_command` uses ordinary execution without project initialization; bare launch requires a configured `default_command`.

### Experimental shared HTTPS transport

`proxy = true` opts a route into TLS mediation; existing routes remain unchanged. Tools receive `SLOPBOX_ACCOUNT_PROXY` and a read-only public `SLOPBOX_ACCOUNT_CA`. Host trust is unchanged, and clients must explicitly trust the session CA. The ordinary proxy also mediates these origins before general-egress handling; other destinations retain normal approval checks. There is no raw-tunnel fallback for a mediated origin. Clients can use their normal proxy settings, or select the account-only endpoint explicitly. For example:

```bash
curl --proxy "$SLOPBOX_ACCOUNT_PROXY" --noproxy '' \
  --cacert "$SLOPBOX_ACCOUNT_CA" https://forge.example.com/api/user
```

This explicit account proxy accepts only configured mediated origins; it does not provide general egress. CONNECT authority, TLS server name, and HTTP Host must agree. The existing route checks still enforce paths, methods, DNS/address restrictions, upstream TLS verification, authentication replacement and exact-byte redaction. Redirects are returned but never followed by the broker; a client's next request needs its own approved route. Only HTTP/1.1 requests with bounded Content-Length bodies are supported, not chunked uploads. Client certificate pinning and clients that ignore proxy/trust settings are outside this experiment.

The public CA expires after one day and is unique to a session. Private certificate keys remain host-side; no reusable CA signing service survives setup. See [POC scope and validation](poc-generic-capabilities.md).

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

An account route can opt into origin-addressed requests with `direct = true`. Slopbox generates private `gh` transport settings using its documented `http_unix_socket` option. Tool processes receive a synthetic token, not the host login. Unattached processes do not receive the `GH_TOKEN` marker. The socket matches the request's host and path against opted-in routes; overlapping routes fail at launch. This is brokered HTTP over a local socket, not direct Internet access or TLS interception.

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

The approval hotkey is separate and opt-in. Without `--approval-view`, Ctrl-] reaches the selected command normally. Host broker diagnostics are buffered and escaped rather than interleaved with the guest display.

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

Every mutation displays the full destination and scope, then requires a fresh 12-character confirmation code. A wrong response cancels; queued input is discarded at view transitions. Commands and confirmation input are not forwarded to the guest. Approval says “retry”; it does not replay the operation. The guest continues executing while the view is open, though its terminal output is paused and may back up.

As with ordinary interactive launches, the supervisor gives the guest a separate PTY and retains the real terminal. Ctrl-Z in the guest view suspends the supervisor and its foreground sandbox process group; use the shell's `fg` to resume. Normal exit and handled termination signals restore host terminal settings. SIGKILL cannot run cleanup.

This is not a screen-preserving terminal multiplexer: returning clears the view and briefly resizes the guest PTY to request a redraw. Guest redraw, suspension, termination, and approvals are tested through PTYs, but visual behavior across terminal emulators still needs manual validation. Resize the window if rendering remains stale, or relaunch without `--approval-view`. Broker diagnostics are isolated from the approval display; their last 16 KiB are escaped and printed when the terminal session ends.

## Concurrent sessions

Each run has separate generated configuration and broker lifetimes. Private application home and package caches persist per workspace; live sessions share that workspace and its state. They are not isolated from each other. Use separate workspaces or staging to separate edits.

## Inspecting effective configuration

```bash
slopbox status
slopbox status --verbose
slopbox status --verbose
slopbox policy
slopbox run --dry-run -- /bin/sh
```

`status` explains the next launch's project writes, readable host resources, general and tool networking, configured model providers, account routes, Git mappings, signing identity, and isolation limits. Use the same profile and resource flags you intend to launch with. `--verbose` adds policy axes, mount targets, trusted extension paths, and configuration/state locations.

Inspection does not decrypt secrets, load signing keys, evaluate Nix environments, contact providers, or create project state. Provider entries report configuration presence, not valid credentials. This is a launch-policy summary, not a claim about the state or readiness of an already-running session.

`policy` remains available for raw policy axes, route names, Git URL mappings, and identity. `--dry-run` prints the native backend command without starting it. The default-environment row describes automatic launch. An explicit runtime selection disables implicit flake activation. Otherwise, a project `flake.nix` is evaluated and built on the host before its environment is activated in the sandbox.

### Diagnose launch problems

Run from a host terminal, not from the agent's shell:

```bash
slopbox doctor
slopbox doctor --workspace /path/to/project --profile contained
slopbox doctor
```

`doctor` checks generic launch prerequisites and performs bounded enforcement probes without executing the selected application, reading secret contents, evaluating Nix, or contacting providers. Passing does not establish credential validity, signing, upstream connectivity, Nix realization or arbitrary client compatibility.

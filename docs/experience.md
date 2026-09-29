# User experience

> Historical design/validation record. Built-in Pi launch and integration have since been removed; use [current configuration](configuration.md) and the [security model](../SECURITY-MODEL.md) for supported behavior.

Slopbox should let someone run a coding agent without learning namespaces, credential brokers, or network proxies. The [authoritative project direction](direction.md) governs this experience; current per-project setup is not the target onboarding requirement.

The primary product promise is:

> Run your coding agent here. Understand its access, approve what it needs, and stay in control of the result.

The supported workflow is NixOS with Pi. The next targets are standard Linux, Apple silicon macOS, and additional harnesses without weakening the host-owned security boundary. This document describes the intended experience; [configuration.md](configuration.md) documents the current interface. Default Pi launch, first-run confirmation, `init`, `status`, `doctor`, revocable `network` rules, and explicit Git URL rewriting are implemented. An opt-in host approval view is prototyped; broader account setup and additional harnesses are still planned.

## Mental model

The default interface should answer four questions:

1. What files can the agent read or change?
2. Where can its tools connect?
3. Which external accounts and identity can it use?
4. Will changes happen immediately or wait for review?

Show concrete access rather than policy terminology. For example, a configured developer session might show:

```text
Project       ~/Projects/example — edits applied immediately
Other files   Host Nix store and selected Pi resources are readable
Tool internet Ask before new destinations
Accounts      Forgejo bot — repository route for org/example
Signing       Commits signed as bot
Model         OpenAI Codex
Credentials   Kept outside the agent
```

This summary must be generated from effective grants, not static profile descriptions. Additional resource mounts, broad API routes, and trusted extensions must be visible. Account access also applies to project commands that receive the route; keeping a token secret does not prevent use of its authority.

Distinguish general tool internet, model access, and authenticated account routes. Do not label a session “offline” or “no internet” when one of those paths remains available. Likewise, “can edit only this project” must not imply that no other host data is readable. The current `network=none` and `--tool-network=none` settings disable general egress, not configured authenticated account routes.

Implementation details belong in verbose diagnostics. Profiles, accounts, identities, harnesses, and grants are useful internal concepts, not prerequisites for starting a session.

## Current first-run setup

The normal command is:

```bash
cd project
slopbox
```

When the folder has no saved host-side setup, select a workspace mode and confirm the access summary:

```text
Configure /path/to/project for Pi.
How should changes work?
  1. Work directly in this folder; host tools see edits immediately (default)
  2. Keep changes separate for review
  3. Read only
Choice:
```

Only choices permitted by host and project policy are offered. Pi is the only supported default harness, so there is no agent picker yet. Add one when another adapter is validated, and reuse the previous choice. Explain that direct changes are immediately visible to host editors, watchers, and build tools. Separate changes require review and application before they reach the host workspace.

Recommend deny-by-default general networking with host approval for new destinations. Show the resulting access summary before launch and require confirmation for sensitive grants. If model access is missing, guide the user through only the account connection needed to start their chosen agent.

Source-control accounts, signing identities, custom mounts, and other integrations are optional follow-up tasks, not first-run requirements. Do not silently import arbitrary host resources or attach unrelated accounts.

Setup saves the confirmed policy axes outside the repository. Later project-file changes cannot exceed that saved ceiling. Re-running `init` explicitly replaces it after review. Noninteractive setup requires `--changes` and `--yes`; otherwise confirmation must come from a host terminal. Missing model access is currently handled with host-side login instructions rather than an embedded account wizard.

## Intended command surface

The everyday commands should stay small:

```text
slopbox                 Start the configured agent here
slopbox status          Explain effective access
slopbox network         Inspect requests and manage approvals
slopbox changes         Review and apply staged changes
slopbox doctor          Diagnose a failed setup or launch
```

Supporting commands remain available without requiring a separate setup sequence before first launch:

```text
slopbox setup           Configure host defaults
slopbox init            Configure or reconfigure this project
slopbox accounts        Connect and manage external accounts
slopbox run             Run another configured agent or command
```

Command surface:

| Current | Intended |
|---|---|
| `policy` | `status --verbose` (implemented; `policy` retained) |
| Host network management | `network` subcommands; opt-in `--approval-view` prototype |
| Guest denial lookup | `denials` (read-only) |
| `stage` | `changes` |
| `auth` | `accounts` |
| `tool-run` | hidden harness-internal command |

CLI parity remains important for automation:

```bash
slopbox network events --follow
slopbox network approve <request-id> --session
slopbox network approvals
slopbox network revoke <rule-id>
slopbox changes diff
slopbox changes apply
slopbox status --verbose
```

## Approval workflow

Optimize the interruption, not the dashboard:

> A command fails → see what was blocked → approve if appropriate → retry.

`slopbox --approval-view` prototypes this flow: Ctrl-] opens a host-owned snapshot view without a second terminal. The supervisor retains the real terminal and gives the guest a private PTY. Session approval, explicit project persistence, and revocation each require a fresh confirmation code; no approval input is forwarded to the guest. The separate host CLI remains available for automation and remote sessions. Keep the view opt-in until real-terminal feedback validates the display transitions.

Show the project, session, exact normalized hostname and port, requested method where known, denial reason, and approval scope. Explain that general HTTPS approval permits a destination, not a particular URL or safe use of its contents. HTTPS paths and methods inside CONNECT tunnels are not visible.

Offer session approval as the recommended scope and project approval as an explicit persistent choice. Rules need stable IDs, timestamps, and revocation. Group related denials for inspection, but do not treat a dependency download or browser request group as blanket authorization for every destination it contacts.

A denied operation remains failed. Say “approved; retry the operation.” Do not automatically replay shell commands or HTTP requests, which may have side effects.

Guests can read denial events but cannot mutate policy. Guest-provided explanations are context, not evidence that a grant is safe. Commands that bypass the proxy may fail without a recorded request; diagnostics should distinguish that from an approvable denial.

### Later network controls

Start with request inspection, session/project approval, and revocation. A larger traffic dashboard, once-only grants, expiry controls, and global approvals are follow-up work, not prerequisites for a usable sandbox.

If global approvals are added, they must use exact hostname and port rules, explicit profile eligibility, stable IDs, audit records, and revocation. They create cross-project exfiltration channels and must not be an implicit convenience default. Projects may disable global rules but cannot enable them. No approval may override private-address or checked-DNS restrictions or confer model or authenticated-route authority.

## Review and recovery

Staged changes are a core workflow, not a backend feature to polish later.

`slopbox changes` should select the relevant stage when unambiguous and ask when several exist. It should make the diff, application status, and retained work easy to find. Applying changes must detect host conflicts, avoid silent overwrites, and recover from interruption without leaving unexplained partial results. Review does not make changed hooks or build scripts safe to execute.

Cancellation, normal exit, rendering failures, and crashes must leave sessions and staged work recoverable. Explain what was retained and how to resume or review it. Reliable whole-stage application comes before selective application and advanced stage management.

## Configuration ergonomics

Follow the [global-defaults configuration direction](direction.md#global-defaults-local-restrictions): define identities and accounts once, reuse host defaults across projects, and add workspace restrictions only where needed. An ordinary new repository should normally need no setup or configuration file.

Do not replace today's repetitive bindings with per-forge templates or mandatory project entries. A wizard cannot substitute for a reusable configuration model. Keep backend details out of ordinary configuration and expose effective grants and their sources through inspection. No replacement TOML schema is specified yet; [configuration.md](configuration.md) documents the current interface.

### Configuration authority

- `.slopbox.toml` remains shareable and narrowing-only.
- Host grants live outside the repository; setup and approval run in the host control plane.
- Mutable approvals live in state rather than static configuration.
- Secrets use host-side storage such as SOPS, Keychain, or Secret Service.
- Integrations may propose grants but cannot approve them.

For example, a known browser extension may need runtime data:

```text
Camoufox needs a disposable cache directory.
Allow ~/.cache/camoufox as a temporary overlay for Pi? [y/N]
```

Explain that the existing cache contents become readable and sandbox writes are discarded. Do not silently import the directory.

## Errors and diagnostics

Errors should explain the failed user goal and the next action:

```text
Git signing is configured, but the selected key is not loaded in the host SSH agent.
Expected: SHA256:...
Run `ssh-add -l` on the host, load the key, and restart Slopbox.
```

Keep raw kernel limits, mount flags, and namespace setup under `--verbose`.

`slopbox doctor` should check the selected backend, project filesystem, runtime and harness availability, required secret resolvers, account connectivity, signing-key availability, and stale sessions or stages. Unsupported combinations should fail with an explanation and a supported alternative, never a silent fallback.

Beginner mode shows consequences and recommendations. Advanced mode exposes policy axes, generated routes, mount sources, runtime closures, and backend diagnostics. Security must not depend on hiding complexity.

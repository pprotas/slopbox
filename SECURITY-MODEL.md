# Slopbox security model

This document describes the current implementation, its actual guarantees, known gaps, and the next security milestones. It distinguishes manually tested behavior from stronger claims that would require a separate kernel or formal review. Product terms such as identity, account, harness, project, session, and profile are defined in [docs/concepts.md](docs/concepts.md).

## Current threat model

The native Linux backend is intended to contain:

- Accidental agent actions outside the project.
- Prompt-injected shell commands using ordinary userspace interfaces.
- Tools inspecting ambient environment variables or common credential paths.
- Ordinary malicious package scripts that do not exploit the Linux kernel, subject to the authority gaps below.
- Accidental access to host services and private network addresses.

It is not currently intended to contain:

- Linux kernel or bubblewrap vulnerabilities.
- Deliberate exploitation of user namespaces or filesystem implementations.
- Denial of service through CPU, memory, process, or disk exhaustion.
- Damage within a live workspace.
- Data sent intentionally to an approved destination.
- Use of an attached credential's authority through its brokered route.

Supply-chain protection requires treating build and dependency code as potentially malicious userspace code. This is stronger than the original accidental-agent threat model and drives the next milestones.

## Assurance profiles

Profiles separate policy from enforcement backend. The native `developer` and `contained` profiles are implemented; `adversarial` remains planned:

- `developer`: live workspace, native namespaces, host/project tools, optional trusted harness resources, credential brokering, and configurable logged egress.
- `contained`: staged workspace, project-declared runtime, no trusted extension code, and allowlisted or disabled general egress. It still shares the host kernel when using the native backend.
- `adversarial`: staged workspace and isolated guest runtime in a microVM, no host harness resources or ambient host tools, strict resource limits, and only fixed broker routes plus explicit minimal egress.

Profiles are presets with contracts, not a single “strictness” slider. Workspace, network, runtime, harness, persistence, credential, and backend policy remain distinct axes. Invalid combinations must be rejected rather than silently weakening a profile.

Host-owned global configuration is authoritative. Project configuration is untrusted: it may describe an environment and request capabilities, but cannot grant them. Session approvals are host control-plane decisions. The pure policy core represents these presets and restrictive merge semantics, and `slopbox policy` displays the effective host policy. Developer and contained have native enforcement implementations; `slopbox run` rejects adversarial policies rather than silently weakening them. A project-local `.slopbox.toml` may request a more restrictive policy; restrictive merge semantics prevent it from expanding the host grant. Unknown fields and symlinked manifests are rejected. Unsupported image-runtime, observe-network, ephemeral-persistence, or microVM combinations are rejected at launch.

Bare `slopbox` launches Pi using a host-owned setup record bound to the canonical workspace. First-run setup happens before sandbox creation, displays the effective access, and requires an explicit terminal confirmation or `init --changes ... --yes`. The saved policy axes form an additional ceiling for default launch, explicit `run`, and inspection. Removing or weakening a repository manifest cannot exceed that saved ceiling. Re-running host-side `init` explicitly replaces it after review; global route, identity, and resource declarations remain host-controlled rather than frozen by the record.

Setup never writes repository policy or modifies the global configuration. Generated records use private permissions and atomic replacement under a per-project lock; a concurrent setup change invalidates an outstanding confirmation. Symlinked setup directories/files and records naming another workspace are rejected. Setup display text escapes terminal controls. The advanced `run` interface remains available without initialization and retains its existing model-authority semantics.

A remote model endpoint is itself an intentional data channel. Any prompt, tool result, or workspace context sent to an untrusted provider is disclosed to that provider. Isolation can prevent access to unrelated host data and additional egress; it cannot conceal context deliberately sent for inference.

## Current architecture

```text
host Slopbox supervisor
├── bubblewrap process sandbox
├── project and session state outside the workspace
├── per-run general HTTP/HTTPS gateway
├── fixed OpenRouter and OpenAI Codex reverse-proxy routes
├── host-selected authenticated HTTP routes
├── Host command/SOPS/environment secret resolution
├── fingerprint-selected Git SSH-signing broker
├── real OpenRouter key in host memory
└── host-only OpenAI Codex OAuth store and refresh logic

bubblewrap sandbox
├── Pi and explicitly loaded trusted extensions
├── nested tool sandbox for shell and build commands
├── live, read-only, or private staged workspace
├── private persistent agent and tool homes
├── read-only host store or selected project/runtime closure
├── private PID and network namespaces
├── general proxy on 127.0.0.1:39080 → general/gateway.sock
├── model proxy on 127.0.0.1:39081 → model/gateway.sock
├── authenticated HTTP proxy on 127.0.0.1:39082
├── narrow Git signing helper without SSH-agent access
├── synthetic OPENROUTER_API_KEY
└── synthetic OpenAI Codex JWT marker
```

General egress and authenticated model traffic use separate host Unix sockets and sandbox-local ports. In `allowlist` mode both sockets are available to the outer sandbox when credentials are brokered. Network mode `none` removes the general socket, forwarder, and proxy environment while fixed model routes remain available according to credential policy. Credential mode `none` independently removes the model socket, forwarder, and provider markers. Combining both removes general and model access, but configured authenticated HTTP routes remain available. Inner tool execution never receives the model socket; it may receive the general socket and configured authenticated HTTP routes. Neither `network=none` nor `--tool-network=none` alone means fully offline execution.

## Native macOS project environments

The native backend selects a project's default Nix dev shell on the host, captures its environment as JSON and queries its closure. Nix evaluation, dependency realization and builders remain trusted host preparation: review the flake and inputs before launch. Shell activation and `shellHook` run only after applying the tool role's Seatbelt profile, once per tool request; neither the host nor Pi sources them.

Only tools receive the selected closure's read-only grants. `runtime=host` retains recognized host installations as fallback; `runtime=project` omits those installations while retaining the native system base and explicitly selected Node. Neither mode exposes the whole store, Nix daemon or host profiles. Captured home/cache, loader, Git and broker-control variables are filtered. Project hooks still have all already-granted tool authority, including account routes.

Each Nix profile root lives in the native session's control directory. Successful task cleanup removes it; uncertain native recovery retains it with the journal. Local preparation/activation/profile tests pass. Pawel reports the Nix build and compiler/linker/enforcement fixture passing on the host. A fresh actual Pi session independently ran the focused tests, formatting and strict Clippy using its selected Nix toolchain, and exercised brokered Git reads. See [native setup and validation](docs/macos.md#project-nix-environments).

## Native macOS generic commands

Host-owned `[runtime].executables` also supports generic native commands without Pi configuration. This requires `runtime=host`, `harness=none` and no project activation. Discovery validates native Mach-O/system-library dependencies and simple bash/sh scripts without executing them. Bundles, dependency roots and non-system dylibs are rejected. The existing launchd/coalition ownership, broker leases, descriptor handling and cleanup remain in use.

Selected files receive literal read and execution grants, with ancestor metadata only. Execution permission is enforced separately from reading: an unreadable but unselected executable must not become executable. Workspace/private-home code may execute. The existing native system-library/locale base remains available, with protected system ICU data files granted literally and read-only. Workspace/credential/control overlaps, non-system hard-link aliases and setuid/setgid selections are rejected; host installation stability remains a prerequisite.

Generic sessions use a disposable private home and temporary storage. Named application Unix IPC is not enabled: a home-prefix grant was found unsafe when a workspace socket was moved into that home. Host-socket symlink, hard-link and rename probes remain denied; only explicitly attached broker sockets are available. Signals are limited to the same sandbox. No Keychain/Mach service or shared temporary-directory access is added. Seatbelt is not a private filesystem/PID namespace: denied file reads do not imply hidden path existence or all host metadata.

All generic subprocesses retain outer account/model authority. There is no automatic native tool adapter or generic `tool-run` role; Pi keeps its existing separate tool role. Production-launcher enforcement and unmodified Claude Code Read/Edit/Bash/streaming fixtures pass locally with disposable credentials. These results do not establish live subscription/provider compatibility, interactive Claude support or full Linux parity. See [native runtime limits and tests](docs/poc-native-runtime.md).

## Filesystem boundary

### Exposed

- Current workspace using the effective live, read-only, or private staged mode.
- Per-project private home for Pi sessions and a separate private tool home.
- Trusted Pi extension and wrapper, read-only. Pi's separate private settings and synthetic authentication state remain writable.
- A generated read-only copy of host `~/.pi/agent/AGENTS.md`, when present, and an allowlisted copy of Pi settings.
- Selected global Pi resources according to the effective `trusted`, `data`, or `none` harness mode.
- Nix-backed developer: entire host `/nix/store` and, when present, a Nix-store-backed NixOS system profile, read-only, `nosuid`, and `nodev`.
- Contained: only the queried closure of the project development profile and required Slopbox, Pi, shell, loader, certificate, and bubblewrap runtime roots.
- Opt-in selected-executable runtime: individual native ELF/script dependencies, the host loader cache and generated command aliases. Optional host-owned `runtime.bundles` grants dedicated application trees read-only, including their code and data. No implicit whole-prefix, library-directory or home import.
- Selected read-only `/etc` files and certificate directories.
- Private `/proc`, `/dev`, and `/tmp`.
- Slopbox runtime executable and gateway socket directory.
- Nix development-environment activation script when applicable.

### Not exposed

- Host home except for the directory chain needed to reach the workspace and explicitly selected runtime files/bundles or Pi resources described below.
- Sibling projects.
- Host `/tmp`.
- Host `/run/user/$UID`.
- SSH, GPG, Docker, Podman, Nix daemon, D-Bus, systemd, journald, display, and browser sockets.
- Host Pi authentication files.
- Slopbox policy, approval, and credential state.

### Workspace validation

Slopbox rejects:

- `/` as a workspace.
- The host home or an ancestor containing it.
- A workspace containing Slopbox configuration or state.
- Linux workspaces overlapping `/nix/store`, which would undermine the read-only runtime grant.
- Linux workspaces overlapping the private home or Slopbox guest control paths. Selected runtime files cannot overlap the workspace.
- Unexpected existing submounts.
- Unix sockets visible during startup.

Mount propagation is private. The generated Linux root filesystem is remounted read-only, protecting runtime aliases; separate workspace, private home and tmp mounts retain their intended writability. Unintended inherited file descriptors are closed before execution.

### Pi configuration import

Slopbox treats host `~/.pi/agent/AGENTS.md` as an explicit prompt-data grant. It copies at most 1 MiB into generated Slopbox state and mounts that copy read-only at the private Pi agent path. Its contents are visible to Pi and may therefore be sent to the configured model provider.

Host `settings.json` is parsed outside the sandbox and reconstructed from an explicit allowlist of model, thinking, UI, retry, compaction, tool-selection, terminal/image, model-cycling, and Markdown settings. Unknown fields and settings with helper-command, network-routing, or state-path semantics are discarded. Slopbox does not import `externalEditor`, shell commands, package-manager commands, proxy configuration, or session paths. It overrides project trust to `never` and Codex transport to `sse`.

In `trusted` harness mode, global conventional extension, skill, prompt, theme, and helper-bin directories are mounted read-only at dedicated guest paths and compatible private-home paths. Installed npm package trees named by the host `packages` setting are also mounted read-only, and their declared resources are passed explicitly to Pi. The complete npm dependency tree is exposed because JavaScript package resolution may require sibling dependencies.

In `data` harness mode, Slopbox imports filtered settings, `AGENTS.md`, skills, prompts, and themes, but excludes host extensions, npm packages, helper binaries, and configured extension runtime mounts. These data resources can still influence model behavior and tool selection; “data” means they do not execute in Pi's process, not that their contents are inert or secret. `none` imports no host Pi resources.

Additional extension runtime data requires an explicit `pi.read_only_mounts` or `pi.temporary_overlay_mounts` entry in host-owned `~/.config/slopbox/config.toml`. Read-only mounts cannot be changed. Temporary overlays use the host tree as a read-only lower layer while keeping guest writes in session-local tmpfs that is discarded on exit. Targets are restricted to the sandbox cache, config, or data home. Slopbox rejects known SSH, GPG, password-store, Pi-auth, and Slopbox-credential roots, as well as mounted resource trees containing pathname Unix sockets. This is an explicit read grant: files under an approved resource tree are visible to Pi and trusted extensions. Configured git packages, local package paths, and arbitrary resource paths are not yet supported. Host `auth.json`, sessions, trust decisions, and unrelated host-agent files are not mounted.

Pi requires writable settings and auth-state paths. Slopbox generates separate files for each run and bind-mounts them at the private Pi agent paths. Settings are initialized from filtered host settings and auth state starts empty; neither contains a host credential. Pi currently updates these files in place. History and model-catalog state remain in the project-persistent agent directory, including their shared locking paths. The trusted wrapper disables automatic extension and resource discovery, explicitly loads the read-only Slopbox adapter and host resources, and reports each trusted extension at startup. `--no-host-pi-resources` disables host settings, `AGENTS.md`, and additional host resources for a session.

Trusted host extensions execute inside Pi's outer process. They can read the mounted workspace and modify it when the effective mode is live or staged, call the model broker, use approved general egress, inspect Pi's private session state, and spawn processes without entering `tool-run`. Their subprocesses inherit the general proxy when enabled and the provider marker environment, but not real credentials. Extension egress remains deny-by-default and follows the normal event/approval/retry flow. They cannot directly access the real provider credential, host home outside the explicit resource mounts, host service sockets, or unrestricted network. This is an accepted compatibility boundary, not an isolation guarantee between Pi and its trusted extensions.

### Explicit filesystem exceptions

In the default Nix-backed developer runtime, the whole Nix store is readable and may contain source copies from unrelated flakes. Read-only access prevents modification, not disclosure. Its sandbox PATH includes host PATH entries that canonically resolve into the store. An existing `/run/current-system/sw` is mounted read-only only if it resolves into the store; it is not required. These binaries do not inherit host filesystem, credential, service-socket, or network authority, but they expose ambient host tooling, reduce reproducibility, and increase parser and shared-kernel attack surface.

The Nix-backed runtime constructs guest `/bin/sh` and `/usr/bin/env` links from selected Nix executables, not the host distribution's binaries. Nix helpers require canonical-store PATH directories and executable regular files; mutable non-store bin directories cannot supply Nix aliases. Linux host helpers also accept root-owned system executables with non-group/world-writable ancestry, including the unresolved lookup path. Merely selecting a guest executable does not authorize it as a host helper. Flake and closure operations explicitly enable the required Nix features in their own process; they do not modify host Nix configuration. A stock Ubuntu/OrbStack installation with single-user Nix passed the full enforcement suite without changing distro executables or creating a NixOS system profile. Hosts still need usable outer and nested user namespaces; Slopbox does not bypass host restrictions or fall back to unsandboxed execution. The separate opt-in Nix-free runtime is described below.

The opt-in `[runtime]` selection discovers native ELF dependencies and simple script interpreters without running selected programs or `ldd`. Only explicitly selected files, protected root-owned installations and host-declared dependency prefixes are eligible. Metadata is not permission to import arbitrary host files. Prefixes authorize discovered files, not directory mounts; known credential/control roots, workspace overlaps, private guest paths and user-owned hard links are rejected. The host loader cache exposes installed library path metadata, not those libraries' contents unless selected. Runtime files must remain stable during a session; discovery is not an atomic snapshot.

Optional host-owned `runtime.bundles` explicitly grants whole application trees as read-only code and data. Bundle selection is distinct from dependency-root authorization: every file in a bundle is readable, not only inferred dependencies. Startup traversal rejects known credential/control and workspace overlaps, private guest paths, broad system roots, mutable hard links, special files and submounts. Data/directory symlinks cannot expand the grant; native ELF links use the same dependency authorization as ELF metadata. Native objects are inspected without execution, including libraries referenced by plugins. Bundled filenames/SONAMEs may supply already-granted dependencies whose caller's loader paths cannot be inferred; this does not change the guest loader's search paths or promise complete dynamic linking. Nothing imports package-manager caches or follows package manifests to authorize additional roots. Keep secrets out of approved installations. Tree validation is startup-only: host changes, including later socket creation, are not prevented. See [bundle acceptance and limitations](docs/poc-runtime-bundles.md).

This mode requires a non-root host user, `runtime=host` and `harness=none`. Integrated Pi launch/setup is rejected rather than downgraded. Explicit generic commands have their outer role's authority; cooperative `tool-run` provides the existing stronger model/tool boundary. Inner execution uses an immutable session bubblewrap entry, not a caller-controlled PATH lookup. Application resources outside explicit bundles and additional subprocess entry points are not inferred; missing resources remain unavailable. An unmodified Aider fixture validates package resources, mediated model requests and edits across workspaces, and confirms that its test subprocess retains fixed-model-broker access until explicitly entering `tool-run`. A [native Claude Code fixture](docs/poc-claude-code.md) validates streamed Anthropic-compatible requests and built-in Read/Edit/Bash tools through the same account transport, without Linux core changes or real provider access. These account-scoped inference routes remain available to tools; this is not inference isolation for either harness or an automatic harness/tool adapter. See [limits and Nix-free enforcement tests](docs/poc-nixless-linux.md).

The contained profile requires an activated flake development environment. The host queries its requisites together with required Slopbox, Pi, shell, loader, certificate, and nested-bubblewrap roots, mounts only those top-level store paths, filters the inherited PATH to mounted roots, and does not mount `/run/current-system/sw`. The selected closure can still contain project source, compilers, interpreters, or other build inputs; closure restriction reduces ambient disclosure and tooling but does not make declared dependencies trustworthy.

The workspace socket scan is not a lasting guarantee. A host process can create a pathname Unix socket inside a live workspace after startup. Linux network namespaces do not isolate pathname Unix sockets reachable through a shared filesystem.

The live workspace is a cross-boundary trust gap. Sandbox code can plant or modify:

- `.cargo/config.toml` and `build.rs`.
- Git hooks.
- Package-manager lifecycle scripts.
- Shell and direnv hooks.
- Editor and language-server configuration.

A host editor, watcher, test command, or build may execute such content before review. Read-only and staged modes avoid direct sandbox writes to the host workspace. Current staged mode attempts copy-on-write cloning and falls back to byte copying. It creates a baseline that is not mounted into the sandbox, followed by a writable copy. The initial host-side snapshot is non-atomic, so concurrent host changes can produce an inconsistent snapshot. It preserves symlinks but not hard links, timestamps, or extended attributes. Host commands can list, diff, apply, and discard retained stages. Whole-stage apply first verifies that the host workspace still matches the baseline and refuses conflicts. Apply does not follow staged symlinks while copying, but it is non-transactional and does not lock out concurrent host writers between verification and each update.

### Concurrent session lifetime

Generated Pi resources, copied `AGENTS.md`, writable settings and synthetic auth state, launch wrappers, Git signing configuration, and Nix development-environment files live in a unique host-owned directory per run. The supervisor retains it until the sandbox exits and removes only that directory on normal exit or setup failure. Startup never resets another run's mounted configuration directory. Persistent mount targets are created without truncating or replacing existing regular files.

Project homes, caches, Pi history, and live workspace files remain shared within the project. This is not isolation between mutually untrusted sessions, protection against concurrent workspace edits, or a guarantee that the same Pi conversation can be resumed concurrently. Staged workspaces keep agent edits separate, subject to the apply limitations above. Abrupt supervisor termination can leave runtime directories requiring later cleanup.

## Process boundary

The current runner uses bubblewrap with:

- User, PID, IPC, UTS, and network namespaces.
- All capability sets dropped.
- `no_new_privs` enabled.
- A private process view.
- No host process descriptors inherited.

The boundary assumes an unprivileged invoking host user. Namespace UID numbers vary; an inner `uid_map` can include intermediate UID 0, but sandbox processes map back to that invoking user in the initial host namespace. The original NixOS audit used UID 1000. The selected-executable runtime explicitly rejects host UID 0; running Slopbox as host root is not a qualified security configuration.

Creating another nested user namespace currently works and grants capabilities scoped to that new namespace. `slopbox tool-run` intentionally uses this facility for an inner tool sandbox. Tests found no way to use nested capabilities to remount parent filesystems or access host devices. This still increases shared-kernel attack surface and is not a guarantee against kernel vulnerabilities.

The inner tool sandbox has a separate persistent home, overlays the outer agent home, hides trusted Pi configuration, creates another network namespace, and can expose the general gateway without exposing the model gateway. A trusted read-only Pi extension routes the built-in `bash` tool and user `!` commands through general `tool-run` by default. General egress remains deny-by-default and produces approvable events. `slopbox run --tool-network=none` disables general shell egress but retains configured authenticated HTTP routes.

## Network boundary

The sandbox has loopback but no direct external interface or route. Applications use sandbox-local TCP forwarders, which connect to separate host general and model gateways through mounted Unix sockets. Network mode `none` omits the general socket and forwarder and disables general Pi shell egress; it does not disable separately governed fixed model or authenticated HTTP routes. Credential mode `none` omits model routes and synthetic provider markers.

In an online `developer` session, Pi may refresh its remote model catalog through the general gateway. A first request to a catalog destination such as `pi.dev:443` is denied and recorded until the host approves it; reopening `/model` or running `pi update --models` retries the request. Catalog state is cached in the project-private Pi home. Stricter profiles set `PI_OFFLINE=1`, so model availability is limited to bundled or deliberately supplied metadata even when separate model inference routes remain enabled.

Host configuration may define named secrets and authenticated HTTP routes selected by shared host defaults and canonical directory rules, or by legacy exact-workspace bindings. Unbound definitions are dormant unless selected. Directory rules can replace account selections or disable signing/accounts; repository configuration cannot grant them. Credential commands and SOPS run only on the host through trusted packaged helpers or, on Linux, protected root-owned system executables. Command arguments are literal, ambient credentials are not inherited, stdout is bounded, and stderr/failed output are not logged. Resolved credentials stay in supervisor memory; host login stores, SOPS identities, plaintext secrets, and real authorization headers are not mounted or exported. Each route fixes an HTTPS upstream base, method allowlist and authentication scheme. An optional canonical workspace binding remains a ceiling even when selected through host defaults. The guest receives a local route name and synthetic base URL. Slopbox does not follow redirects or inherit host proxies. It rejects unsafe encoded path traversal, non-public upstream addresses unless explicitly enabled by the host route, and encoded responses. Guest authentication is replaced and exact reflected secret bytes are redacted. Routes are available to shell/build code by design, so arbitrary project code can exercise the complete authority granted by that route and upstream account. Repository permissions and branch protection remain essential.

Experimental `proxy = true` routes accept HTTPS CONNECT through the account broker, using session-local public trust and preissued origin certificates whose private keys stay host-side. The ephemeral CA signing key is discarded after certificate issuance. CONNECT authority, TLS server name and HTTP Host must agree; decrypted HTTP/1.1 requests reuse the existing route checks. Unknown origins, duplicate Host/Content-Length fields, Transfer-Encoding and unsafe paths fail closed. Tools opt in through explicit proxy/trust settings; the host trust store and general proxy are unchanged. Local protocol tests cover verified upstream TLS, authentication replacement, redaction, authority substitution, framing and curl/Node interoperability. Native macOS and Linux fixtures separately verify read-only trust, host-config/direct-network denial and tool environment separation across two workspaces. The Linux fixture uses a verified local HTTPS upstream, verifies signed commits from one shared identity in both workspaces, and checks a directory rule that disables accounts/signing in a third. The native fixture uses the existing test-only HTTP pin upstream; protocol tests separately verify upstream TLS. These tests use disposable credentials and do not establish arbitrary-client compatibility.

Opted-in origin-addressed routes share these checks through a private Unix socket. Stock `gh` receives generated transport configuration and synthetic authentication in the tool role, not the host login. Host/path matching must select exactly one configured route. GraphQL authority cannot be narrowed to a repository by an HTTP path; a GraphQL route needs appropriately restricted provider-side credentials.

A host route may list `git_urls` to generate session-private Git `url.*.insteadOf` rules. Both the outer process and inner tools read the generated configuration; repository remotes are not modified, SSH agents are not forwarded, and no Git signing identity is required. These are Git prefix rewrites for convenience, not an additional security boundary. Fixed upstream routes, method restrictions, and provider permissions still determine authority. Project Git configuration may interfere with rewriting but cannot obtain the host credential.

Host configuration may also define reusable Git identities and SSH signing-key fingerprints selected by host defaults/directory rules, while preserving optional exact-workspace bindings. The supervisor selects exactly one matching public key from the host SSH agent. The agent socket and private key are never mounted or exported. A dedicated Unix-socket broker accepts only bounded Git commit objects whose author and committer match the configured identity, then asks `ssh-keygen` to produce an SSHSIG signature in the `git` namespace. This grants project code authority to create signed commits as that identity; it does not grant SSH authentication or raw SSH-agent access. Project code can still create or push unsigned commits, so repositories that require signatures must enforce that policy server-side.

The general gateway:

- Denies unknown destinations by default.
- Authorizes exact hostname and port before DNS lookup.
- Rejects private, loopback, link-local, metadata, CGNAT, benchmarking, documentation, unspecified, multicast, and reserved addresses.
- Handles IPv4, IPv6, and IPv4-mapped IPv6 forms.
- Requires every DNS result to be public.
- Connects to the checked `SocketAddr` without a second DNS lookup.
- Records canonical denial events.
- Deduplicates identical denial events within a session.
- Keeps approval operations outside the guest-accessible protocol.

General-egress HTTPS remains an opaque CONNECT tunnel; opt-in account TLS mediation is a separate capability. Once a hostname is approved, the gateway cannot restrict methods or paths inside that TLS connection. Approval controls destinations, not data flow.

Direct requests that never reach the proxy do not generate denial events. They fail because the sandbox has no route or resolver access.

## Policy inspection

`slopbox status` reads global host policy, saved host-side setup, and narrowing repository policy, discovers permitted resource paths, and reports the next launch's access. Verbose output includes policy axes and mount targets. It checks only configuration presence for model providers; it does not read credential contents, decrypt secrets, query signing keys, evaluate Nix environments, or contact upstream services. It does not create project state and is not a live-session inventory or readiness check.

`slopbox doctor` additionally checks launch prerequisites and probes outer and nested native namespaces with a five-second timeout. The probe executes trusted host bubblewrap and a fixed Bash `exit 0`, clears the environment, closes inherited descriptors on supported kernels, and mounts the read-only Nix store or selected files/bundles plus private proc/dev/tmp filesystems. ELF discovery invokes only the protected system `ldconfig -p` to read loader-cache metadata; selected programs and scripts are not executed during discovery. It exposes neither workspace nor broker sockets. Diagnostics do not start Pi, run project or extension code, evaluate Nix, resolve account secrets, query signing keys, contact upstream services, or change project state/approvals. The temporary diagnostic file is removed after the probe. Passing these checks does not validate a complete launch, provider access, signing, or Nix realization.

## Approval boundary

Session and project rules have stable IDs, originating request IDs, creation timestamps, and revocation timestamps. An opt-in host-controlled approval view is available as a prototype. Global approvals, expiry, once-only grants, and a larger traffic dashboard are deferred; see [docs/roadmap.md](docs/roadmap.md).

Inside the sandbox:

```bash
slopbox denials
```

returns current-session events but cannot change policy.

On the host:

```bash
slopbox network events --follow
slopbox network approve <request-id>
slopbox network approve <request-id> --project
slopbox network approvals
slopbox network revoke <rule-id>
```

uses canonical host-side event state. `network events` includes retained project events with session and timestamp context; `--json` provides structured output. Repeated identical denials share a request ID and first-seen timestamp. Each supervisor holds an exclusive lock in its own session state; host inspection uses shared nonblocking lock attempts to distinguish live sessions from stale files. Session approvals use the request's originating session, not a project-wide active-session pointer. A denied request remains failed and must be retried after approval.

Rules permit exact normalized hostnames/IPs and ports. Approval defaults to the request's live session; project persistence requires `--project`. Neither scope overrides reserved-address checks, general-network policy, or model/account separation. Approval does not fix DNS errors. Every new authorization reads the current rule store. Revocation does not cancel already-authorized requests or established tunnels; another matching rule can still permit the destination.

Rules are stored in host-owned `network-rules.json` using a per-project exclusive lock, private permissions, and atomic replacement. Revoked records are retained; re-approval creates a new ID rather than reactivating an old one. Malformed state fails closed. Event logs are JSON lines and readers ignore an incomplete trailing append. Displayed event text escapes terminal controls.

Running `slopbox network approve` or `network revoke` inside the sandbox modifies no host policy because host state and control operations are unavailable there. The security boundary is the absent host control plane, not the absence of a command name.

### In-session host view

Foreground interactive launches retain the host controlling terminal in the supervisor and relay window-size changes. The sandbox's standard streams instead use a new PTY and session; the relay's descriptors are close-on-exec and are not passed to guests. In this mode the supervisor establishes the private controlling terminal before bubblewrap, replacing bubblewrap's usual `--new-session` setup. No approval endpoint or host control socket is exposed inside the sandbox.

Direct interactive Pi launches also reserve Ctrl+V for a host-mediated Wayland image import. Only host-terminal key input starts the fixed `wl-paste` helper; bracketed-paste input, framed control-string replies, and guest output do not request reads. As with other terminal hotkeys, unbracketed input cannot be distinguished from typing. There is no guest clipboard endpoint. The helper has a cleared environment except for Wayland display/runtime selection and locale, no inherited broker descriptors, bounded output, and short deadlines. It is killed/reaped on cancellation or termination. Only supported image MIME types with matching file signatures are published; image decoding remains inside the guest, not in the host supervisor.

An import shares the selected bytes with the whole sandbox, not just Pi's editor. Files have private permissions, are atomically published in a session-specific read-only mount, and are removed on normal shutdown. They never enter the workspace automatically. Limits are 20 MiB per image and 64 images/128 MiB per launch. Escape/Ctrl+C can cancel pending capture; later keystrokes cannot overtake it, and failure discards queued input rather than submitting or retrying. The guest receives neither a desktop socket nor access to subsequent clipboard changes. Generic commands and noninteractive launches do not enable image import. Abrupt termination can leave private runtime imports behind, and Pi history can retain images already consumed.

`slopbox --approval-view` (also available on `run`) additionally enables the host approval view. Without that flag, Ctrl-] is forwarded to the guest. Only host-terminal input is inspected for the approval hotkey. Guest PTY output is not parsed as an approval command. While the view is open, guest output is withheld and host input is consumed by the view. The view reads canonical current-session events and project rules, escapes terminal controls, shows the exact destination and scope, and uses a fresh random 48-bit single-attempt challenge for every mutation. Input batches are discarded across prompt transitions so queued keystrokes or terminal replies do not count as confirmation. Approvals still pass through the normal destination, scope, liveness, and reserved-address checks.

Broker threads' stderr is captured separately in a bounded tail buffer, not interleaved with the approval display. The last 16 KiB are escaped and printed after terminal restoration. The relay bounds its pending input/output buffers and handles terminal loss and termination by killing/reaping its sandbox child. Normal completion, handled signals, and suspension restore host termios; SIGKILL cannot run restoration.

The terminal emulator remains trusted. This prototype resets rendering modes and requests a guest redraw on return; it is not a full terminal emulator or screen-preserving multiplexer. Arbitrary terminal applications and visual behavior across emulators are not yet validated. The feature remains opt-in, and the separate host CLI remains available. Opening the view does not suspend computation or cancel existing connections, and approving never replays a failed operation.

## Credential boundary

The current model broker supports two provider credential sources:

```text
host OPENROUTER_API_KEY environment variable
host-managed OpenAI Codex OAuth credential
```

OpenAI OAuth access and refresh tokens are stored in a mode-`0600` host file under `$XDG_DATA_HOME/slopbox/credentials`. The store uses an advisory lock and atomic replacement for refresh-token rotation. OS-keyring storage and remote token revocation are not implemented yet. OAuth and refresh requests use fixed OpenAI endpoints without inherited host proxy settings.

The sandbox receives:

```text
OPENROUTER_API_KEY=slopbox:openrouter
synthetic OpenAI Codex JWT with account ID `slopbox`
```

A trusted Pi wrapper disables extension auto-discovery and explicitly loads the read-only Slopbox extension. The extension registers configured built-in providers against the model-only local port. The writable private Pi agent directory is therefore not trusted to supply executable extensions. The host model gateway:

- Accepts only `POST /openrouter/api/v1/chat/completions`.
- Removes guest authentication headers.
- Injects the real bearer token only into the fixed upstream request.
- Disables redirects and host proxy inheritance.
- Resolves OpenRouter itself, rejects reserved results, and pins the checked address.
- Requests identity-encoded responses and rejects other content encodings.
- Drops response headers containing the exact host credential and redacts exact credential bytes from the streaming response body, including matches split across reads.
- Does not persist the real key in guest files, project state, arguments, events, or logs.

For OpenAI Codex, the gateway accepts only `POST /openai-codex/codex/responses`, obtains or refreshes the OAuth access token under the host credential lock, extracts and stores the account ID during login/refresh, removes guest authentication and account headers, and injects the real bearer token and `chatgpt-account-id` only into `https://chatgpt.com/backend-api/codex/responses`. Pi currently attempts WebSocket transport first and falls back to SSE because the gateway exposes only the HTTP route.

Authenticated Pi/OpenRouter use has been manually validated with a real key. Local mock-upstream tests cover streaming before completion, downstream disconnect cancellation, redirect rejection, upstream connection failure, upstream status forwarding, request-header confinement, and reflected exact-credential redaction. Authenticated long-running streaming and cancellation against OpenRouter remain outstanding. Reflection after an upstream transformation or encoding is outside the exact-byte redaction guarantee; OpenRouter remains a trusted credential recipient.

### Current authority gap

The real credentials are secret, but each brokered provider authority is available to every process in the outer sandbox. Any such process can call the local OpenRouter port or model gateway socket directly without a network approval. Pi's default shell execution now enters the inner sandbox first, but Pi itself and trusted extensions still retain outer authority.

Code running through the default Pi shell adapter cannot exercise this authority. A trusted outer-sandbox extension, a future adapter bug, or any process deliberately launched outside `tool-run` could:

- Spend OpenRouter credits or consume ChatGPT subscription quota.
- Send workspace data to a configured model provider in a prompt.
- Exercise any operation exposed by the fixed route.

Approval-gating OpenRouter would not solve this. Pi needs persistent model access, and a session approval would still grant every process the same route. Pi and tool subprocesses need different network capabilities.

Use a dedicated OpenRouter key with a provider-side spending limit. The fixed Codex route reduces the OAuth token's exposed protocol surface but does not prevent use of the attached ChatGPT subscription through that route.

## Package and supply-chain behavior

Package-manager homes are private per project. Host Cargo, npm, Python, and similar caches are not mounted because they can contain credentials, mutable configuration, unrelated source, and poisoned state.

Consequences:

- First-time builds may need registry access.
- Registry destinations must be approved explicitly.
- Downloads persist in the project's private home for later sessions.
- Vendored or preseeded dependencies can work offline.
- Nix dev-shell tools are available, but this does not automatically populate language package-manager caches.

Network denial limits supply-chain code, but approving a registry for an entire build session is not complete protection. A compromised package can use any network and broker authority present while it executes.

The generic pattern is **acquire locked artifacts, then execute offline**:

1. Copy only lockfiles, manifests, package-manager configuration, and required patches into a disposable acquisition workspace where possible.
2. Run the ecosystem-specific acquisition command with no model credential, no publish credential, registry-only egress, and a writable quarantine cache.
3. Require lockfile integrity data and reject dependency resolution or lockfile changes by default.
4. Promote the resulting artifacts into the project-private execution cache.
5. Build, install, and test in the inner sandbox with OS-enforced network denial and no authenticated account routes, regardless of the package manager's own offline flag. The current `--tool-network=none` setting alone does not enforce this when account routes are configured.
6. Make promoted caches read-only during execution where practical.

“Fetch” is not a universal safe operation. Some package managers execute project configuration, plugins, VCS helpers, build backends, or dependency code while resolving or preparing artifacts. Such acquisition remains sandboxed and sees as little project data as possible.

Initial ecosystem mappings:

| Ecosystem | Acquisition | Offline execution | Important caveat |
|---|---|---|---|
| Cargo | `cargo fetch --locked` | `cargo test --locked --offline` | Sanitize discovered Cargo configuration and credential providers. |
| pnpm | `pnpm fetch --frozen-lockfile` | `pnpm install --offline --frozen-lockfile` | Include trusted workspace configuration and patches deliberately. |
| npm | `npm ci --ignore-scripts` in a disposable tree | Offline install/rebuild and tests | npm has no equally clean lockfile-only fetch phase; keep lifecycle scripts disabled during acquisition. |
| pip | `pip download --require-hashes --only-binary=:all:` | `pip install --no-index --find-links ... --require-hashes` | Source distributions and PEP 517 metadata/build steps can execute code and require a quarantined wheel-build phase. |
| Go | `go mod download` | `GOPROXY=off go test ./...` | Direct VCS fallback and private-module helpers need separate policy. |
| Maven/Gradle | Populate a private repository/cache under restricted egress | `--offline` build | Build files and plugins may execute during dependency resolution, so acquisition is not passive. |

Nix requires a separate design because evaluation and realization do not map cleanly onto language package-manager caches, and the current sandbox intentionally has no host Nix daemon.

Cargo is first only because Slopbox itself is Rust and Cargo has a comparatively clean acquisition command. The profile architecture is package-manager-independent.

## Manual audit results

The following were manually exercised successfully:

- Host-home and sibling-project hiding.
- Read-only Nix store.
- Private home, `/tmp`, `/dev`, and process view.
- Capability dropping and `no_new_privs`.
- Failed setuid, remount, device-node, host-handle, host-proc, and ptrace attempts.
- Absence of common host service sockets.
- Failed direct network access.
- HTTP and HTTPS deny-by-default behavior.
- Host-only approval state.
- IPv4, IPv6, mapped-IP, private-address, metadata-address, and encoded-IP rejection.
- Checked-address connection behavior.
- Synthetic OpenRouter credential visibility and successful host-side authentication.
- Synthetic Codex marker visibility, host OAuth credential confinement, and Codex header replacement with a mock upstream.

No escape was found using the tested userspace techniques. This is not evidence that no kernel, bubblewrap, parser, or filesystem vulnerability exists.

`tests/e2e.sh`, exposed as `nix run .#e2e`, now automates gateway-plane separation, inner `none`/`general` modes, OpenRouter and Codex credential-canary absence, host credential inventory, denial lookup, session and project approvals, live revocation, structured event following, rejection of guest approval/revocation attempts, persistent tool state, Codex model registration, Pi shell routing, project-extension rejection, Pi shell cancellation, and concurrent-session mount/configuration stability and approval independence, read-only status inspection, default Pi launch, terminal setup approval/cancellation, host-view input separation and fresh confirmation, broker-diagnostic isolation, PTY suspend/resume and termination restoration, width/height/rapid resize propagation with and without the approval view, Pi redraw on return, clipboard failure notices and read-only import visibility in Pi/inner tools, saved policy ceilings, doctor prerequisite failures and namespace probing, and Git URL rewriting in live/staged workspaces and inner tools. Git routing tests deliberately reject discovery requests at the broker before contacting an upstream; separate tests exercise successful Git fetch/pull/push against local repositories. It uses synthetic credentials and makes no model request. Clipboard capture uses fixture helpers in unit tests; PTY tests seed an import to check the real Pi/inner-tool mount. Live compositor clipboard transfer still needs host acceptance testing. Authenticated OpenRouter and Codex lifecycle behavior still requires separate acceptance testing.

## Security implementation status and remaining milestones

The sections below group enforcement work by area, not delivery order. [docs/roadmap.md](docs/roadmap.md) prioritizes launch, approvals, and reliable session/stage recovery before new platforms. Defects in supported security boundaries remain immediate work.

### 1. Split general and model gateways — implemented

The native backend now has independent data planes:

```text
general gateway
  approved HTTP/HTTPS traffic
  denial lookup
  no provider credentials

model gateway
  fixed provider routes
  credential injection
  no general proxying
```

Pi receives both when enabled. Inner tool subprocesses never receive the model gateway. General egress is optional; configured authenticated HTTP routes are a separate capability available to tools in either network mode.

### 2. Generic inner tool sandbox — implemented

The sandbox provides:

```bash
slopbox tool-run --network=none -- <command>
slopbox tool-run --network=general -- <command>
```

The inner sandbox:

- Uses another user, PID, IPC, UTS, network, and mount namespace.
- Removes provider, gateway, and inherited proxy variables.
- Uses a separate persistent tool home instead of the Pi home.
- Hides the model gateway socket directory.
- Hides general and model gateway sockets in `none` mode, but retains configured authenticated HTTP routes.
- Re-exposes only the workspace as writable from the outer filesystem.
- Preserves direct-network denial.

A network namespace alone is insufficient because pathname Unix sockets are filesystem capabilities. Manual integration tests confirmed that `none` mode reached neither the general nor model gateway, while general mode reached denial lookup but not the model port or socket. This does not imply that configured authenticated HTTP routes are absent. Agent adapters must use `tool-run`; arbitrary trusted outer-sandbox extensions can still spawn processes directly.

### 3. Pi adapter — implemented

A trusted read-only Pi extension routes:

- Built-in `bash` tool calls.
- User `!` commands.
- Selected extension tools where possible.

through `slopbox tool-run`. General mode is the default for usable deny-and-approve workflows; the host can disable general egress with `--tool-network=none`. Configured account routes remain available in either mode.

Pi remains in the outer sandbox with model access. Build and dependency code launched through the default shell paths executes in the inner sandbox without it. The trusted Pi wrapper and extension are outside the guest-writable home and mounted read-only. The wrapper disables extension auto-discovery and loads only the trusted adapter explicitly. Pi still requires writable settings and synthetic credential state, which remain in the private home alongside writable sessions. The adapter declines Pi project trust so project-local extensions cannot gain outer-sandbox authority. Host `AGENTS.md`, allowlisted settings, conventional resources, and configured npm packages are imported read-only. Broader git/local package support and per-resource policy remain future work. Agent-specific adapters remain separate from the generic enforcement core.

Acceptance criteria:

- Pi model calls succeed.
- Approved Pi web tools can use the general gateway.
- Build scripts cannot reach the model route or socket.
- Tool runs with `--tool-network=none` cannot reach the general or model gateway or direct network; configured authenticated HTTP routes remain available.
- Build scripts do not receive provider marker variables or Pi session metadata.

The adapter and direct RPC `bash` path have been smoke-tested in both `general` and `none` modes without making a model request. General mode reached denial lookup but not the model port or socket. A fixture confirmed that a project-local extension is not loaded. Imported global Pi extensions are trusted code: they execute in the outer process and can bypass the shell adapter by spawning processes themselves.

### 4. Acquire-then-offline workflows

Add package-manager profiles beginning with Cargo:

- Locked fetch command.
- No model credential.
- Workspace read-only where possible.
- Registry-only general egress.
- Private writable cache.
- Offline build execution afterward.

Later profiles cover pnpm/npm, Python, Go, and JVM package managers. Nix remains a separate profile because its evaluator, builders, fetchers, and store have different trust boundaries.

### 5. Staged workspace workflow

Copy-based staging is implemented through the workspace policy and retains an unmounted baseline plus each writable copy in project-private Slopbox state. Host-side lifecycle commands currently include:

```bash
slopbox stage list
slopbox stage diff <stage>
slopbox stage apply <stage>
slopbox stage discard <stage>
```

Whole-stage apply checks the baseline before updating and handles staged symlinks as leaf entries. Future hardening must add selective application, transactional rollback, and stronger protection against concurrent host changes during application. Applying a change does not make it safe to execute; review remains necessary.

### 6. Native-backend hardening

- Landlock filesystem rules.
- Seccomp restrictions for unnecessary namespace, mount, kernel-module, BPF, keyring, and tracing operations.
- Resource limits through user cgroups.
- Mediated live filesystem without special-file or pathname-socket exposure.
- Expand automated regression tests beyond the current native/Pi end-to-end suite.

Seccomp policy must account for browsers and development tools that legitimately use namespaces.

### 7. Formally verified policy core

Evaluate [Verus](https://github.com/verus-lang/verus) for a small I/O-free policy crate after the split-gateway semantics stabilize. Candidate executable invariants include:

- A general-gateway decision can never attach a provider credential.
- The model gateway can authorize only a fixed provider, method, and path.
- Reserved addresses are denied regardless of user approvals.
- DNS resolution is requested only after hostname authorization.
- Session approvals cannot become project approvals.
- Staged apply paths cannot escape the workspace root.

The runtime should consume the verified decision type rather than reimplementing policy around it. OS calls, DNS, sockets, `httparse`, reqwest/rustls, bubblewrap, the kernel, and other external crates remain trusted or require explicit external specifications. Verus verifies code against the specification; it does not prove that the specification captures the intended security policy. `assume`, `external_body`, and other trusted assumptions must be minimized and audited.

Verus currently supports a subset of Rust and is under active development, so this is a focused hardening experiment rather than a prerequisite for the POC. Integration tests and adversarial tests remain necessary.

### 8. MicroVM backend

Use a separate guest kernel when deliberately malicious code or kernel exploitation is in scope. Reuse the same workspace, gateway, credential, and approval interfaces while replacing the native enforcement backend.

# Platforms and backends

Slopbox policy is intended to be backend-independent. Filesystem preparation, accounts, identities, approvals, authenticated routes, signing, events, and staged changes should survive a backend change.

The primary runtime is NixOS-oriented and Pi is the supported harness. An [experimental native macOS path](macos.md) now runs explicitly selected Pi/Node and separately sandboxed bash on the tested M1/macOS 27 host. Standard Linux without host Nix, broader native Mac tooling and the VM designs below remain planned.

## Linux

### Native backend

The implemented backend uses bubblewrap with Linux user, mount, PID, IPC, UTS, and network namespaces.

It supports the `developer` and `contained` profiles. It shares the host kernel and is not intended to contain kernel exploits. See [../SECURITY-MODEL.md](../SECURITY-MODEL.md).

### Image-backed runtime (planned)

Build a pinned Linux OCI image containing the executor, isolation tools, and selected harness. Validate its execution on standard Linux without requiring Nix on the host, then reuse the image contents for the macOS guest. Nix may still build the image or provide an optional project environment.

Select an existing OCI execution mechanism through a small Linux spike. Prove nested namespace support, narrow broker mounts, blocked direct egress, and non-root operation before choosing a default. Do not expose a container-management socket to project code or require privileged mode as an undocumented workaround. Preserve the existing native Nix path.

### MicroVM backend

The planned `adversarial` profile requires a separate guest kernel and isolated image runtime. Slopbox should reuse a VMM rather than build one. Microsandbox, Gondolin, Apple Containerization on supported hosts, and other minimal runtimes are candidates.

## macOS

Two backends serve different workflows: native macOS execution for developer tooling, and a Linux VM for a separate guest kernel and image-based runtime. Do not require a Linux guest for every Mac project: native macOS/Xcode tooling is a product requirement.

| Workflow | Linux | macOS target |
|---|---|---|
| Native developer tooling, shared host kernel | bubblewrap | Seatbelt |
| VM-isolated Linux runtime, separate guest kernel | existing VMM to be selected | Apple `container` or another validated VMM |

These are architectural counterparts, not identical enforcement mechanisms. The current named `contained` profile still uses the native backend with staged changes and a restricted runtime; the planned `adversarial` profile selects a microVM. This plan does not change those profile definitions. A VM alone does not establish the full adversarial contract.

### Native Seatbelt backend (first macOS spike)

Evaluate Seatbelt through `sandbox-exec` for the macOS developer workflow. [Anthropic's sandbox runtime](https://github.com/anthropic-experimental/sandbox-runtime) is a useful reference for filesystem/network rules, Mach/Unix IPC controls, and compatibility tests. Its default broad read access is not Slopbox's desired policy; do not adopt its configuration wholesale.

Seatbelt constrains access rather than constructing bubblewrap's private Linux namespaces. Prove the policy, not just successful command launch:

1. Pi and native project commands can run with a private home/state directory and declared toolchain access.
2. Credential roots, unrelated files, desktop services, and inherited descriptors remain inaccessible.
3. Network access goes through the appropriate brokers; tool processes cannot access the harness's model capability. Do not assume nested Seatbelt profiles provide the needed separation without testing.
4. Filesystem rules handle symlinks and alternate path spellings, and descendants cannot broaden their authority.
5. PTY resize, cancellation, suspension, concurrent sessions, and cleanup work on the actual macOS 27 host.
6. A representative unsigned `xcodebuild` works with explicit SDK, build-output, and cache access.

Simulator/device access, Apple Events, and code signing are separate capability work. Inventory their required services instead of broadly enabling Mach IPC, forwarding Keychain access, or running builds unsandboxed. Use narrow brokers where suitable; any explicit host handoff must disclose that it leaves the sandbox.

The goal is to support the project's native development environment, not to silently remove restrictions when a tool needs more access. Unsupported combinations must explain the missing capability. Native macOS must meet its declared profile contract before being advertised as supported.

### Apple `container` (VM backend candidate)

Apple's [`container`](https://github.com/apple/container) CLI is a candidate for the VM-isolated workflow, not a prerequisite for native Mac development. Its current README names Apple silicon and macOS 26 as the supported baseline. Validate a pinned released version on the target M1/macOS 27 laptop rather than inferring support from the OS version alone. This design needs one Linux VM, not nested hardware virtualization.

Its underlying [`Containerization`](https://github.com/apple/containerization) framework runs each Linux container in a separate lightweight VM using `Virtualization.framework`. Relevant capabilities include:

- OCI-compatible images;
- a VM per container;
- VirtioFS bind mounts, including read-only mounts;
- named ext4 volumes and tmpfs;
- read-only root filesystems;
- CPU, memory, ulimit, capability, and process controls;
- per-container networking;
- Unix-socket publishing;
- a vsock guest agent;
- custom kernels and init images.

Potential Slopbox mapping:

| Slopbox concept | Apple primitive |
|---|---|
| Live workspace | restricted VirtioFS bind mount |
| Read-only workspace | read-only VirtioFS mount |
| Staged workspace | named ext4 volume populated by Slopbox |
| Broker connection | published Unix socket or vsock bridge |
| Guest runtime | pinned OCI image |
| Ephemeral files | tmpfs and disposable container |
| Resource policy | CPU, memory, ulimit, capability controls |

Use `container run` for the initial backend. `container machine` defaults to a read-write host-home share; its defaults are not suitable here. Do not use `container run --ssh`: it forwards the complete host SSH agent into the guest. Slopbox's signing and future SSH transport brokers should remain narrow host-side capabilities.

### Gaps to prove

Apple `container` is an isolation runtime, not a complete Slopbox backend.

The initial spike must prove:

1. The guest has no direct internet path; all egress reaches the Slopbox gateway.
2. Broker sockets work without exposing unrelated host sockets.
3. Only the selected workspace or staged volume is shared.
4. UID, symlink, mode, and case-sensitivity behavior is understood.
5. Signals, terminal resize, cancellation, and process exit propagate correctly.
6. A pinned Slopbox guest image can run Pi and project tools.
7. Staged changes can be exported and applied safely.
8. Resource limits and cleanup survive crashes.

The current upstream [CLI reference](https://github.com/apple/container/blob/main/docs/command-reference.md) documents `container network create --internal` as host-only networking. This is useful, but still exposes a route toward the host and is not a broker-only network policy. Likewise, `--no-dns` is not an egress barrier.

Prove that untrusted harness/project processes have neither direct internet access nor access to unrelated host services, with broker transport outside their network namespace or equivalently enforced isolation. Test Unix-socket/vsock forwarding direction and lifetime rather than assuming a published socket provides both directions. Recheck all required flags against the chosen release.

### Requirements and fallback

The official `container` CLI requires Apple silicon and supports macOS 26. It installs a system service and requires administrator authorization during installation. macOS 15 behavior is explicitly unsupported for full functionality.

If the Apple CLI cannot satisfy the broker transport or isolation tests, compare other existing Linux-VM runtimes. Gondolin and Lima are candidates; libkrun requires particular care because its documented security model requires host isolation of the VMM itself. This selection is independent of the native Seatbelt backend. Backend selection and any capability limitations must be explicit and visible in `slopbox status`.

### Implementation approach

For the Apple VM spike, start by invoking the signed `container` CLI rather than embedding its Swift APIs. Evaluate the native Seatbelt path separately; share host control logic where the concrete implementations permit it.

Proposed MVP:

- staged/read-only workspaces first;
- pinned arm64 Linux OCI image;
- no host home sharing;
- no SSH-agent forwarding;
- data-only harness resources;
- broker-only egress;
- explicit CPU and memory limits;
- disposable root with named project cache volumes.

Add live VirtioFS mode only after the staged path is reliable. Stage export/apply must reject concurrent host changes and retain recoverable work on failure.

Keep the portable host supervisor separate from the Linux executor. Current Linux-specific PTY, namespace, descriptor, and `/proc` assumptions need an audit; changing the bubblewrap command into `container run` is not the whole port. AWS SSO/Okta, Copilot login/refresh, approvals, signing, and explicit clipboard capture remain on the host. Never share `~/.aws`, a harness credential directory, or the host home to make authentication work.

## Backend interface

The policy layer should compile into a small backend plan covering:

```text
prepare image/runtime
mount workspace and private state
publish broker endpoints
configure network
start/exec/stop
forward terminal and signals
collect status and logs
snapshot or destroy
```

Backends may reject unsupported plans. They must never substitute a weaker implementation silently. Introduce this interface only as a concrete second backend requires it; users should keep the same launch, approval, and review workflow rather than configure backend primitives.

## Windows

Windows is not an active target. A future port would likely begin with WSL2 and require separate validation of filesystem semantics, networking, host integration, and credential storage.

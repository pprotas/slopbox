# Architecture

[Project direction](direction.md) is authoritative; [security model](../SECURITY-MODEL.md) describes current guarantees. Older POC/handoff documents may describe the removed embedded Pi integration.

The host coordinator in `session.rs` resolves the effective host and narrowing project policy, validates the workspace and selected runtime, creates private per-run state, attaches approved broker routes and launches one selected command. Bare `slopbox` uses a host `default_command`; `slopbox run -- COMMAND` accepts an explicit command. Saved launch records from older versions are read only as policy ceilings. The core never loads Pi extensions, resources, or a hardcoded harness command.

`backend/linux/` builds bubblewrap namespaces, selected ELF/script runtime grants, Nix store/closure grants, and an explicit cooperative `tool-run` inner sandbox. `backend/macos/` uses Seatbelt, selected Mach-O/script resources, literal read/execute grants, session supervision and coalition ownership. On macOS generic commands have a single outer role and no implicit Nix development environment activation. Unsupported capability requests fail rather than fall back to another role or runtime.

The host gateway separates general destination approvals, fixed model-provider routes, and host-selected authenticated HTTP routes. The host alone reads named secrets, login stores and the SSH agent; guest clients receive synthetic broker settings. `policy.rs` supplies restrictive policy merging; `launch.rs` reads historical saved ceilings; `guest_environment.rs` expands bounded public session values; `git_signing.rs` verifies commit metadata before asking the host SSH agent to sign. Repository policy cannot authorize those grants.

Harness-specific configuration, tool hooks and UI live in external integrations. Process isolation is useful on its own but does not imply harness/tool separation: ordinary subprocesses inherit outer broker authority. An external Linux integration may enter `tool-run` explicitly; the backend enforces that inner boundary independently. No extension presence or generic command name is evidence that all tools use it.

# Project direction

**Status: authoritative, approved by Pawel.**

This document governs Slopbox's product and architecture direction. It takes precedence over conflicting proposals in other design documents, roadmaps, integration plans, and configuration examples. Changing this direction requires an explicit owner decision and an update here, not an implementation-specific exception.

This is a design target, not a claim that the current implementation meets it. [SECURITY-MODEL.md](../SECURITY-MODEL.md) describes current guarantees and limitations. Architectural changes must preserve those guarantees; compatibility is not permission to weaken isolation.

## General-purpose isolation, not an integration catalogue

Slopbox should provide an execution boundary underneath development tools, not a curated environment that only works with tools we have individually integrated.

The core should understand processes, filesystem access, runtime resources, network connections, credentials, and signing. It should not require a new implementation for each CLI, forge, harness, or package manager. Moving every special case behind an adapter interface does not remove the maintenance burden.

Use stable boundaries:

- Git and HTTP rather than separate security implementations for GitHub, GitLab, Forgejo, and Bitbucket. Hosted, self-hosted, and enterprise installations should primarily differ in destinations, trust configuration, account acquisition, and permissions.
- Executables, environments, and permitted runtime files rather than a prerequisite list of recognized installers.
- Command execution rather than a particular harness's settings and extension system. Harness-specific resource imports and UI conveniences belong above the basic execution contract.

Some coupling is justified: OS enforcement mechanisms, model-provider authentication and wire protocols, and genuinely different capabilities. Keep it at those boundaries. A general plugin framework is not itself a solution.

## Harness integrations live outside this repository

Slopbox must not ship particular harness integrations, including optional built-in adapters. Pi extensions, Claude-side modifications and equivalent integrations belong in external projects or packages. They own harness-specific configuration, resource discovery, tool hooks, provider registration and UI behavior.

Slopbox owns the generic execution, resource, role, credential and signing interfaces those integrations consume. An extension can route a tool invocation into a restricted role; Slopbox must enforce that role independently. Extension presence alone is not evidence that every tool path is separated. Requested stronger contracts must fail explicitly when unavailable, not silently fall back to shared authority.

There should be one harness-neutral configuration and launch path, not mutually exclusive Pi and generic modes. The existing embedded Pi extension, Pi preparation and hardcoded Pi launch are migration work to extract, not a precedent for more integrations. Merely moving them into another module in this repository does not satisfy this boundary.

Compatibility fixtures may exercise real harnesses and separately supplied integrations. They must not become production adapters or prerequisites for ordinary command execution. Preserve the existing enforcement guarantees and regression evidence during extraction.

## Resolve the security contracts explicitly

Generality does not make the following differences disappear:

**Credential containment requires usable transport.** Keeping credentials outside an HTTPS client requires a brokering mechanism that client actually uses. The current `gh` Unix-socket configuration solves one case, not the general problem. Establish a shared transport design rather than finding a different CLI-specific transport setting for every client. Generic TLS mediation is a candidate to evaluate, not an approved implementation: trust stores, certificate pinning, upstream validation, redirects, and credential binding need a security design.

**Credential secrecy is not limited account authority.** A generic broker cannot infer repository-level authorization from a shared GraphQL endpoint. Prefer restricted accounts/tokens and provider-side permissions where available. Otherwise disclose the broader authority or retain explicitly API-aware restrictions. Do not silently broaden access for compatibility, forward authentication across origins, or treat a redirect as approval for another destination.

**Harness/tool separation is stronger than ordinary process isolation.** Separating tools executed inside a harness requires cooperation or a different execution model. Do not promise that boundary for every unmodified harness. Basic command isolation and richer harness integration need explicit capability contracts; no silent fallback from the stronger contract to a weaker one.

Real credentials, private keys, and host SSH-agent sockets remain host-side. Unsupported capabilities must be explained rather than worked around by sharing credential roots or bypassing enforcement.

## Runtime discovery without installer dependence

The enforcement layer should consume a common description of the selected executable, environment, read-only runtime resources, and private writable state. Package-manager resolvers may help produce it, but must not each implement their own security boundary.

Dependency discovery is a separate problem. Nix supplies closure information; arbitrary host installations may not. Establish how runtime resources are selected and validated without increasingly elaborate installer heuristics, manual lists of hundreds of paths, or silent whole-store/whole-home grants. A manifest users must maintain by hand would merely move the complexity into configuration.

Support an unfamiliar installation through the runtime contract, not by adding its brand to core validation. Installation paths alone are not proof of trust; host authorization and protection from sandbox modification still matter.

## Global defaults, local restrictions

An ordinary new repository should normally need no Slopbox-specific setup or configuration file once host defaults are established.

- Define each identity, account, and reusable runtime preference once.
- Select global defaults independently of individual workspaces.
- Allow host-owned rules to select or narrow defaults for directory trees, groups of workspaces, or individual projects, without copying definitions.
- Keep repository-controlled configuration narrowing-only. A project file or mutable Git remote cannot authorize another account, destination, signing identity, or host resource.
- Make precedence understandable and effective access inspectable, including where settings came from, without resolving secrets or starting a session.

The current file is global, but its exact-workspace identity and account-route bindings are not a reusable global model. Replace that model rather than hiding its repetition behind a wizard.

Users should not normally configure Pi and Node paths in a separate `[macos]` section. OS differences belong in backend implementations. Machine discovery and exceptional overrides may exist, but ordinary configuration should describe intended access and tools, not backend plumbing or installation-version paths.

Defaults must not silently attach unrelated accounts or grant access. Reuse host-authorized choices; require host approval for additional authority. Sensitive approvals remain explicit and scoped.

## Priorities and acceptance

Pause integration-by-integration expansion while establishing the general execution/runtime contract, shared credential transport, and reusable global configuration. Continue fixing security and reliability defects in supported behavior.

Use an unfamiliar CLI, a self-hosted forge, a non-Pi harness, and a differently installed tool as design tests, not instructions to immediately build several more bespoke integrations. Preserve existing enforcement tests and validate any stronger capability claims separately.

The acceptance criterion is:

> A new repository normally needs no setup, and an unfamiliar tool using supported protocols normally needs configuration rather than changes to Slopbox's core.

Failure to meet that criterion is a design issue to revisit, not an automatic reason to add another special case.

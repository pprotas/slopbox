# Concepts

> Historical design/validation record. Built-in Pi launch and integration have since been removed; use [current configuration](configuration.md) and the [security model](../SECURITY-MODEL.md) for supported behavior.

Slopbox separates who is acting, which external authority they can use, what software orchestrates the work, and where that work happens. Keeping these concepts distinct prevents configuration from collapsing into a list of secrets and mounts.

These are internal design and advanced configuration concepts. The everyday interface should explain files, connections, accounts, and how changes are applied without requiring users to learn this vocabulary.

## Human

The human is the policy authority. They choose what a project, harness, identity, and account may do. Approval controls must remain outside the agent-accessible data plane.

A human can delegate their own identity or account to an agent, but that must be explicit. “The agent can use my account” is an authority grant, not a convenience setting.

## Identity

An identity answers:

> Who should this action be attributed to?

For source control, an identity currently contains:

- Git author and committer name;
- Git email address;
- SSH signing-key fingerprint.

An identity is not a secret and is not sufficient to access a service. The corresponding signing operation is a capability supplied by a host broker.

An agent may act as:

- a dedicated service identity such as `pi <pi@example.com>`; or
- the human's identity, when the human explicitly delegates it.

A dedicated identity is preferable for autonomous work because forge history clearly distinguishes agent activity. A human identity can be appropriate for supervised work, but external systems may then make agent and human actions indistinguishable. Slopbox should retain its own session audit identity either way.

The current signing broker selects a host-agent key by fingerprint and only signs bounded Git commit objects whose author and committer match the configured identity. It does not expose `SSH_AUTH_SOCK` or provide SSH authentication.

## Account

An account answers:

> Which external service authority may be exercised?

Examples include:

- Forgejo, GitHub, or GitLab accounts;
- model-provider subscriptions and API keys;
- package registries;
- cloud accounts;
- authenticated MCP servers;
- issue trackers and CI services.

An account includes the service, endpoint, authentication mechanism, and provider-side scope. Its credential is only one implementation detail. A single account may support several protocols, such as Git smart HTTP, a REST API, and a provider CLI.

Accounts and identities are independent. A Forgejo token may authenticate API requests while an SSH key signs commits. Both may represent the same principal, but they convey different authority.

Slopbox should expose accounts through narrow capabilities:

- fixed authenticated HTTP routes;
- standard protocols such as Git smart HTTP;
- host-side OAuth refresh;
- external structured integration servers;
- protocol-specific brokers where generic HTTP is insufficient.

Real credentials remain host-side. The sandbox receives a synthetic marker, local route, or constrained protocol endpoint.

An unauthenticated local MCP server is a tool or harness resource, not necessarily an account. It becomes account-like when it carries external authenticated authority.

## Harness

A harness is the trusted orchestration program that:

- communicates with a model;
- supplies prompts and project context;
- invokes tools;
- loads extensions or plugins;
- stores sessions.

Pi is the first supported harness. A harness is distinct from the model provider and from the project it operates on.

Harness code has unusual authority because it may need model access while project tools must not inherit that access. A safe harness adapter therefore defines:

- how the harness is launched;
- which settings and data are imported;
- which executable plugins are trusted;
- how shell and build tools enter the inner tool sandbox;
- which model and general-network routes are available;
- where sessions and caches persist.

A command is not automatically a safe harness adapter. If Slopbox cannot prevent its tool subprocesses from inheriting model authority, the combination must be rejected or run without model credentials. The intended generic-command path receives no implicit model authority; that launch distinction still needs enforcement in the current runner.

## Project

A project is the host-owned trust context for one canonical workspace.

It binds together:

- workspace location and access mode;
- security profile and policy narrowing;
- harness selection;
- identities and accounts;
- private homes and caches;
- network approvals;
- retained staged workspaces;
- audit history.

A project is not synonymous with a Git repository. Git is optional, and one project may contain no repository or several repositories.

Project-local configuration is untrusted. A checked-in `.slopbox.toml` may describe requirements or reduce authority, but it cannot grant accounts, identities, host files, network destinations, or a weaker profile. Host configuration remains authoritative.

For shared projects, the repository can carry narrowing policy and environment declarations. Each user independently maps those declarations to local accounts, identities, paths, and approvals. Future organization policy may add another host-controlled ceiling, but project files must never become self-approving.

## Session

A session is one running realization of a project policy.

It has:

- an effective profile and backend;
- concrete filesystem mounts;
- gateway and broker sockets;
- session approvals;
- denial and audit events;
- temporary overlays and processes.

Session authority expires when the session exits. Project and future global approvals outlive it according to their own scope and expiry.

## Profile

A profile is a named assurance contract, not a user identity or account bundle.

Current profiles are:

- `developer`: compatibility-oriented native isolation and usually a live workspace;
- `contained`: staged workspace, selected project runtime, and data-only harness resources;
- `adversarial`: planned microVM isolation, staged files, isolated image runtime, and minimal explicit egress.

Profiles set defaults across independent policy axes. Unsupported combinations fail rather than silently becoming weaker.

The beginner interface should describe consequences—“changes are immediate” or “review before applying”—while retaining profile names for advanced inspection. Access summaries must come from effective grants rather than profile names alone. General networking, model access, and authenticated account routes are separate capabilities: disabling one does not imply an offline session.

## Capability and grant

A capability is concrete authority, such as:

- write this workspace;
- connect to `example.com:443`;
- call one authenticated repository API prefix;
- sign a matching Git commit;
- use a model route;
- read a host-provided skill directory.

A grant connects a capability to a project, session, harness, identity, or account. Configuration should describe high-level intent and compile it into these low-level grants.

This is the central security rule:

> Data-plane possession determines authority. Names, markers, and instructions are not security boundaries.

## Integration

An integration translates a service or harness into generic Slopbox capabilities.

For example, a Forgejo integration can derive:

- a repository-scoped Git smart-HTTP route;
- a repository API route;
- an `fj`-compatible API route;
- a read-only Actions web route;
- temporary Git remote rewriting.

Provider behavior belongs in a small trusted adapter or external structured server where possible, not in the policy core. Add adapters as the supported workflow needs them rather than building a general integration framework first. They propose grants for host confirmation and must disclose broad API authority required for CLI compatibility. The core continues to enforce generic routes, methods, paths, secrets, and protocol possession.

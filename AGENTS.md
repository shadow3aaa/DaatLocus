# Daat Locus Agent Guidelines

This file defines the repository's product and engineering boundaries.
[docs/architecture.md](docs/architecture.md) records architecture and interface
contracts; the contribution guides cover development, validation and packaging.

## System Boundaries

The shared WebUI runs in the Windows/macOS Tauri client and optional browser entry.
The Actor/App backend, durable routing, incremental HTTP API and conversation
client are connected.

- Root has the fixed IM presentation; every other Actor has the work presentation,
  including when opened in the main window. Reuse data/control hooks, not a
  container-dependent rendering mode. Account/settings live in the top-left
  floating menu, conversation search in the top-right floating button.
  Actor references open a conversation dialog; never restore a permanent sidebar.
- Use a rounded multiline composer with attachments, approval/model selectors
  and send/cancel controls. Preserve drafts when a send or upload fails.
- Derive local message/activity views from semantic events. Fetch heavy details
  on expansion and reconnect streams from the last consumed event cursor.
- Client interfaces use bounded queries and resumable incremental event streams,
  never repeated full-runtime snapshots.
- The desktop client owns its window and tray. Close hides the window; Quit client
  leaves the independent service running; Stop and exit uses OS-owner CLI control.
  Keep the native loop on the main thread and the backend free of GUI dependencies.
  Reuse the existing Logo and account HTTP/SSE contracts. Native WebViews grant no
  JavaScript shell/filesystem/process capabilities; external links stay in the browser.
- The maintenance CLI uses clap for argument parsing, validation, help and version
  exits. Preserve the original terminal character logo and help palette. Parse
  arguments before opening storage or starting the runtime. Android embeds the same
  core with app-private Files and HTTP Browser. Terminal and broader native Apps
  remain future work.
- Enforce account Owner and Actor permissions at every operation. Delegation
  cannot increase capability grants. Restrictive process isolation must fail
  closed when the platform cannot enforce it.
- Do not restore deleted modules merely to make the old product run or compile.
- Do not recreate the old Session, RuntimeTool, Dashboard, Sleep, or Workflow
  architecture through aliases, adapters, compatibility bridges, or renamed
  Apps. Git history preserves the former implementation.
- SCOPE/LSP and Coding wrappers are removed. Files/Skills are ordinary scoped App
  operations, including SHA256 freshness checks. Do not restore the engine or
  retain its build, hooks, dependency or documentation requirements.
- Preserve independently useful code; remove interfaces, state, dependencies,
  tests, and documentation that exist only to support the deleted system.

## Product Boundaries

- Actor = Agent core + installed App instances. The core is the Actor's execution
  mechanism, not a parallel business management system.
- App is a domain state machine exposing tools. It is the only tool abstraction.
  Do not require a common `get_state` tool or a separate App workspace UI.
- One fixed root Actor is the default entry. Owner can directly access any Actor
  through its stable conversation link. Creation/ownership is an internal
  permission relationship, not a user-visible access level.
- Ownership determines hierarchy; delegation does not change ownership or
  access. Results remain associated with the originating request.
- The account Core directly exposes only the portable Actors App. Hosted Actors
  perform research and filesystem/process/browser work. Keep the delegation App
  ceiling distinct from installed Apps and intersect it with device capability;
  never increase read-only, filesystem, tool or approval grants through delegation.
- Actor configuration is internal creation/persistence state, not a user management
  surface. Do not expose editors or client/native RPCs that rewrite Apps, workspace,
  instructions, read-only identity, filesystem roots, recipients or tool allowlists.
  User permission controls only switch Full access / Ask / Auto; model selection
  stays a composer preference. Actor creation happens through the Actors App,
  with inherited creation constraints. Actors cannot be deleted by users or models.
- All Actors retain identity, configuration, context, and history. Do not expose
  Actor deletion in the client, HTTP API, native protocol or model tools.
  Completion, idleness, restart or worker shutdown must not recycle an Actor.
- Project, read-only investigation, and general Actors use the same core with
  different Apps and access constraints. Enforce read-only and folder scope in
  tools, not only in prompts.
- Guest conversation sharing, PIN sessions and quick-tunnel publication are removed.
  Retain optional CF P2P signaling; it is not a conversation-sharing service.
- Keep one shared WebUI for native and browser clients. Root shows explicit user deliveries,
  compact task links, decisions and outcomes; it excludes intermediate model
  text and tool pipelines. Other Actors use plain work prose, chronological
  collapsible activity and independent outcomes. Use a muted elapsed-time disclosure
  with a thin separator, followed by interleaved prose and compact icon/text tool
  groups. Activity lists flow in the conversation, without a nested scrolling box
  or fade mask. Actor popups never change this
  identity-based presentation. Keep no permanent sidebar or multi-page Dashboard.
  Show outcomes and failures in their conversation; keep no separate new-update
  notification widget. Private provider diagnostics are not conversation prose.
- Keep Skills as discoverable instruction resources accessed through Apps. Keep
  authentication, configuration, persistence, delivery, diagnostics, and access
  control as infrastructure. Do not introduce another skill execution engine.

Keep implementation contracts in [docs/architecture.md](docs/architecture.md)
consistent with these boundaries. Resource lifecycle, distribution and
profile-local daemon management are infrastructure, not separate business systems.

## Execution And Approval

The account's core Actor is shared across devices. Devices share its identity,
inbox, model context and conversation,
provide App instances, and temporarily host its worker. Keep healthy affinity;
automatically hand off in stable order to a synchronized eligible device on loss.
Execution coordination belongs to account devices and must work over LAN or SSH
without external services. Worker/DO is optional public rendezvous and signaling;
it must not own execution leases, fencing epochs or business commits. Device
OpenRaft metadata uses persistent votes and joint membership, with authenticated
native RPCs. New devices remain learners rather than permanent voting dependencies.
Normal shutdown synchronizes and transfers authority through the existing quorum.
Loss cannot invent a singleton quorum. A previously synchronized eligible device
without reachable account authority automatically uses an explicit local recovery
branch, retaining inputs and uncertain effects. Reconnection freezes execution,
selects one valid inference context and transparently merges conversation messages
and observations before committing a shared head. Private archives preserve the
original branches; no manual history recovery UI is required. Never replay started
work from a losing branch.
A fresh single-device account runs locally. Availability during partition takes
precedence over a global single-writer guarantee; local branches are not Raft commits.
Ordinary Actors remain hosted. Never approximate the core
by merging independent roots. Resume inference, never replay uncertain App effects.
Every effect still requires device-side epoch, authorization, approval and sandbox checks.

- Actor inboxes are serial; Actors advance concurrently. Preserve durable input,
  tool protocol groups, origin, replies and delivery references across delegation.
- Core handles short coordination turns. Explicit durable deliveries can precede
  turn completion; yielding waits for the current tool protocol group to settle.
  Completing an input does not complete its delegated work. Progress and results
  return as correlated inputs; Core chooses meaningful user updates without polling
  or a second fast-response model. Do not load device project instructions into Core.
- The Agent loop has no fixed message, ReAct-step or tool-batch count limit. On
  input-budget exhaustion clear the working window and inject an archive recovery
  instruction; never summarize or slide away history. Actors App history reads
  remain scoped and paginated. Persist and replicate reset anchors and model-bound
  actual-usage calibration, without reusing stale usage or replaying uncertain effects.
- Record interrupted executions. Do not automatically replay App effects after
  restart, cancellation, configuration changes or resource shutdown.
- Auto uses an independent model request without worker history or execution
  tools. Its only decisions are approve/deny. Errors, timeouts and invalid
  responses deny the operation; Auto cannot fall back to human approval.
- Ask waits for Owner approval of immutable arguments and requested scope. None
  omits review and can use the operation's requested extra filesystem scope.
- A grant is limited to one proposed action. Never mutate permanent Actor grants
  from a model review. Read-only identity, tool restrictions,
  framework credentials and other Actors' private data remain hard constraints.
- Models cannot set their approval mode or change a child's inherited mode.
  Only Owner configuration changes mode. Cancellation/reconfiguration must
  invalidate waiting approvals. Owner HTTP resolution is only for Ask Pending.
- Opened filesystem targets must stay within the effective policy; an earlier
  canonical-path check alone does not justify unsafe symlink-following I/O.
- Restrictive native process isolation needs platform-specific positive and
  negative tests. Never replace an unavailable sandbox with host execution.
- Read-only Browser uses bounded HTTP GET without Chrome/page JavaScript,
  validates DNS/redirect destinations and does not inherit ambient credentials.

## Client Service Contract

- Use bounded Actor/message/result/approval queries and monotonic event cursors.
  Stream small semantic facts and references; fetch heavy details on expansion.
- Re-check authorization for every live-stream emission and heartbeat, including
  expired/revoked account sessions. Register wakes before queries and
  respect consumer backpressure. No repeated full-runtime snapshots.
- Owner accesses any Actor; only approval mode and conversation model
  preferences are user-editable. Root is the default entry; no Actor has a deletion
  control. Conversation URLs locate an Actor and never grant access.
  Global Owner approvals aggregate action controls. No guest share authorization exists.
- Construct source/reply/authority metadata on the server. Accept only opaque
  attachment IDs; a client path or forged source cannot grant access.
- Cookie writes require a trusted Origin and the request header; don't trust
  Host/Forwarded or add broad CORS. Keep private credentials out of observations.
- Resource bytes require Actor authorization and a controlled ledger path.
  HTML/SVG must use a sandbox CSP without same-origin privileges. Signatures
  cannot replace Actor access checks.
- Private persistence must establish OS Owner permissions before writing secret
  bytes and propagate permission failures, including on Windows.
- Configuration changes prepare a valid candidate and resolve removed Actor model
  preferences to the account default only after persistence succeeds. Providers
  and their models can always be removed; an empty model setup keeps input queued.
  Hold the lifecycle lock over worker stop, persistence and publication. Persist
  the exact candidate in the service profile, not a request rebuilt later.
- Normal builds compile and embed the WebUI, including the shared authentication
  WASM. Explicit `DAAT_LOCUS_BUILD_WEBUI=0` supports backend-only development checks;
  distribution must reject binaries without the embedded WebUI.
- Browser account login uses OPAQUE (RFC 9807) with the shared Rust/WASM suite and
  Argon2id. Passwords stay in the client; Worker/DO never stores or validates them.
  Remove the former Owner Token registry, Bearer path and token endpoints completely.
  Bind sessions to the account public-key namespace, exact profile and credential
  revision; use opaque HttpOnly cookies, never JavaScript/URL credential storage.
  First registration needs a verified local OS-owner connection, not loopback alone.
  New devices join through OPAQUE account login without pairing codes or manual
  enrollment interfaces. Discovery hints cannot grant trust. Bind bootstrap to
  account, source, target, immutable device proof, Origin and transport context;
  encrypt private enrollment records with the one-login key. Verify local OS-owner
  installation and exact namespace before importing. Protect these records from Apps
  and observations. Local preferences remain separate from account authority.
  Account activation resumes gated workers; login/logout is not an Actor lifecycle.
  Browser builds compile the shared authentication crate to WASM with wasm-pack;
  backend-only builds must not invoke this toolchain.

- Same-account device discovery/sync is automatic, with no enabled/LAN/public/core
  toggles. Keep an optional public connection origin and explicit SSH targets.
  Default device labels come from OS APIs. Non-model settings auto-save through
  serialized narrow patches; keep failed input and pending edits. Model/provider
  edits retain Save. UI text states fields, actual status and actions, without
  descriptions of architecture, implementation features or redundant guidance.
- Provider settings use list/detail pages within the same dialog. Adding a model
  automatically discovers it; keep no discovery button. Use models.dev metadata
  for defaults and capability choices with custom values, validated cache updates
  and compiled fallback. Translate provider labels in the Chinese UI.

## Tools And Access

- External effects require actual tool calls. Assistant text alone does not
  execute commands, edit files, send messages, or complete a delivery.
- Check paths and access at execution time. UI visibility and model instructions
  cannot substitute for filesystem or tool permissions.
- Preserve explicit identifiers and freshness checks where stale state could
  cause an operation to target the wrong resource.
- Let code perform mechanical lookup, enumeration, deduplication, validation,
  persistence, and delivery bookkeeping. Let the model make semantic choices.
- Keep provider credentials and authorization tokens out of logs, results,
  observations, and client-visible diagnostics.

## Model-Facing JSON Schemas

The portable schema contract applies to App tool inputs, structured model
outputs, and freeform fallback schemas. Provider boundaries must receive valid
schemas; do not silently repair them through provider-specific normalization.

- Use `daat_locus_macros::model_schema` and `model_schema_for::<Type>()` for Rust
  model-facing types. Use a narrow schema-only mirror when necessary, rather
  than handwritten provider JSON.
- Root schemas are objects. Every object has `properties`, `required`, and
  `additionalProperties: false`; every property is required.
- Represent optional model-visible values as required nullable fields. Missing
  field tolerance in local deserialization does not change this contract.
- Do not emit JSON Schema `default`; serde defaults and `skip_serializing_if`
  must not define model-visible optionality.
- Use scalars, homogeneous arrays, objects, nullable unions, and string enums.
  Avoid dynamic keys/maps, tuple arrays, payload-carrying enums, `prefixItems`,
  and schema-valued `additionalProperties`.
- Avoid composition/conditionals: `oneOf`, `allOf`, `anyOf`, `not`, `if`, `then`,
  `else`, `dependentRequired`, and `dependentSchemas`.
- Avoid nonportable validation keywords: string length, pattern, and format;
  numeric bounds and multiples; array bounds, uniqueness, and contains.
- Prefer inlined schemas. Do not place sibling keywords beside `$ref`.
- Keep names and descriptions concise. Validate dynamically supplied schemas
  when loading them, and test the final model-facing JSON.

## Data And Presentation

- Shared activity and tool observations describe source facts with the natural
  structure of each domain. They must not contain rendered rows, glyphs,
  truncation, preview text, layout, colors, or expanded/collapsed state.
- Preserve original text for unstructured output and typed data for structured
  domains. Do not reconstruct semantic facts by parsing old render output.
- Clients own drafts, selection, scrolling, expansion, and local presentation.
  Such state must not become Actor execution state or persisted source data.
- Tool activity uses concise rows and domain content views. Never show raw tool
  payload/argument JSON or model-specific transcripts as a product fallback.
  Approval parameters remain fully inspectable through readonly labeled values.
  Actual source files and command output retain their original content.

## Working In This Repository

- Inspect Git status before edits. Preserve unrelated user changes, untracked
  work, and dirty submodules. Do not reset or overwrite them as part of cleanup.
- Work within the user's authorized scope. Do not deploy, publish, message
  others, rewrite pushed history, or delete persistent Actor data without the
  appropriate user authorization.
- Before recursive deletion or moving files, resolve absolute target paths and
  verify each remains inside the intended workspace or explicitly named
  directory. On Windows use native PowerShell operations with `-LiteralPath`;
  do not pass enumerated paths to another shell for deletion.
- Run relevant checks on retained modules. Record passed checks and blocked
  checks accurately; do not claim product usability from a library check.
- Use configured formatters for mechanical changes, inspect their output, and
  keep unrelated formatting out of semantic edits.
- Commit titles must be in English and identify the actual change. Keep commits
  coherent. Do not commit research directories, caches, generated artifacts,
  runtime logs, or unconfirmed experiments.

## Source And Documentation

- All built-in App instructions live in `src/app_instructions`; tool descriptions
  and schemas stay with each App. Keep no partial root `prompts` directory.
- WebUI text uses matching `webui/src/locales/en-US.json` and `zh-CN.json` catalogs.
  Default to the browser's first supported language, with English fallback;
  Appearance offers browser default, Chinese and English with automatic save.
  Language changes update existing views. Preserve user text and original source content.
- Use `tests/http_api.rs` with contract suites in `tests/http` and reusable isolated
  account/server fixtures in `tests/support`. Do not restore the simulated UI example.
- Stable implementation contracts belong in `docs/architecture.md`; development,
  validation and packaging instructions belong in CONTRIBUTING documents.
  Remove task journals, screenshot handoff records and progress checklists;
  preserve release changelogs. The repository needs no Git submodules.

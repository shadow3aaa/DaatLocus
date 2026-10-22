# Daat Locus Agent Guidelines

This file defines the repository's product and engineering boundaries.
[docs/architecture.md](docs/architecture.md) records architecture and interface
contracts; the contribution guides cover development, validation and packaging.

## System Boundaries

The shared WebUI runs in the Windows/macOS Tauri client and optional browser entry.
The Actor/App backend, durable routing, incremental HTTP API and conversation
client are connected.

- Core has the fixed IM presentation; every other Actor has the work presentation,
  including when opened in the main window. Reuse data/control hooks, not a
  container-dependent rendering mode. A narrow app navigation rail contains Home,
  Apps, device-local Virtual Displays on Windows/Android, and the account menu;
  mobile uses a modal navigation drawer. There is one Core
  entry, no session/chat catalog or permanent Actor sidebar. Actor references
  open a conversation dialog. Settings use full-page categories.
- Virtual Displays is an Owner-only observation of existing device outputs, with
  transient image bytes rather than Actor resources. Viewing never creates or
  reconfigures screens, reconnects privileged transports, requests permissions or
  grants input control. Stop refreshing while the view is closed or backgrounded.
- Use a rounded multiline composer with attachments, a model selector
  and send/cancel controls. Preserve drafts when a send or upload fails.
- Derive local message/activity views from semantic events. Fetch heavy details
  on expansion and reconnect streams from the last consumed event cursor.
- Client interfaces use bounded queries and resumable incremental event streams,
  never repeated full-runtime snapshots.
- The desktop client owns its window and tray. Close hides the window; Quit client
  leaves the independent service running. The tray exposes only window visibility
  and client exit; service maintenance uses OS-owner CLI control.
  Keep the native loop on the main thread and the backend free of GUI dependencies.
  Reuse the existing Logo and account HTTP/SSE contracts. Native WebViews grant no
  JavaScript shell/filesystem/process capabilities; external links stay in the browser.
- The maintenance CLI uses clap for argument parsing, validation, help and version
  exits. Preserve the original terminal character logo and help palette. Parse
  arguments before opening storage or starting the runtime. Android embeds the same
  core with app-private Files, HTTP Browser and an Owner-enabled PhoneUse native
  backend. PhoneUse alone may use Shizuku or librootkotlinx Root authorization;
  the service and other Apps keep their ordinary UID and sandbox. Terminal and
  broader native Apps remain future work.
- Enforce account ownership, authenticated remote authority and sandbox access at
  every operation. Restrictive process isolation must fail
  closed when the platform cannot enforce it.
  The explicitly approved ComputerUse desktop capability is an exception to the
  project filesystem sandbox: target GUI apps retain this Windows user's file
  access. Keep this exception inside ComputerUse; other Apps retain their sandbox.
  Reject read-only remote authority and exclude Daat Locus's own processes/windows.
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
- Standard MCP servers install as device-local App instances through AppsManager or
  the Owner Apps page. Adapt nonportable schemas inside DL; never require a custom
  MCP protocol. List/inspect/refresh skip review but retain hard permissions;
  install/enable/close require existing approval. Refresh only at settled tool-group
  boundaries or the next input, preserving Actor identity/history and unchanged Apps.
  Core constructs only the portable Actors App. Other Actors receive every
  device-provided App; there are no Actor App or tool capability allowlists.
  Do not require a common `get_state` tool or a separate App workspace UI.
- Core is the only preset Actor and the default entry. Every other Actor is
  created and named by its creating Actor; never preset a device executor.
  Owner can directly access any Actor
  through its stable conversation link. Creation/ownership is an internal
  permission relationship, not a user-visible access level.
- Ownership determines hierarchy; delegation does not change ownership or
  access. Results remain associated with the originating request.
- The account Core directly exposes only the portable Actors App. Hosted Actors
  on the same device install the same device-provided Apps; creation cannot select
  Apps. Do not retain App access lists, tool allowlists, delegation App ceilings,
  local read-only Actor identities or recipient lists. Hosted Actors perform
  research and filesystem/process/browser work with their project sandbox and
  Actor-local remembered approvals. Authenticated remote authority and private
  data isolation remain execution boundaries, independent of App installation.
- Actor configuration is internal creation/persistence state, not a user management
  surface. Do not expose editors or client/native RPCs that rewrite Apps, workspace,
  instructions or filesystem roots.
  Owner settings switch one account-wide Full access / Ask / Auto mode for all existing
  and future Actors; executors have no individual approval-mode selector. Model selection
  stays a composer preference. Actor creation happens through the Actors App,
  with inherited creation constraints. Actors have no individual deletion operation.
- All Actors retain identity, configuration, context, and history outside an
  explicit Owner account-wide work reset. This settings-only maintenance operation
  stops execution, clears Actors, history, resources and grants across devices,
  and recreates the initial Core; it preserves credentials, models, device trust
  and workspace files. Signed reset generations fence old business traffic before
  reconnecting replicas synchronize. Never expose reset to models or Apps, or add
  individual Actor deletion in the client, HTTP API or native protocol.
  Completion, idleness, restart or worker shutdown must not recycle an Actor.
  Core may archive/unarchive hosted Actors only on an explicit Owner request,
  without review. Archive only idle Actors with no pending inputs or delegated
  work; persist the state and reject new messages on every local/remote path.
  Preserve identity, configuration, context, history, resources and grants.
  Unarchive restores message admission without replay; Core cannot be archived.
- Project, investigation and general Actors use the same core and device Apps.
  Project workspaces grant recursive filesystem access from their top directory.
  Enforce sandbox and authenticated remote restrictions in tools, not only prompts.
- Guest conversation sharing, PIN sessions and quick-tunnel publication are removed.
  Accounts run either purely locally or interconnect through one public relay.
  Remove device-direct LAN, SSH, WebRTC, ICE/STUN and hole-punching paths.
  The relay uses one portable WSS protocol on Cloudflare Workers/DO or an ordinary
  public server; it is transport infrastructure, not an execution authority.
- Keep one shared WebUI for native and browser clients. Core shows explicit user deliveries,
  compact task links, decisions and outcomes; it excludes intermediate model
  text and tool pipelines. Other Actors use plain work prose, chronological
  collapsible activity and independent outcomes. Use a muted elapsed-time disclosure
  with a thin separator, followed by interleaved prose and compact icon/text tool
  groups. Activity lists flow in the conversation, without a nested scrolling box
  or fade mask. Actor popups never change this
  identity-based presentation. Keep no session list, Actor sidebar or multi-page Dashboard.
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
Purely local accounts run the same Actor core without cross-device transport,
consensus, replication or partition-recovery branches. Preserve local crash
recovery, durable inputs and uncertain-effect protection. Interconnected accounts
keep execution coordination on account devices over authenticated, end-to-end
encrypted logical channels carried exclusively by the public relay. Worker/DO or
an ordinary relay server must not own execution leases, fencing epochs or business commits. Device
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
  Core chooses send for queued work or steer for guidance bound to its exact
  delegated request. Apply guidance only at settled tool-group boundaries under
  the original authority; never promote Core prose to Owner approval. Report
  unapplied Core guidance if its target ends; do not create another task from it.
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
  tools on the current account Core worker, including for operations hosted on
  another device. Review never changes Core affinity or uses its serial inbox.
  Its only decisions are approve/deny. Errors, timeouts and invalid
  responses deny the operation without blocking for human approval. After a denial,
  the originating Actor may explicitly appeal its immutable approval ID to Core.
  Core asks the Owner in natural language; only a later direct Owner reply may
  resolve that appeal, remember the exact scopes and asynchronously notify the
  Actor. Never automatically replay the denied call or treat worker prose as approval.
  Core validates Owner replies on its current host and sends a bounded native
  decision to the operation device. That device checks the original scope and
  hard authority, records its local grant and notifies the Actor; it must not
  run a Core App tool or require Core execution-context replication for approval.
- Apps declare operations requiring review, their review criteria and semantic
  approval fingerprints. Undeclared ordinary operations do not require review.
  Sandbox access outside existing or remembered roots independently requires
  review, regardless of which App requested access.
- Ordinary tool schemas and arguments contain no runtime review fields. An
  uncovered permission returns an immutable approval reference without executing.
  Actors submit that reference, task and reason through the separate Actors App
  approval operation. Runtime binds it to the original Actor, active input,
  arguments and scope. Approval remembers permission without replaying the call;
  retrying an operation covered by remembered grants needs no new application.
  New review applications include the Actor's current task and reason; runtime
  supplies operation, scope and authority facts. Check existing grants first;
  covered permissions need no new application or review. Task/reason metadata
  never changes semantic fingerprints. Appeal control operations do not require review.
- Ask waits for Owner approval of immutable arguments and requested scope. None
  omits review and can use the operation's requested extra filesystem scope.
  Ask/Auto rejection is a structured permission decision, not a tool or input
  execution failure. Preserve its reason and source approval reference across
  devices and activity/history views; prevent the denied effect while allowing
  authorized alternatives. Never automatically retry the denied operation.
  Approval memory is automatic, with no opt-in control or reminder. Display review
  cards only for Ask; automatic review stays out of the conversation UI.
  Approved fingerprints persist per Actor. Reuse equivalent approved actions;
  merge covered filesystem scopes, with directories recursively covering children
  and write grants covering reads. Policy, target or meaningful argument changes
  require a new action fingerprint. Keep remembered grants separate from Actor
  configuration and reclaim them when Actor data is removed by account reset.
  Never broaden authenticated remote authority, framework credential access or
  other Actors' private data through review or remembered grants.
- Models cannot change the account approval mode. Only Owner settings change it;
  execution resolves the current account mode rather than an Actor's creation snapshot.
  Cancellation/reconfiguration must invalidate waiting approvals.
  Owner HTTP resolution is only for Ask Pending; Auto appeals use the Core-mediated
  conversation path and remain separate from the original denied decision.
- Opened filesystem targets must stay within the effective policy; an earlier
  canonical-path check alone does not justify unsafe symlink-following I/O.
- Runtime directory names and roots, including `.daat-locus`, have no built-in
  filesystem blacklist. Apply ordinary scoped filesystem review and remembered
  grants to these paths; Terminal must not infer path permissions from command text.
  Explicit OAuth-file, Actor-resource and native maintenance authority boundaries
  remain independent of filesystem scope.
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
- Owner accesses any Actor; account approval mode and conversation model
  preferences are user-editable. Core is the default entry; no Actor has a deletion
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
- Build the shared WebUI/authentication WASM independently before its consumers.
  Tauri owns native assets through frontendDist and its built-in compression;
  native companion services and Android core disable embedded-webui. Standalone
  browser-serving binaries enable embedded-webui and embed the same dist with zstd;
  headless builds use --no-default-features. DAAT_LOCUS_BUILD_WEBUI=0 remains a
  backend-check override. Rust build scripts never invoke Bun or wasm-pack.
  Packaging validates the appropriate UI host and rejects duplicate native assets.
- Native transport targets only the verified profile-local service, retains
  session cookies in the profile's WebView store and never returns them to JS.
  Preserve HTTP authorization, OPAQUE, bounded pull-based SSE/cancellation and
  resource access checks. Native resource previews have no native command grants;
  do not introduce broad CORS, arbitrary HTTP proxies or business-command replicas.
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

- Account connectivity has two explicit persisted modes: local and relay.
  Relay accounts discover/synchronize authenticated devices automatically through
  the configured public WSS endpoint. Never infer a mode switch from a transient
  outage or silently turn a relay account into an independent local account.
  Keep no LAN, SSH, STUN, direct-address or per-feature connection controls.
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

## Test Design And Maintenance

- Test an observable behavior, failure mode, or stable external contract of this
  repository. Be able to name a plausible regression that would make the test
  fail. Test count and coverage percentages are not goals by themselves.
- Use independent expected results: known vectors, explicit examples, invariants,
  or externally visible outcomes. Do not copy the implementation into a test,
  derive the expected result through the same code path, or merely compare two
  identical calls.
- Do not test Rust/TypeScript language guarantees, derived traits, enum inequality,
  trivial field assignment, constant values, path-joining getters, or third-party
  libraries in isolation. Exercise our integration behavior when it adds a risk.
- Do not use source-code searches or regular expressions over implementation files
  as unit tests. Use compiler, type, lint, or architecture checks for static rules;
  exercise the resulting behavior for runtime contracts.
- Avoid tests that freeze incidental wording, colors, CSS classes, whitespace,
  private helper structure, field counts, or arbitrary batching boundaries. Exact
  assertions are appropriate for wire formats, escaping, accessibility semantics,
  security boundaries, and other explicit contracts.
- Assert the claimed outcome. A nonempty file, successful serialization, absence
  of an exception, or echoed input alone does not establish correct contents,
  persistence, fallback, authorization, or execution. Mocks must not supply the
  behavior the test claims to verify.
- Static rendering can verify initial markup, escaping, and accessibility
  attributes. It cannot prove clicks, subscriptions, language updates on mounted
  views, refresh, synchronization, or draft retention across a transition.
  Exercise that transition through the real controller/component, or remove the
  claim and the redundant test; wiring the expected state in a fixture proves
  nothing about the production integration.
- Do not assert literal translated copy to test localization or business status.
  Check locale selection and resource integrity separately. UI behavior tests may
  locate a semantic control through the current resource, and must assert the
  actual state/action. Preserve exact fixture text when testing user data,
  original tool output, escaping, or privacy; it is not interface copy.
- Put each case at the narrowest useful boundary. Unit tests cover nontrivial
  decisions and transformations; integration tests cover persistence, HTTP,
  process isolation, and coordination. Repeat a scenario at another layer only
  when it protects a distinct failure mode. Prefer table cases to copied tests.
- Keep normal tests deterministic and isolated: temporary storage, controlled
  inputs and clocks, local fake services, bounded waits, and restored global
  state. Never depend on a real account, credentials, mutable external catalogs,
  public network, or a developer's home data. External-environment checks must be
  explicitly opt-in and report unavailable prerequisites accurately.
- Await asynchronous assertions and propagate failures. Missing prerequisites
  must be reported as ignored/opt-in or fail with a clear reason, never return
  early and report success. Use self-cleaning unique temporary directories;
  normal tests must not write benchmark reports or fixed paths in the checkout.
  Provision and explicitly run required native sandbox/privilege cases in their
  platform CI jobs; marking them opt-in locally must not remove CI coverage.
- Regression tests must exercise the mechanism that caused the bug and assert
  the corrected result; where practical, confirm they fail before the fix. Keep
  authentication, authorization, sandbox positive/negative cases, protocol/schema
  contracts, data integrity, recovery, and uncertain-effect replay protections.
- Do not add tests for every reversible cosmetic edit or one-line wrapper.
  Remove obsolete, tautological, or redundant cases and their unused fixtures;
  do not weaken meaningful assertions or delete valuable coverage to hide a failure.
  Report separately which checks ran and which were not run.

## Source And Documentation

- Actor and approval-role instructions live in `src/app_instructions`; runner
  injects Actor instructions independently of App installation. Put on-demand
  cross-tool guidance in `src/builtin_skills`; keep tool descriptions and schemas
  with each App. Apps have no ambient documentation interface. Keep no partial
  root `prompts` directory.
- WebUI text uses matching `webui/src/locales/en-US.json` and `zh-CN.json` catalogs.
  Default to the browser's first supported language, with English fallback;
  General settings offer browser default, Chinese and English with automatic save.
  Language changes update existing views. Preserve user text and original source content.
- Use `tests/http_api.rs` with contract suites in `tests/http` and reusable isolated
  account/server fixtures in `tests/support`. Do not restore the simulated UI example.
- Stable implementation contracts belong in `docs/architecture.md`; development,
  validation and packaging instructions belong in CONTRIBUTING documents.
  Remove task journals, screenshot handoff records and progress checklists;
  preserve release changelogs. The repository needs no Git submodules.


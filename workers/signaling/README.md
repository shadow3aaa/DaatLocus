# Daat Locus Relay

This service connects account devices through public WebSockets. Deploy the same
relay on Cloudflare Workers/Durable Objects or an ordinary server. Devices retain
all Actor execution, Raft coordination, leases, checkpoints and durable delivery.
The relay carries bounded end-to-end TLS streams and OPAQUE login exchanges; it
never executes Apps, validates passwords or stores Actor history.

## Run and deploy

```sh
bun install --frozen-lockfile
bun run typecheck
bun test
# Real local Cloudflare adapter (environment variable set in the current shell):
# DAAT_LOCUS_TEST_RELAY_WORKER=1 bun test tests/local.test.ts
bunx wrangler dev --local
bunx wrangler deploy
```

The existing deployment uses `dl.shadow3.work`. Preserve the `SignalGroup` and
`AccountRoom` Durable Object bindings/migrations: existing revocation records
remain authoritative. Protocol 2 replaces the previous signaling protocol;
upgrade clients before using the new service. No direct-device fallback exists.

On an ordinary server, run `bun run server.ts` behind a TLS reverse proxy with
WebSocket Upgrade support. Environment variables:

| Variable | Meaning |
| --- | --- |
| `PORT` | HTTP port, default 8787 |
| `RELAY_LISTEN` | Listen address, default 127.0.0.1 |
| `RELAY_DATABASE` | Persistent SQLite file, default relay.sqlite |
| `GROUP_ROOTS` | Optional JSON object mapping permitted account IDs to root public keys |
| `REVOKED_STAGES` | JSON array of administratively denied device IDs, default [] |

Restrict OS access to the SQLite file and preserve it on restart. It contains
revocation records, not Actor data or account passwords. `/health` returns
`{"service":"daat-locus-relay","protocol":2}`. Clients use the public WSS
origin with explicit `mode=relay` (the native transport also permits an explicit
`ws://127.0.0.1` local endpoint for loopback fixtures/proxies); purely local accounts start no relay, device
listener, discovery, Raft, replication or partition recovery.

## Transport protocol 2

`/connect/<account-public-key-hash>` opens a device connection. The relay sends
`challenge` with a nonce, expiry and protocol 2. The client sends `register` with
protocol 2, enrollment and an Ed25519 `relay-register` proof. Its signed version-2
payload binds account, device, exact challenge and expiry. Membership signatures,
expiry, revoked issuers and configured account roots are checked before admission.
`registered` lists online devices; `presence` only advertises hints, never trust.
Devices validate the member ledger again before opening a channel.

The lower device ID opens a channel using `open {channel,target}` with a new UUID.
The recipient receives `incoming {channel,source}`, constructs its bounded byte
stream, and replies `accept {channel}`. The opener receives `opened {channel}`.
Both sides then perform their existing pinned mutual TLS and durable generation
handshake over this stream. Relay routing is not proof of peer identity.
`close {channel}` and `closed {channel}` retire a channel without affecting others.
Device replacement, revocation and transport loss close its associated channels.
After reconnect, use new channel UUIDs and the existing durable protocol cursors;
a forwarding acknowledgement is never a delivery receipt.

Binary messages contain: version byte 2, kind byte, 16 raw UUID bytes, body.
Kind 0 carries 1..16384 bytes. Each direction starts with 65536 bytes of credit.
Kind 1 carries one big-endian u32 granting consumed bytes back to the opposite
sender. Credits cannot exceed outstanding received bytes or the fixed window.
Each connection has at most 32 logical channels; registration/control messages
are limited to 64 KiB and 64 messages/second. Device-side queues and flow control
bound memory and prevent slow receivers from blocking the relay event loop.
All sends recheck current membership and revocation. There is no durable relay
message queue, tool retry, cloud execution lease or business commit.

`revoke` carries the existing signed account revocation record; the relay
persists it and broadcasts `revocations`, including to reconnecting clients.
The `/accounts/<login-name-hash>` room retains the OPAQUE device-join exchange:
public discovery, `start`/`finish` towards an authorized device and
`challenge`/`accepted`/`rejected` back. Device enrollment/private credential
records remain sealed with the one-login key and are verified before local import.

## Verification boundaries

`bun test` exercises real standalone sockets, signatures, registration,
replacement, channel forwarding/credits, revocation, OPAQUE packet routing and
rejection of execution-authority requests. The Worker opt-in runs the same
contracts against real local Durable Objects. Rust unit/HTTP tests use an
in-process mock relay while retaining production device TLS, multiplexing,
replication and coordination. Public endpoint checks are explicitly opt-in and
use disposable identities; they do not access an existing account.

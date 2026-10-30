# Daat Locus relay

The relay connects devices that use the same Daat Locus account. Purely local accounts do not need it. Choose either the Bun server or the Cloudflare Worker, then configure its public WSS address in the clients' account settings.

## Install and test

From `workers/signaling`:

```sh
bun install --frozen-lockfile
bun run typecheck
bun test
```

Normal tests use local sockets and disposable identities. To test the real local Cloudflare adapter, set `DAAT_LOCUS_TEST_RELAY_WORKER=1` and run `bun test tests/local.test.ts`.

## Bun server

```sh
bun run server.ts
```

Put it behind an HTTPS reverse proxy that forwards WebSocket upgrades. The default listener is `127.0.0.1:8787`. A request to `/health` returns `{"service":"daat-locus-relay","protocol":2}`.

| Variable | Default | Purpose |
| --- | --- | --- |
| `PORT` | `8787` | HTTP listen port |
| `RELAY_LISTEN` | `127.0.0.1` | Listen address |
| `RELAY_DATABASE` | `relay.sqlite` | Persistent SQLite file |
| `GROUP_ROOTS` | `{}` | JSON map of permitted account IDs to root public keys |
| `REVOKED_STAGES` | `[]` | JSON array of administratively denied device IDs |

Preserve the database on restart and restrict access to its OS user. It retains revocation records. The relay does not store account passwords or Actor history.

## Cloudflare Worker

Edit `wrangler.jsonc` with your Cloudflare account, domain and access settings before deployment.

```sh
bunx wrangler dev --local
bunx wrangler deploy
```

For an existing deployment, preserve the `SignalGroup` and `AccountRoom` Durable Object bindings and their migrations. Use clients that support relay protocol 2.

The [CI workflow](../../.github/workflows/ci.yaml) runs the relay checks. Deployment is separate from repository tests; do not point fixtures at a personal account or public relay.

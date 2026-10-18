# Stage signaling

This Worker and Durable Objects provide **online** rendezvous, signed WebRTC
offer/answer forwarding, and password-login message forwarding for new devices.
OPAQUE and its encrypted enrollment result terminate on devices. The Worker never
verifies passwords, decrypts or stores credential records, coordinates execution,
or receives Actor invocations, history, resources or offline mail. No TURN relay
is included.

The Durable Object uses the Hibernation WebSocket API. Attachments contain only
challenge/identity, membership expiry, issuer IDs and bounded rate counters.
An alarm removes expired registrations/challenges; ordinary WebSocket ping/pong
does not require a business poll. The server rechecks group configuration,
revocation and membership expiry before every forwarding operation.
Membership issuance accepts at most 30 seconds of future clock skew; expiration,
revocation, signatures and authorization chains remain strictly checked.

Requirements: Node 22 or newer and Bun. Node 24 was used for local verification.

```powershell
bun install --frozen-lockfile
bun run typecheck
bun test
```

## Deployment

The deployment configuration binds `daat-locus-signaling` to `dl.shadow3.work`.
For another installation, change `account_id` and the custom domain in
`wrangler.jsonc` to match an active Cloudflare zone in your account.

```powershell
bunx wrangler login --device --browser=false
bunx wrangler deploy
```

Wrangler creates the `SignalGroup` and `AccountRoom` SQLite Durable Objects from
the migrations and binds the custom domain. Set DL's optional public connection
service to `wss://dl.shadow3.work`, without a path. The root HTTPS path returns
404 because this service exposes only WebSocket endpoints.

When a proxy is required, set `HTTPS_PROXY` before running Wrangler; it does not
automatically use the Windows system proxy. Keep localhost out of the proxy with
`NO_PROXY=localhost,127.0.0.1,::1`.

## Verification

The explicit local-runtime test creates disposable signing keys in memory and
writes only public group configuration and test runtime state under the ignored
repository `target/stage-signaling-test` directory. It starts a local Workers
runtime on port 45827, tests registration/forwarding and rejection of business
messages, and closes the test process and WebSockets.

```powershell
$env:DAAT_LOCUS_TEST_SIGNALING='1'
# Set DAAT_LOCUS_TEST_NODE to an absolute Node 22+ executable if needed.
bun test tests/local.test.ts
```

For an operator-configured installation, `GROUP_ROOTS` is a JSON object mapping
allowed Group IDs to their independently pinned public authority keys. The
checked-in empty object allows self-authenticated public-key namespaces. A
nonempty map restricts who can use this connection service; it does not define
account or Actor authority. `REVOKED_STAGES` is a JSON array of
Stage IDs, including revoked issuers. These values contain no device private keys.
The custom domain is configured in `wrangler.jsonc`.

A native Stage connects to `wss://<configured-origin>/connect/<group-id>` and
signs the socket's fresh challenge with purpose `signal-register`. Membership
chains use exactly the signed bytes and domain from `src/stage/identity.rs`.
Only purpose `rtc-negotiation` forwards signed SDP, with device/group, target,
generation, nonce, expiry and consistent SHA256 DTLS fingerprint binding.
Native peers independently verify membership and SDP before admitting data.

`/accounts/<account-name-hash>` is an online lookup and transient login path.
Fresh device signatures register clients; existing account publishers additionally
prove their signed membership. Clients can send only login start/finish, publishers
only challenge/encrypted result/rejection. Lookup is not account authority: the
device-side OPAQUE exchange and bound encrypted result establish trust. Anonymous
login sockets cannot send Actor or coordination traffic. The ACCOUNTS binding and
v2 migration are included in the deployment configuration.

Local Worker tests and native loopback WebRTC tests are separate evidence. They
do not prove cloud deployment, domain routing or public NAT traversal. Stage
native connection supervision and device management use the same authenticated
peer protocol over LAN, SSH and WebRTC. The shared Actor is coordinated on devices;
it keeps working over healthy direct channels when this optional service is lost.

API reference: [Cloudflare WebSocket Hibernation](https://developers.cloudflare.com/durable-objects/best-practices/websockets/).

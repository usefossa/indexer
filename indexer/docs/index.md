# Fossa Indexer

A standalone Rust service that indexes Fossa protocol state and exposes the API used by the maintained OpenCode client. Supply a Solana HTTP RPC and either its standard WebSocket endpoint or a standard Yellowstone Geyser gRPC endpoint. No hosted Fossa API, Redis, provider SDK, wallet private key, or separate database server is required.

The client keeps wallet keys locally, signs transactions locally, and sends inference directly to the selected provider. This service indexes accounts and receipts, authenticates wallet ownership, and relays already signed transactions through the configured RPC. It never signs transactions, maintains accounts on chain, provides gas sponsorship, or performs inference.

## Run

Use Rust 1.88 or newer and a C compiler for bundled SQLite. Run these commands from the repository root:

```sh
cd indexer
cp .env.example .env
# edit FOSSA_RPC_URL and FOSSA_WS_URL for your RPC.
cargo build --release --locked
./target/release/fossa-indexer .env
```

For Yellowstone, set `FOSSA_GRPC_URL`; set `FOSSA_GRPC_TOKEN` only when the endpoint requires `x-token`. gRPC takes precedence when both stream URLs are set. HTTP RPC remains necessary for consistent snapshots, wallet reads, blockhashes, transaction submission, and historical receipts. Account streams do not replace those RPC capabilities.

The default `.env.example` endpoints assume a local mainnet-connected RPC at ports 8899/8900. A local test validator requires its actual genesis hash and matching program, market, and mint. Startup checks the RPC's full `getGenesisHash` result. The shorter Solana CAIP-2 network identifier is not interchangeable with that hash.

`fossa-indexer --help` and `--version` require no configuration. An optional positional path selects an environment file; otherwise `.env` is loaded from the current directory. Existing environment variables take precedence. Relative database paths resolve from the working directory. Configuration and state are ignored by Git.

Default API address: `http://127.0.0.1:8790`. `GET /health` returns HTTP 200 only while the canonical market is synchronized and the account stream is current; otherwise it returns 503 with readiness and slot information. SIGINT and SIGTERM close live sessions and stop the service. SQLite transactions preserve committed state; interrupted work is reconciled after restart.

## OpenCode

Keep the existing Fossa plugin and change its `options.apiUrl` to the indexer's public URL. For a local install, the maintained installer also accepts `FOSSA_API_URL=http://127.0.0.1:8790`.

```json
{
  "options": {
    "apiUrl": "http://127.0.0.1:8790"
  }
}
```

This is the options fragment of the existing plugin entry; preserve its package/path and other settings. Choose **Import wallet** in OpenCode. A wallet signature authorizes a local access/refresh session; wallet keys never leave the client. Browser quick connect returns HTTP 501 with instructions to use manual import. Website login, email/OAuth, hosted custody, provider administration, and browser funding flows are outside this service's API.

The maintained plugin can use its existing custom RPC setting as well. Without that override, it uses the authenticated indexer RPC proxy. The proxy accepts the client's signed legacy Solana transactions; it rejects unsigned, malformed, mismatched-payer, and unapproved-program transactions. It verifies every required Ed25519 signature before forwarding. No transaction is generated or signed by the indexer.

## Data flow and guarantees

1. Establish the account stream with confirmed or finalized commitment, then request a contextual `getProgramAccounts` snapshot. A configured market and matching mint must exist before readiness becomes true.
2. Validate owner, discriminator, exact account length, market association, canonical model/provider/metadata PDA and bump, user vault derivation, and model book bindings. Unknown accounts never enter the usable catalog. Terms and capacity books are joined only through their canonical model's recorded addresses.
3. Persist updates in SQLite WAL transactions. Compare `(slot, write_version)` for streamed updates; standard Solana WS receives an ordered local sequence for same-slot writes. Snapshot floors reject stale replay and tombstones prevent deleted accounts being resurrected by older updates. Snapshot reconciliation discovers missed updates, closures, and ownership changes.
4. Reconcile every 30 seconds by default and after reconnect. Restored state remains unready until fresh reconciliation. Disconnects, stalled slot notifications, failed reconciliation, and stale snapshots clear readiness. A confirmed index can observe forks; finalized commitment offers stronger finality with additional latency. Receipt history always uses finalized transactions.
5. Cache catalog JSON and fan it out through bounded live connections. Account ingestion has a bounded queue, batched persistence, and stream backpressure. Slow client writes time out after five seconds. Cached booking watches are delivered immediately; missing accounts use bounded background RPC reads. Inference data never crosses this service.

Catalog `ready` indicates current canonical chain state. `metadataPending` and `metadataVerified` report metadata coverage separately. Unavailable or invalid metadata excludes the affected model or leaves its provider unavailable; it does not disable unrelated verified models. Metadata fetches retry, and obsolete unreferenced documents are removed from memory and SQLite. Account count, raw indexed bytes, metadata bytes, HTTP response sizes, connections, and request rates are bounded.

Metadata must match its on-chain SHA-256 over the exact fetched bytes and pass the bundled provider/model schema. Model IDs must also match their on-chain SHA-256 identifier. Inline JSON data URIs are supported. Remote documents require HTTPS on port 443, a public hostname, public resolved IP addresses, no redirects, no URL credentials, and no proxy. Resolved addresses are pinned for the request. Fetches use four concurrent tasks, a 32 KiB body limit, and timeouts. External metadata hosts remain availability dependencies chosen by metadata authors.

## Receipts and accounting

Native `/v1/opencode/receipts` streams all locally archived compact `{booking, generation, charge}` summaries belonging to the authenticated wallet, in bounded chunks. This lets the client reconcile older pending requests without a silent 1,000-record cutoff. Profile/detail responses return the latest 1,000 detailed receipts.

The service discovers signatures with standard `getSignaturesForAddress`, then retrieves finalized transactions with `getTransaction`. It reconstructs successful top-level reusable claim instructions from instruction bytes and resolves v0 loaded addresses; optional or truncated runtime logs are not required. Failed transactions and malformed, foreign-market, or mismatched claims are excluded. SQLite deduplicates canonical receipt identities. Live and historical cursors survive restart. Unavailable transactions remain pending with bounded exponential retry delay, so they cannot starve available older transactions.

Historical coverage is always explicitly incomplete: a node can prune signature or transaction history, and reaching its oldest available page does not prove genesis-to-present completeness. Detailed profile coverage reports include pending transactions, the last discovery check, and whether the RPC's available history was exhausted. Configure an archival RPC when older receipts matter. Records accumulated in SQLite require disk space proportional to retained history.

Amounts, balances, charges, collateral, reserves, and generation counters are decimal strings backed by exact Rust integers. Token rates and small bounded counters are JSON numbers. The indexer does not convert atomic amounts through floating point. Native funds responses include only the authenticated wallet's selected lane and expired reservations eligible after the existing 15-second grace period.

## API

All native authenticated HTTP endpoints use `Authorization: Bearer <access token>`. Refresh uses the refresh token. Live WebSocket upgrades also require the access token in this header; it is never placed in the URL. Session families are revoked on logout, and refresh rotates both tokens atomically. Access expiry and revocation are checked during live sessions. SQLite stores only token hashes and expires one-time challenges after 120 seconds.

| Endpoint | Purpose |
| --- | --- |
| `GET /health` | Readiness, stream state, snapshot/index slot, version |
| `GET /v1/config` | Public protocol and native client settings only |
| `GET /v1/market/catalog` | Cached catalog; `/v1/market` is an alias |
| `GET /v1/market/metadata/{sha256}` | Verified referenced document bytes |
| `POST /v1/opencode/challenge` | `{wallet, lane}` to `{challenge, message}` |
| `POST /v1/opencode/connect` | `{challenge, signature}` with base64 Ed25519 signature |
| `POST /v1/opencode/refresh` | Single-use refresh token rotation; expiry in milliseconds |
| `POST /v1/opencode/logout` | Revoke the session family |
| `GET /v1/opencode/wallet` | Canonical lane/source/vault, balances, generation, booking pool |
| `GET /v1/opencode/funds` | Exact free balance and eligible expired reservations |
| `GET /v1/opencode/receipts` | All archived compact wallet receipt summaries |
| `GET /v1/opencode/live` | Authenticated catalog, chain, wallet, and booking updates |
| `POST /v1/opencode/rpc` | Allowlisted Solana reads and locally signed transaction relay |
| `GET /v1/market/orders` | Authenticated wallet/lane bookings |
| `GET /v1/market/profile` | Authenticated wallet, bookings, and receipt details |
| `GET /v1/market/providers/{wallet}` | Verified public provider profile and receipt details |
| `/v1/opencode/device…` | HTTP 501; manual import is available |

Challenge message: `fossa-opencode-connect-v1:{program}:{market}:{wallet}:{lane}:{challenge}`. Connecting an already initialized lane verifies its protocol, owner, and vault. Uninitialized lanes remain available for client-side setup.

Live envelopes are `{type: "catalog" | "chain" | "wallet", data}` and `{type: "booking", address, data: {owner, data, slot, version}}`. Booking `data` is base64, or null when absent/closed. Send `{watch: address}` or `{unwatch: address}`; at most 64 watches per connection. Chain data can be null when unavailable. Consumers must honor readiness and quote expiration.

Wallet `ready` means its on-chain lane is initialized; catalog readiness reports index freshness separately. Failed wallet reads close the live connection rather than reporting an initialized wallet as uninitialized.

RPC methods: `getAccountInfo`, `getMultipleAccounts`, `getLatestBlockhash`, `getSignatureStatuses`, `getBlockHeight`, `getMinimumBalanceForRentExemption`, `getFeeForMessage`, `getGenesisHash`, `getSlot`, `getBalance`, `getTokenAccountBalance`, `getHealth`, `sendTransaction`. Program allowlist for signed legacy transactions: configured protocol, System Program, Compute Budget, legacy SPL Token, and Associated Token Account. The payer must be the authenticated wallet. The proxy enforces packet and account-list bounds; RPC credentials remain server-side.

## Configuration and operation

The full settings list and defaults are in [`.env.example`](../.env.example). Default commitment is `confirmed`; `processed` snapshots are deliberately unsupported. The stale threshold must exceed the reconciliation interval. Defaults allow 100,000 indexed accounts, 128 MiB of indexed account bytes and metadata bytes independently, 128 live sockets, 600 HTTP requests/minute per peer IP, and 30 authentication attempts/minute per peer IP. No forwarded client-IP header is trusted; place additional per-client controls at a reverse proxy if exposing the service beyond loopback.

For remote use, set `FOSSA_PUBLIC_URL` to your HTTPS origin and expose the service through your own TLS termination. Native HTTP is accepted only for localhost addresses by the maintained client. gRPC `x-token` and RPC URL credentials/query tokens belong in `.env`, never public configuration. New state directories/files use restrictive Unix permissions; protect the database directory and backups because wallet identities, local sessions, and indexed history are private operational data.

Use a separate database for different program/market/mint, genesis hash, or commitment settings; the database records these identities and refuses mismatched reuse. Back up SQLite with a consistent SQLite backup mechanism or after stopping the service; WAL files participate in persistence. There is no automated on-chain maintenance or historical completeness guarantee.

## Verify and source layout

```sh
cd indexer
cargo fmt --check
cargo check --locked
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Tests use synthetic keys and local mocked RPC, WebSocket, and Yellowstone endpoints. They cover wire/auth compatibility, exact amounts above JavaScript's safe integer range, canonical metadata and PDA joins, stale/replayed updates, deletions, restart recovery, receipt reconstruction, and native receipt streaming. They do not contact mainnet or move funds. The release profile retains thin LTO, one codegen unit, and stripped symbols. Dependencies are locked; no sibling checkout is required.

`src/chain/` handles transport and reconciliation; `protocol.rs` validates account layouts; `index.rs` and `db.rs` manage state; `catalog.rs` and `metadata/` build verified views; `auth.rs` and `api/` implement native client contracts; `receipts.rs` discovers and archives historical claims. All code, tests, Cargo files, settings examples, and documentation live in this `indexer/` directory.

Transport references: [Solana contextual account snapshots](https://solana.com/docs/rpc/http/getprogramaccounts), [Solana WebSocket API](https://solana.com/docs/rpc/websocket/programsubscribe), [Yellowstone Geyser protocol](https://github.com/rpcpool/yellowstone-grpc/blob/master/yellowstone-grpc-proto/proto/geyser.proto).

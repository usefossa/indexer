# indexer

## scope

standalone protocol indexer and compatible client API. source, tests, and build settings live under `indexer/`. read [self-hosting documentation](https://fossa.ag/docs/users/sh) before changing protocol or API behavior.

## workflow

- dependency updates and security fixes are manual; retain vulnerability alerts and dependency inventories.
- read the relevant implementation before editing. clarify uncertain requirements and keep changes focused.
- deployments and package publication require an explicit request. preserve unrelated local settings and durable indexed data.

## implementation

- use compact modules, exact integer accounting, and bounded work. preserve canonical account validation, freshness checks, and restart recovery.
- keep inference direct between client and provider. do not introduce signing keys, custody, vendor-specific transport dependencies, or mandatory external services.
- keep readmes to documentation links: one properly capitalized link per component, each on its own line. public documentation explains roles, trust boundaries, and verified behavior without private operational details.

## configuration and security

- local secrets and settings belong in ignored `.env` files. examples contain placeholders only.
- never log or commit tokens, signed messages, credentials, private state, or generated artifacts.
- preserve existing databases and recovery state; maintenance does not authorize destructive resets.

## verification

- before a requested commit or push, run `cargo fmt --check`, `cargo check --locked`, `cargo clippy --locked --all-targets -- -D warnings`, and `cargo test --locked` from `indexer/`, then review the final diff.
- ordinary tests use isolated synthetic state and require no production credentials. live checks require explicit opt-in.

## commits and releases

- create commits and push only when explicitly requested.
- use `[type] short description`; allowed types are `[fix]`, `[feat]`, `[add]`, and `[update]`.
- messages must be one lowercase line. combine multiple changes with `+`.
- never add attribution or `co-authored-by` trailers.
- bump affected versions with semver for behavior or release changes. tag requested releases `vX.Y.Z`.

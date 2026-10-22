# indexer

standalone rust protocol indexer and client api. source, tests, build settings and documentation live under `indexer/`. read [documentation](indexer/docs/index.md) before changing protocol or api behavior.

- use compact modules, exact integer accounting and bounded work. preserve canonical account validation, freshness checks and restart recovery.
- keep inference direct between client and provider. never introduce signing keys, custody, vendor-specific transport dependencies or external service requirements.
- local secrets and settings belong in ignored `.env`; examples use placeholders. never log tokens, signed messages or credentials.
- no commits, pushes, deployments, dev servers or extra documentation unless requested. no screenshots. use subagents only when requested.
- requested commit/push requires `cargo fmt --check`, `cargo check --locked`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked` and final review from `indexer/`. bump semver for behavior changes.
- commit format: `[fix]`, `[feat]`, `[add]` or `[update]`, lowercase single line, no attribution. prose and comments lowercase; preserve normal user-facing copy and identifier casing.
- readmes contain documentation links only. public docs explain roles, trust boundaries and verified behavior without private operational details.

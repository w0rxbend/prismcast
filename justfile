# Prismcast task runner recipes. See AGENTS.md for the canonical command list.

# Format all workspace crates.
fmt:
    cargo fmt --all

# Lint everything; warnings are errors.
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Run the full workspace test suite.
test:
    cargo test --workspace

# Audit dependencies (licenses, bans, advisories, sources).
deny:
    cargo deny check

# Full CI gate: format check, lint, tests.
ci:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

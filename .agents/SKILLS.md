# Prismcast development skills

30 project-local skills support Claude Code, Codex and Kimi Code CLI. `.agents/skills/` is the canonical store; `.claude/skills`, `.codex/skills`, `.kimi/skills` and `.kimi-code/skills` are relative directory symlinks to it. Start an agent session from this repository. Existing sessions may require a restart to refresh discovery.

## Installed set

- Rust language and design: rust-stable, rust-stdlib, rust-by-example, rust-api-design, rust-macros, rust-unsafe-ffi.
- Rust engineering: rust-workspace, rust-module-layout, rust-cargo-build, rust-crate-discovery, rust-dependencies, rust-semver, rust-documentation, rust-style-clippy, rust-code-review.
- Runtime and verification: rust-concurrency, rust-performance, rust-observability, rust-testing.
- Planned controllers/services: rust-cli, rust-web, rust-web-security, rust-http-client.
- Desktop and media: relm4-desktop, gtk4-libadwaita, gstreamer-rust, linux-media-capture, linux-desktop-packaging, ffmpeg-rust-integration, ffmpeg.

23 Rust skills come from [full-stack-skills/rust-skills](https://github.com/full-stack-skills/rust-skills). The FFmpeg command/reference skill comes from [magnus919/agent-skills](https://github.com/magnus919/agent-skills). Their supporting resources and licenses are included. Exact upstream revisions and exclusions are recorded in [skills-lock.json](skills-lock.json). FFmpeg frontmatter has one documented local compatibility adaptation.

Six desktop/media integration skills were authored locally from PLAN.md, AGENTS.md and linked official documentation. They fill the Relm4, GTK, GStreamer and platform gaps without adopting unavailable GTK packs with missing references. They are instruction skills, not installed Rust/native libraries or proof that the application integrates successfully.

Excluded: database, embedded, Java migration, Lombok and UniFFI skills. Tauri/Electron, vendor AI-inference pipelines and unrelated frontend/design packs were not relevant to the requested Rust desktop stack. GStreamer remains the planned primary engine; FFmpeg supports probing, remuxing, validation and justified integrations.

## Usage and validation

Agents select skills from their names/descriptions. Explicitly mention the desired skill when needed, for example `Use relm4-desktop to implement the scene list` or `Use gstreamer-rust to debug recording finalization`. All project architecture and task rules in AGENTS.md still apply.

All 30 skills passed the skill-creator frontmatter validator. Entrypoint Markdown links to local resources resolve, and all five directory layouts expose the same 30 entrypoints. `just ci` passed (format, Clippy and workspace tests). Agent discovery was checked on disk; interactive CLI loading was not exercised.

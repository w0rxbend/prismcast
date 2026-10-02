# Worktree reconciliation — INTEGRATE-001

2026-10-02: the user requested merging every existing worktree into main and
continuing directly on main. All 41 auxiliary worktree HEADs are now ancestors
of main. No worktree or archived commit was deleted.

Twenty-seven heads were already reachable. The other fourteen histories were
merged while retaining the validated main tree. `git cherry main <branch>`
confirmed patch-equivalent integration for CAPTURE-001/002, CORE-005,
MEDIA-001..005, UI-002, UI-003 and toggle follow-ups. BRIDGE-001 and UI-004
were already reconciled as ef2f191 and eabfcba; `git range-diff` showed only
expected integration with coalesced snapshot refresh and scene controls.

The paused archive 2ce3390 was reviewed separately. Its scene polish was
already retained in 4a45a95 (see JOURNAL Phase 2). Current weak signal captures
and real GTK regressions supersede that implementation. Its alternative
source backend, settings and replacement platform exports conflict with the
validated reusable RGBA source/compositor contract, and its event vector is
unbounded. Those alternatives remain in merge ancestry for inspection;
current backend APIs and ADR-0011 were retained. Structured inventory,
runtime-floor diagnostics and additional patterns can be adapted in later
scoped tasks.

The only dirty worktree was capture-002-media: two Cargo.lock path-dependency
entries adding prismcast-capture to media-gst and preview. Both exact
dependency lists already exist in main. Its local edit remains preserved;
copying its older whole lockfile would remove newer auth/TLS dependencies.

Validation: `just ci` and `just deny` passed after reconciliation. The merge
changed ancestry only; the application tree remained byte-identical.

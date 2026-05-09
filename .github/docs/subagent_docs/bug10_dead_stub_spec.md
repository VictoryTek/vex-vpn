# BUG 10 Spec: Remove Dead `app.rs` Stub

**Feature Name:** `bug10_dead_stub`
**Severity:** Low
**Date:** 2026-05-09

---

## Current State Analysis

### `src/app.rs` (full file contents)

```rust
// Reserved for future signal bus implementation.
#[allow(dead_code)]
pub struct App;
```

The file is 3 lines. It declares a public struct `App` with no fields, no methods, and no
trait implementations. The `#[allow(dead_code)]` suppressor was added explicitly because the
compiler already knew the struct was unused.

### `src/main.rs` line 1

```rust
mod app;
```

This is the sole `mod` declaration at the top of the file (line 1), immediately before the
other module declarations:

```rust
mod app;      // ← line 1 — the declaration to remove
mod config;
mod dbus;
mod state;
mod tray;
mod ui;
```

The `mod app;` line is required for the project to compile; without it, `src/app.rs` would
be an orphaned file (Rust would not compile it). With it, the compiler pulls in the dead stub
and `#[allow(dead_code)]` silences the warning. The net effect is dead weight with no
functional benefit.

### Cross-file reference audit

A full search across all `src/*.rs` files for the tokens `app`, `App`, and `mod app` returned
the following **relevant** results:

| File | Line | Token | Notes |
|------|------|-------|-------|
| `src/app.rs` | 3 | `pub struct App;` | Declaration — to be deleted |
| `src/main.rs` | 1 | `mod app;` | Module declaration — to be removed |
| `src/main.rs` | 15 | `use crate::state::AppState;` | `AppState` — unrelated, from `state.rs` |
| `src/main.rs` | 34+ | `app_state`, `app` (lowercase) | Local variables — unrelated |
| `src/state.rs` | 63–149 | `AppState`, `impl AppState` | Separate struct in `state.rs` — unrelated |
| `src/dbus.rs` | 116, 160 | `apply_kill_switch` | Substring match only — unrelated |

**Conclusion:** The `App` struct defined in `src/app.rs` is referenced **nowhere** outside
that file. No other module imports it, constructs it, or passes it as a type.

---

## Problem Definition

The `app.rs` module was stubbed as a future signal bus abstraction. The actual inter-component
communication in vex-vpn uses:

- `Arc<RwLock<AppState>>` — shared mutable state passed by clone to every component
- `std::sync::mpsc::SyncSender<TrayMessage>` / `Receiver<TrayMessage>` — tray-to-UI channel
- A 3-second `poll_loop` in Tokio that refreshes `AppState` from D-Bus

This architecture covers all required communication paths. A separate signal bus layer adds
no value and was never built. Retaining the stub:

1. Increases noise for future maintainers ("what is this?")
2. Keeps a `#[allow(dead_code)]` suppressor that hides a real compiler diagnostic
3. Adds an unnecessary module declaration in `main.rs`

---

## Proposed Solution

**Remove the dead stub entirely.** This is a two-step delete:

### Step 1 — Delete `src/app.rs`

Delete the file. No content needs to be preserved.

### Step 2 — Remove `mod app;` from `src/main.rs`

Remove **line 1** of `src/main.rs`:

```rust
mod app;
```

Exact surrounding context (lines 1–6 before change):

```rust
mod app;
mod config;
mod dbus;
mod state;
mod tray;
mod ui;
```

After change (lines 1–5):

```rust
mod config;
mod dbus;
mod state;
mod tray;
mod ui;
```

No other lines in `main.rs` require modification.

---

## Implementation Steps

1. Delete `/home/nimda/Projects/vex-vpn/src/app.rs`.
2. In `/home/nimda/Projects/vex-vpn/src/main.rs`, remove the line `mod app;` (line 1).
3. Verify the build compiles cleanly with `nix develop --command cargo build`.
4. Verify clippy passes with `nix develop --command cargo clippy -- -D warnings`.

---

## Dependencies

None. This change removes code; it introduces no new dependencies.

---

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Another file imports `crate::app::App` that was missed | Very Low | Build error | Cross-file audit (above) found zero references; build step will catch any miss |
| Future developer wanted to implement the signal bus | Low | Minor rework | The `Arc<RwLock<AppState>>` pattern is documented in project context; a real bus can always be added later as a new module |
| File deletion cannot be undone without Git | Low | Low | File is 3 lines with no logic; Git history preserves it |

---

## Files to Modify

| Action | Path |
|--------|------|
| Delete | `src/app.rs` |
| Edit | `src/main.rs` (remove line 1: `mod app;`) |

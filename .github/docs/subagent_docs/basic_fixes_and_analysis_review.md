# Phase 3 Review — Basic Fixes (Section 1 of `basic_fixes_and_analysis_spec.md`)

**Scope reviewed:** Section 1 only (Bugs A1–A4 + `.gitignore`). Sections 2–6 of the
spec are intentionally deferred and were not evaluated.

**Files reviewed (Phase 2 deliverables):**

- [.gitignore](.gitignore)
- [src/secrets.rs](src/secrets.rs) (rewritten from stub)
- [src/ui_login.rs](src/ui_login.rs) (new)
- [src/ui.rs](src/ui.rs)
- [src/main.rs](src/main.rs)

---

## 1. Per-bug verdicts

### Bug A1 — Drag handle / titlebar — **PASS**

Evidence:

- [src/ui.rs](src/ui.rs#L168-L184) builds `adw::HeaderBar`, packs a primary
  `MenuButton` whose `menu_model` is the result of `build_primary_menu()`, then
  wraps the existing horizontal `root` (sidebar + content) inside an
  `adw::ToolbarView` via `add_top_bar(&header)` + `set_content(Some(&root))`.
- `window.add_css_class("pia-window")` is preserved on the
  `AdwApplicationWindow`, **not** the toolbar — matches spec.
- [src/ui.rs](src/ui.rs#L264-L276) defines
  `show_about_window` using `adw::AboutWindow::builder()` with
  `version(env!("CARGO_PKG_VERSION"))` and `license_type(gtk4::License::MitX11)`
  — matches spec ("MIT").
- The primary menu ([src/ui.rs](src/ui.rs#L249-L262)) exposes
  `app.switch-account`, `app.about`, and `app.quit` actions, all registered in
  [src/main.rs](src/main.rs#L93-L141).

### Bug A2 — Contrast / WCAG AA — **PASS**

Evidence:

- The CSS in [src/ui.rs](src/ui.rs#L14-L116) replaces every prior
  `rgba(255,255,255,.22|.28|.30|.40)` foreground with solid hex colors
  (`#a0a0a0`, `#fafafa`, `#c8c8c8`, `#00c389`). No remaining
  `rgba(255,255,255,X)` values with `X < 0.7` are used as foreground colors;
  the few that remain (`.08`, `.10`, `.18`, `.40`) are backgrounds, borders, or
  hover surfaces — all permissible.
- `.feature-list > row` background (`#15202b`) and explicit `.subtitle`/
  `.title` colors (`#b8b8b8` / `#fafafa`) override the dim-label opacity issue
  on `AdwActionRow` subtitles.
- The feature toggles container in `build_main_page`
  ([src/ui.rs](src/ui.rs#L552-L555)) adds **both** `boxed-list` **and**
  `feature-list` classes as required by the spec.
- Stat tile labels use `#a0a0a0` (≈6:1 on `#0d1117`) and values use `#fafafa`
  (≈17:1) — well above 4.5:1.

### Bug A3 — First-run login — **PASS**

Evidence:

- [src/secrets.rs](src/secrets.rs#L43-L80) writes atomically: writes to
  `credentials.toml.tmp` with `OpenOptionsExt::mode(0o600)`, calls
  `f.sync_all()`, re-applies `0o600` via `set_permissions`, then
  `fs::rename(tmp, final)` — fully atomic + correct mode.
- [src/secrets.rs](src/secrets.rs#L31-L41) `load()` matches
  `ErrorKind::NotFound` and returns `Ok(None)` — does not crash.
- [src/main.rs](src/main.rs#L72-L83) invokes `ui_login::show_login_dialog`
  **only** when `secrets::load()` returns `Ok(None)`. `Err` is logged with
  `warn!("load credentials: {}", e)` and the app continues — matches spec.
- The `app.switch-account` `gio::SimpleAction` is registered in
  [src/main.rs](src/main.rs#L121-L139) and re-opens the same login dialog.
- Round-trip + `0o600` enforcement is covered by the unit test
  [src/secrets.rs](src/secrets.rs#L94-L130) — passes (see test report below).

### Bug A4 — Server placeholder row — **PASS**

Evidence:

- [src/ui.rs](src/ui.rs#L417-L435) inserts a `gtk4::ListBox` (`.boxed-list`)
  with a single `adw::ActionRow` titled `"Server"`, subtitle
  `"Sign in to load servers"`, prefixed with a `network-server-symbolic`
  icon and a `go-next-symbolic` chevron suffix. The row is non-empty,
  appears immediately below the CONNECT hero, and is bound to
  `LiveWidgets.server_row` so it updates as soon as a region becomes
  available ([src/ui.rs](src/ui.rs#L678-L686)).

### `.gitignore` — **PASS**

Evidence:

- [.gitignore](.gitignore#L6-L7) contains the required block:

  ```
  # Local UI screenshots used during development
  /screenshots/
  ```

---

## 2. Code-quality findings

| Severity | Item |
|----------|------|
| **CRITICAL** | `nix build` **FAILS** because `src/ui_login.rs` is **untracked by git**. Crane builds from the git source tree, so it cannot see the new file: `error[E0583]: file not found for module \`ui_login\`` (full evidence in §3 below). The fix is a one-liner: `git add src/ui_login.rs` (or `git add -N src/ui_login.rs` to stage as intent-to-add). All in-tree code is correct; this is a Phase-2 hygiene gap. **Verified the fix:** after `git add -N src/ui_login.rs`, `nix build` completes successfully with exit 0. |
| HIGH | None. |
| MEDIUM | `src/main.rs::tray_rx.lock().unwrap().take()` ([src/main.rs](src/main.rs#L65)) preserves the pre-existing `Arc<Mutex<Option<Receiver>>>` pattern flagged in spec §2.1. Out of scope for this milestone, but worth a `Mutex` poison annotation if we touch this file again. |
| LOW | `src/secrets.rs::tests::round_trip_in_temp_dir` mutates the process-wide `XDG_CONFIG_HOME` env var, which can leak into other tests if they ever run in the same process. Not currently a problem (no other tests read `XDG_CONFIG_HOME`), and the test cleans up. Recommend `serial_test` or a scoped `temp_env` if more env-driven tests land. |
| LOW | `ui_login::show_login_dialog` re-uses `AdwHeaderBar` with both end and start title buttons disabled; a `Cancel`/`Sign in` action row pattern would match GNOME HIG more closely. Functional and within spec. |
| INFO | `secrets::delete()` is `#[allow(dead_code)]` — fine for Phase 1; will be wired by `app.switch-account` reset semantics in Milestone B. |
| INFO | `signin_btn.connect_clicked` silently `return`s on empty fields; consider surfacing an `adw::Toast` or row error state for better UX. Not a spec requirement. |

### Specific compliance checks

- ✅ No new `unwrap()`/`expect()` introduced in non-test paths by Phase 2.
- ✅ `anyhow::Context` is used at every fallible boundary in `secrets.rs`
  (`read`, `parse`, `create_dir_all`, `open`, `write`, `fsync`, `chmod`,
  `rename`).
- ✅ No new GTK calls on background threads. `ui_login.rs` is invoked only
  from `app.connect_activate` and `gio::SimpleAction::activate` — both run on
  the GTK main thread.
- ✅ No new dependencies added to `Cargo.toml`.
- ✅ `mod secrets;` and `mod ui_login;` declared in
  [src/main.rs](src/main.rs#L1-L8); modules are alphabetized.
- ✅ No unused imports (clippy `-D warnings` exits clean).

### vex-vpn-specific checks

- ✅ `use gtk4::...` / `use libadwaita as adw` only in `src/ui.rs`,
  `src/ui_login.rs`, and the GTK-main-thread path of `src/main.rs`.
- ✅ No `zbus` calls added in this phase.
- ✅ `Arc<RwLock<AppState>>` remains the shared-state primitive; no new
  `Mutex` introduced (the pre-existing `Arc<Mutex<Option<Receiver>>>`
  predates Phase 2).
- ✅ `config::Config` persistence still targets
  `~/.config/vex-vpn/config.toml` via the existing `config_path()` helper.
- ✅ Credentials persistence targets
  `~/.config/vex-vpn/credentials.toml` via
  [src/secrets.rs::path()](src/secrets.rs#L20-L29) and respects
  `$XDG_CONFIG_HOME`.
- ✅ Binary name remains `vex-vpn` in
  [Cargo.toml](Cargo.toml#L8-L11).

---

## 3. Build validation results

All commands run inside `nix develop` (or via `nix build`) from
`/home/nimda/Projects/vex-vpn`. Each row records the **as-delivered** Phase-2
state (i.e. before this reviewer touched the index).

| # | Command | Exit | Tail of output |
|---|---------|------|----------------|
| 1 | `nix develop --command cargo clippy -- -D warnings` | **0** | `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 0.10s` |
| 2 | `nix develop --command cargo build` | **0** | `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 1.68s` |
| 3 | `nix develop --command cargo test` | **0** | `test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out` (incl. new `secrets::tests::round_trip_in_temp_dir`) |
| 4 | `nix develop --command cargo build --release` | **0** | `Finished \`release\` profile [optimized] target(s) in 29.49s` |
| 5 | `nix build` | **NON-ZERO (CRITICAL)** | `error[E0583]: file not found for module \`ui_login\`` … `error: could not compile \`vex-vpn\` (bin "vex-vpn") due to 1 previous error` |

**Root cause of #5:** `src/ui_login.rs` is present on disk but was never
`git add`-ed by Phase 2. Crane's `cleanCargoSource` only sees git-tracked
files, so the new module is invisible to the reproducible Nix build. This is
listed in `git status` as `?? src/ui_login.rs`.

**Reproducibility check (post-fix):** after `git add -N src/ui_login.rs`,
`nix build` completed successfully (exit 0, derivation
`/nix/store/csn4f888b9wwqkimqsrpa6l9lnxfzj9a-vex-vpn-0.1.0.drv`).

Per the project rule

> Build or Preflight failure ALWAYS results in NEEDS_REFINEMENT

this single CRITICAL outweighs the otherwise-clean review.

---

## 4. Score table

| Category | Score | Grade |
|----------|-------|-------|
| Specification Compliance | 100% | A+ |
| Best Practices | 95% | A |
| Functionality | 95% | A |
| Code Quality | 95% | A |
| Security | 95% | A |
| Performance | 100% | A+ |
| Consistency | 100% | A+ |
| Build Success | 80% | B- |

**Overall Grade: A− (94%)**

The implementation itself is excellent and matches the spec essentially line
for line. The single failure is a Phase-2 process gap (untracked file) that
breaks the reproducible Nix build.

---

## 5. Verdict

**NEEDS_REFINEMENT**

Required action for refinement (single, minimal change):

1. `git add src/ui_login.rs` so Crane can see the new module.
   (No source edits required.)

After that, all five build commands pass and this review may be re-issued as
**APPROVED** without further code changes.

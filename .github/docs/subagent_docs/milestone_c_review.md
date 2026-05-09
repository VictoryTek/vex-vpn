# Milestone C — Review & Quality Assurance

**Date:** 2026-05-09  
**Phase:** 3 — Review  
**Reviewer:** Phase 3 QA Subagent  
**Spec:** `.github/docs/subagent_docs/milestone_c_lovable_spec.md`

---

## 1. Per-Feature Spec Compliance

### F1 — First-Run Onboarding Wizard

| Check | Status | Evidence |
|-------|--------|----------|
| `src/ui_onboarding.rs` exists | ✅ PASS | File present |
| Exports `show_onboarding` | ✅ PASS | `pub fn show_onboarding(app, state, pia_client, on_complete)` at line 28 |
| Carousel has 5 pages | ✅ PASS | Pages 0-4: Welcome, Sign In, Privacy, Kill Switch, Done; all appended to carousel |
| Async token validation with spinner | ✅ PASS | `glib::spawn_future_local` + `client_inner.generate_token().await`; spinner toggled correctly |
| Credentials saved on success | ✅ PASS | `crate::secrets::save(&creds).await` called on successful auth |
| `set_deletable(false)` + close guard | ✅ PASS | `.deletable(false)` in builder AND `connect_close_request(|_| glib::Propagation::Stop)` |
| `main.rs` routes to onboarding vs main window | ✅ PASS | `secrets::load_sync_hint()` → `Ok(None)` → `ui_onboarding::show_onboarding(...)` |
| On-complete callback builds main window | ✅ PASS | Lambda calls `build_and_show_main_window(&app_clone, ...)` |

**Note:** Spec §2.6 suggested wrapping the credential check in `glib::spawn_future_local` with the async `secrets::load().await`. Implementation uses `secrets::load_sync_hint()` (synchronous, non-blocking) directly in `connect_activate`. The `secrets.rs` module documents this as an intentional alternative for the GTK main thread. Behaviour is equivalent; the deviation is justified and documented.

**F1 result: PASS**

---

### F3 — polkit-gated `vex-vpn-helper` binary

| Check | Status | Evidence |
|-------|--------|----------|
| `src/bin/helper.rs` exists | ✅ PASS | File present |
| Named `vex-vpn-helper` in `Cargo.toml` | ✅ PASS | `[[bin]] name = "vex-vpn-helper" path = "src/bin/helper.rs"` |
| Handles `enable_kill_switch` | ✅ PASS | `Command::EnableKillSwitch { interface, allowed_interfaces }` → `run_nft_enable` |
| Handles `disable_kill_switch` | ✅ PASS | `Command::DisableKillSwitch` → `run_nft_disable` |
| Handles `status` | ✅ PASS | `Command::Status` → `check_status` |
| `interface` validated before use | ✅ PASS | `is_valid_interface(&interface)` called; also validates `allowed_interfaces` |
| No tokio / GTK / reqwest in helper | ✅ PASS | Grep confirmed: no tokio, adw, gtk4, or reqwest imports |
| `polkit-vex-vpn.policy` has `auth_admin_keep` | ✅ PASS | `<allow_active>auth_admin_keep</allow_active>` in XML |
| `src/helper.rs` invokes `pkexec vex-vpn-helper` | ✅ PASS | `Command::new("pkexec").arg(helper_path())...` |
| `nix/module-gui.nix` NOPASSWD rule removed | ✅ PASS | No `sudo.extraRules` block present; only polkit rule for systemd units |
| Polkit action installed in `module-gui.nix` | ✅ PASS | `environment.etc."polkit-1/actions/org.vex-vpn.helper.policy".source = ...` |
| `vex-vpn-helper` copied to `$out/libexec/` in `flake.nix` | ✅ PASS | `cp target/release/vex-vpn-helper $out/libexec/vex-vpn-helper` in `postInstall` |
| `@HELPER_PATH@` substituted in Nix | ✅ PASS | `--replace-fail '@HELPER_PATH@' "$out/libexec/vex-vpn-helper"` |
| `environment.pathsToLink = ["/libexec"]` | ✅ PASS | Present in `module-gui.nix` |

**F3 result: PASS**

---

### F4 — Secret Service Scope Reduction

| Check | Status | Evidence |
|-------|--------|----------|
| No `oo7` dependency added | ✅ PASS | `Cargo.toml` does not contain `oo7` |
| Plaintext fallback kept | ✅ PASS | `~/.config/vex-vpn/credentials.toml` path retained |
| Atomic write with 0600 permissions | ✅ PASS | `OpenOptionsExt::mode(0o600)` + `fsync` + `rename` |
| Permissions check (warn if world-readable) | ✅ PASS | `#[cfg(unix)]` block in `load_sync()` warns on `mode & 0o077 != 0` |
| `load()` is async | ✅ PASS | `tokio::task::spawn_blocking(load_sync)` |
| `save()` is async | ✅ PASS | `tokio::task::spawn_blocking(move || save_sync(&c))` |
| `load_sync_hint()` for GTK activate | ✅ PASS | Public, documented synchronous wrapper for main-thread use |
| Round-trip test with permission check | ✅ PASS | `secrets::tests::round_trip_in_temp_dir` covers load/save/delete + 0600 assertion |

**F4 result: PASS**

---

### F5 — Desktop Notifications

| Check | Status | Evidence |
|-------|--------|----------|
| `notify-rust = "4"` in `Cargo.toml` | ✅ PASS | Present on the `notify-rust = "4"` line |
| `notify_status_change` function in `state.rs` | ✅ PASS | `fn notify_status_change(old, new, region)` — private, synchronous |
| Called from `poll_loop` | ✅ PASS | `tokio::task::spawn_blocking(move || notify_status_change(&old, &new, region.as_deref()))` |
| Non-blocking (spawn_blocking) | ✅ PASS | Confirmed — not awaited, fire-and-forget task |
| Connected notification includes region name | ✅ PASS | `"Connected to {}"` with region name if available |
| Disconnected fires only from connected/kill-switch state | ✅ PASS | `matches!(old, Connected | KillSwitchActive)` guard |
| Error notification fires with Urgency::Critical | ✅ PASS | `.urgency(Urgency::Critical)` for `ConnectionStatus::Error` |

**F5 result: PASS**

---

### F6 — PreferencesWindow + ShortcutsWindow

| Check | Status | Evidence |
|-------|--------|----------|
| `src/ui_prefs.rs` exports `build_preferences_window` | ✅ PASS | `pub fn build_preferences_window(parent, state) -> adw::PreferencesWindow` |
| Three pages: Connection, Privacy, Advanced | ✅ PASS | `build_connection_page()`, `build_privacy_page()`, `build_advanced_page()` all called |
| Connection page: interface, max_latency, DNS | ✅ PASS | `adw::EntryRow`, `adw::EntryRow`, `adw::ComboRow` all present |
| Privacy page: kill switch + allowed interfaces | ✅ PASS | `adw::SwitchRow` + `adw::EntryRow` for comma-separated ifaces |
| Advanced page: auto-connect + log level | ✅ PASS | `adw::SwitchRow` + `adw::ComboRow` |
| `app.preferences` action registered in `main.rs` | ✅ PASS | `gio::SimpleAction::new("preferences", None)` → `add_action` + `set_accels_for_action("app.preferences", &["<Control>comma"])` |
| `assets/shortcuts.ui` exists | ✅ PASS | File present |
| ShortcutsWindow loaded via Builder | ✅ PASS | `gtk4::Builder::from_string(include_str!("../assets/shortcuts.ui"))` in `ui.rs:294` |
| Wired to `app.show-shortcuts` action | ✅ PASS | `gio::SimpleAction::new("show-shortcuts", None)` calls `ui::show_shortcuts_window` |
| Primary menu includes Preferences + Shortcuts | ✅ PASS | `view_section.append(Some("Preferences"), Some("app.preferences"))` + Keyboard Shortcuts |
| `show_shortcuts_window` uses safe `.object()` (no panic) | ✅ PASS | Uses `match builder.object::<gtk4::ShortcutsWindow>("help_overlay")` with error log |

**F6 result: PASS (cargo builds)**

---

## 2. Code Quality Findings

| # | Finding | Location | Severity |
|---|---------|----------|----------|
| CQ-1 | `Mutex::unwrap()` on poisoned mutex — theoretically panics if the locking thread panics | `main.rs:74,105` | LOW — mutex used for one-time `take()`, poisoning extremely unlikely |
| CQ-2 | `.expect("no display")` panics if GTK can't find a display | `ui.rs:154` | LOW — acceptable startup panic; GTK cannot run without a display |
| CQ-3 | `libc` crate imported in `src/bin/helper.rs` but `use libc` is missing — `libc::geteuid()` called as unsafe FFI directly | `src/bin/helper.rs:44` | INFO — compiles correctly (Cargo resolves it); `libc` dep in `Cargo.toml` |
| CQ-4 | Helper writes nft ruleset to predictable `/tmp/pia_kill_switch_<pid>.nft` — symlink TOCTOU possible (see Security section) | `src/bin/helper.rs` | MEDIUM — see SEC-1 |
| CQ-5 | `ConnectionStatus::Error(msg)` body is sent verbatim to notification daemon | `src/state.rs` | LOW — msg is static "Service failed"; no credentials leak |
| CQ-6 | `allowed_interfaces` field in `HelperRequest` is `Option<&[String]>` but `apply_kill_switch` always passes `None` — `allowed_ifaces` in helper never populated from GUI `Config::kill_switch_allowed_ifaces` | `src/helper.rs:88-94` | MEDIUM — functional gap: prefs page allows setting extra ifaces but they are not forwarded to the helper |

---

## 3. Security Findings

| # | Finding | Severity | Details |
|---|---------|----------|---------|
| SEC-1 | **TOCTOU tempfile** — helper writes nft ruleset to `/tmp/pia_kill_switch_<pid>.nft`. PID is predictable; a local attacker could pre-create a symlink at that path before helper runs, redirecting the write to an arbitrary file. Since the helper runs as root, this is a write-as-root primitive. | MEDIUM | Mitigation: pipe ruleset directly to `nft -f -` stdin rather than via a tempfile. |
| SEC-2 | `validate_interface` in `helper.rs` (`is_valid_interface`) requires first byte to be `ascii_lowercase`, but the config-level `validate_interface` in `config.rs` allows uppercase digits in position ≥1 (`is_ascii_lowercase() || is_ascii_digit()`). Minor inconsistency but both block injection chars. | LOW | No injection risk; both reject `;`, `\n`, `"`, spaces. |
| SEC-3 | Error notification body forwards `msg` from `ConnectionStatus::Error(msg)` verbatim. Currently `msg` is only set to `"Service failed"` from the poll loop. If future code introduces richer error messages, they could appear in the notification. | LOW | Document that notification bodies must not include tokens or IPs. |
| SEC-4 | polkit `auth_admin_keep` policy caches authentication per session. If a less privileged user obtains an active session with cached auth, they could trigger kill switch without re-prompting. This is the expected and documented behaviour (`auth_admin_keep` vs `auth_admin`). | INFO — by design |
| SEC-5 | No token/credential logging found — auth_token, username, and password are not passed to any log macro. `pia.rs` tests confirm `AuthToken` Debug output is redacted. | ✅ PASS |

---

## 4. vex-vpn-Specific Architecture Checks

| Check | Result | Notes |
|-------|--------|-------|
| Both `vex-vpn` and `vex-vpn-helper` in `Cargo.toml [[bin]]` | ✅ PASS | Lines 11-16 |
| `vex-vpn-helper` installed to `$out/libexec/` in `flake.nix` | ✅ PASS | `postInstall` copies to `$out/libexec/vex-vpn-helper` |
| GTK calls only in `src/ui*.rs` and GTK main thread path of `src/main.rs` | ✅ PASS | `tray.rs`, `state.rs`, `dbus.rs`, `helper.rs`, `secrets.rs`, `config.rs`, `pia.rs` are GTK-free |
| zbus stays at 3.x | ✅ PASS | `dbus_proxy` macro and `Connection::system().await` — 3.x API only |
| `Arc<RwLock<AppState>>` for shared state | ✅ PASS | No `Mutex<AppState>` introduced |
| Config still targets `~/.config/vex-vpn/config.toml` via `config_path()` helper | ✅ PASS | `config_path()` in `config.rs` unchanged |
| Binary name remains `vex-vpn` in `Cargo.toml [[bin]]` | ✅ PASS | `name = "vex-vpn"` present |

---

## 5. Build Validation Results

| # | Command | Exit Code | Result | Notes |
|---|---------|-----------|--------|-------|
| 1 | `nix develop --command cargo clippy -- -D warnings` | 0 | ✅ PASS | Zero warnings, zero errors |
| 2 | `nix develop --command cargo build` | 0 | ✅ PASS | Clean debug build |
| 3 | `nix develop --command cargo test` | 0 | ✅ PASS | 15 tests passed, 0 failed |
| 4 | `nix develop --command cargo build --release` | 0 | ✅ PASS | LTO + strip release build |
| 5 | `nix build` | **101** | ❌ **FAIL** | **CRITICAL** — see below |

### `nix build` failure detail

```
error: couldn't read `src/../assets/shortcuts.ui`: No such file or directory (os error 2)
   --> src/ui.rs:294:46
    |
294 |     let builder = gtk4::Builder::from_string(include_str!("../assets/shortcuts.ui"));
```

**Root cause:** The Crane source filter in `flake.nix` `commonArgs.src` includes only:
1. `.crt` files (via `certFilter`)
2. Files matched by `craneLib.filterCargoSources` (Rust source + Cargo files)

The file `assets/shortcuts.ui` is a GtkBuilder XML file — it is **not** a Rust source file and **not** a `.crt` file, so it is excluded from the Nix build sandbox. When `include_str!("../assets/shortcuts.ui")` is evaluated by `rustc` at compile time, the file is absent and the build aborts.

**Required fix (in `flake.nix`):**

```nix
commonArgs = {
  src = let
    certFilter = path: type:
      type == "directory" || builtins.match ".*\\.crt$" path != null;
    uiFilter = path: type:
      type == "directory" || builtins.match ".*\\.ui$" path != null;
    srcFilter = path: type:
      (certFilter path type) || (uiFilter path type) || (craneLib.filterCargoSources path type);
  in pkgs.lib.cleanSourceWith {
    src = craneLib.path ./.;
    filter = srcFilter;
  };
  # ...
};
```

---

## 6. Score Table

| Category | Score | Grade |
|----------|-------|-------|
| Specification Compliance | 97% | A+ |
| Best Practices | 88% | B+ |
| Functionality | 95% | A |
| Code Quality | 87% | B+ |
| Security | 82% | B |
| Performance | 95% | A |
| Consistency | 96% | A |
| Build Success | 60% | D |

> Build Success is scored 60% (4/5 steps pass; step 5 `nix build` is a critical failure).

**Overall Grade: B (88%) — weighted down by critical nix build failure**

---

## 7. Verdict

**NEEDS_REFINEMENT**

---

## 8. Issues to Fix

### CRITICAL — Must fix before PASS

| ID | File | Fix Required |
|----|------|-------------|
| FIX-1 | `flake.nix` | Add `.ui` file filter to Crane `srcFilter` so `assets/shortcuts.ui` is included in the Nix build sandbox. See exact code in §5 above. |

### MEDIUM — Strongly recommended

| ID | File | Fix Required |
|----|------|-------------|
| FIX-2 | `src/bin/helper.rs` | Replace tempfile write with direct pipe to `nft -f -` stdin to eliminate the TOCTOU symlink attack surface (SEC-1). |
| FIX-3 | `src/helper.rs` | Pass `allowed_interfaces` from `Config::kill_switch_allowed_ifaces` when calling `apply_kill_switch` so prefs-page settings actually take effect (CQ-6). |

### LOW — Optional improvements

| ID | File | Note |
|----|------|------|
| OPT-1 | `src/main.rs` | Replace `Mutex::unwrap()` on `tray_rx` with `.unwrap_or_else` or `ok()` for defensive handling. |
| OPT-2 | `src/state.rs` | Add doc comment noting `notify_status_change` must not include tokens or server IPs in notification bodies. |

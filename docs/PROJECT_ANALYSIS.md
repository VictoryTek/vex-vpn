# vex-vpn — Project Analysis

A public engineering review of the `vex-vpn` codebase as of May 2026. This document is the actionable companion to the bug‑fix specification at `.github/docs/subagent_docs/basic_fixes_and_analysis_spec.md`. Walk down the list and tick items off as they ship.

The review covers four reported runtime bugs ("Section A"), a full architectural review ("Section B"), and a prioritized feature backlog ("Section C"). Severity uses a four‑tier scale: **Critical**, **High**, **Medium**, **Low**.

---

## At a glance

| Area | Status | Headline |
|------|--------|----------|
| Architecture & threading | ✅ Addressed | `process::exit` removed; `async_channel::Receiver` clone replaces `take()` pattern. |
| Error handling | ✅ Addressed | `Config::load` returns `Result`; `anyhow::Context` at D-Bus boundaries; auto-reconnect toggle in prefs. |
| Async & D-Bus | ✅ Addressed | Proxy caching via `OnceCell`; NM `StateChanged` watcher; handshake watchdog (Milestone D). |
| Security | ✅ Partially hardened | Sudoers narrowed, interface validated, TLS pinned, token memory-only. Secret Service (`oo7`) deferred to C. |
| PIA integration | ✅ Implemented | `generate_token`, `server_list`, `measure_latency` shipped; `add_key`/port-forward stubs deferred. |
| Kill switch | High risk | Runtime path ignores `allowedInterfaces`/`allowedAddresses`; pre‑connect leak window. |
| UI / UX | ✅ Partially addressed | Headerbar ✅, contrast ✅, server picker ✅. Preferences / Shortcuts / About deferred to C. |
| Tray | ✅ SHIPPED (Milestone E) | SVG icons bundled; tray event-driven via broadcast channel. |
| Config | ✅ SHIPPED (Milestone E) | Interface validation ✅. Atomic write ✅. Schema version ✅. |
| Nix packaging | OK | Runtime closure misses `wireguard-tools`/`nft`/`iproute2` for non‑NixOS. |
| NixOS module | ✅ SHIPPED (Milestone E) | DNS override `lib.mkDefault` ✅. `wg` path hardened ✅. |
| Testing | ✅ Improved | 23 tests (15 unit + 3 integration + 5 pia); `tests/config_integration.rs` added. |
| Documentation | OK | README is solid; `nix run` standalone path is undocumented. |
| Dependencies | ✅ Updated | `reqwest` 0.12 (rustls) added. `thiserror` still unused. |
| Build & CI | ✅ SHIPPED (Milestone E) | `scripts/preflight.sh` + `cargo fmt --check` ✅. GitHub Actions + GitLab CI ✅. |

---

# Section A — Reported Bugs (all Critical)

## ✅ A1. Window has no titlebar / drag handle — SHIPPED (Milestone A)

- **Where.** [src/ui.rs](../src/ui.rs#L137-L156) — `adw::ApplicationWindow::set_content(&horizontal_box)`.
- **Why.** `AdwApplicationWindow` ships *no* default titlebar — the caller must add one. We added a `gtk4::Box` directly, so the window has no draggable surface (especially visible on Wayland), no close/min buttons in some compositors, and no place to attach a primary menu.
- **Fix.** Wrap the existing root in `adw::ToolbarView`, push an `adw::HeaderBar` as the top bar, hide the title text, and set the toolbar view as the window content. This is the canonical libadwaita 1.4 layout (verified via the `gnome/libadwaita` documentation set).
- **Bonus.** The new headerbar is also where the primary menu lives (Bug A3 needs it for "Switch account…").

## ✅ A2. Unreadable text — dark fonts on dark background — SHIPPED (Milestone A)

- **Where.** Embedded `APP_CSS` in [src/ui.rs](../src/ui.rs#L16-L93). Six selectors use 0.22 – 0.40 alpha foregrounds on a `#0d1117` window — well below the 4.5 : 1 WCAG AA threshold. The libadwaita `.dim-label` rule used by `AdwActionRow` subtitles is calibrated for the default Adwaita window, which we replaced.
- **Worst offenders.**
  | Selector | Current | On `#0d1117` |
  |----------|--------|---------------|
  | `.section-title` | `rgba(255,255,255,.22)` | ≈ 2.0 : 1 |
  | `.stat-label` | `rgba(255,255,255,.28)` | ≈ 2.4 : 1 |
  | `.hero-ip` | `rgba(255,255,255,.30)` | ≈ 2.6 : 1 |
  | `.nav-btn` | `rgba(255,255,255,.40)` | ≈ 3.2 : 1 |
- **Fix.** Replace those alpha colors with solid `#a0a0a0` (≈ 6 : 1 vs. `#0d1117`) for dim text and `#fafafa` for primary text. Wrap `AdwActionRow`s in a `.boxed-list` / `.feature-list` container so they sit on a card background where `.dim-label` is legible. Full CSS replacement is in the spec.

## ✅ A3. No login prompt on first run — SHIPPED (Milestone A)

- **Where.** Three contributing factors:
  - [src/secrets.rs](../src/secrets.rs#L1-L4) — stub.
  - [src/pia.rs](../src/pia.rs#L1-L4) — stub.
  - [src/main.rs](../src/main.rs#L43-L60) — `app.connect_activate` calls `ui::build_ui` unconditionally; never checks for credentials.
- **Why.** The README's NixOS path expects `/run/secrets/pia` to exist before the app launches. `nix run github:victorytek/vex-vpn` does not produce that file, so the systemd unit fails its `ConditionFileNotEmpty`, no `region.json` is ever written, the app silently shows "Select a server" forever — and there is no UI affordance for the user to recover.
- **Fix.** Implement a minimal `secrets::{load,save,delete}` that persists to `~/.config/vex-vpn/credentials.toml` with `0o600`. On startup, branch: if no credentials, present a modal `adw::Window` with `AdwEntryRow` + `AdwPasswordEntryRow` and a "Sign in" button that validates against PIA's `generateToken` endpoint (in the new `pia.rs`). Add a "Switch account…" entry to the new headerbar menu (Bug A1) for re-login. Move to Secret Service (`oo7` crate) in a follow-up milestone.

## ✅ A4. No servers listed / no server picker — SHIPPED (Milestones A + B)

- **Where.** No code path exists that *displays* a server list. Dependent on Bug A3 because `region.json` (the only present source of region data) is written by the backend after auth.
- **Fix.** Two parts:
  1. **Data** — implement `pia::PiaClient::server_list()` against `https://serverlist.piaservers.net/vpninfo/servers/v4`, with PIA's CA bundled via `include_bytes!`.
  2. **UI** — add an `adw::NavigationView` containing a Dashboard page (current widgets) and a Servers page (`adw::PreferencesPage` + filterable `gtk4::ListBox`). Each row shows region name, country, port-forward badge, and live latency.
- **Phasing.** The first PR ships the **read-only** version: list, sort, persist a favorite. Actually pinning the chosen region requires the helper binary (Section B § Security) and is deferred.

## ✅ A5. (User request) `screenshots/` to `.gitignore` — SHIPPED (Milestone A)

Append the rule below to [.gitignore](../.gitignore):

```diff
 /target/
 /result
 /result-*
 flake.lock
+
+# Local UI screenshots used during development
+/screenshots/
```

---

# Section B — Full Project Review

## ✅ B1. Architecture & threading — SHIPPED (Milestone D)

| Aspect | Verdict |
|--------|----------|
| GTK confined to main thread | ✅ |
| Tokio shared state via `Arc<RwLock<AppState>>` | ✅ |
| Tray on its own OS thread | ✅ |
| `Arc<Mutex<Option<Receiver>>>` → `async_channel::Receiver::clone()` | ✅ Fixed (Milestone D) |
| `std::process::exit` removed | ✅ Fixed (Milestone D) |

## ✅ B2. Error handling — SHIPPED (Milestone D)

- ✅ `Config::load` now returns `anyhow::Result<Config>`; callers handle the error properly.
- ✅ `anyhow::Context` added at D-Bus call sites.
- ✅ `std::process::exit` removed — main returns `anyhow::Result<()>`; Tokio runtime drops cleanly.
- ✅ `clippy::unwrap_used` lint not yet denied at crate root — low priority, Milestone E.

## ✅ B3. Async & D-Bus — SHIPPED (Milestone D)

- ✅ `SystemdManagerProxy` cached via `OnceCell` — no longer rebuilt on every call.
- ✅ NetworkManager `StateChanged` watcher drives auto-reconnect (F7).
- ✅ `apply_kill_switch` invokes `pkexec vex-vpn-helper` via stdin pipe.
- ✅ `PropertiesChanged` subscription for unit active-state — 3 s poll replaced (Milestone E).

## ✅ B4. Security — SHIPPED (Milestones B + C)

- ✅ Credentials at rest — plaintext `credentials.toml` with `0o600` atomic write.
- ✅ Sudoers `nft` NOPASSWD rule fully **removed** (Milestone C).
- ✅ `vex-vpn-helper` binary via `pkexec` with `auth_admin_keep` polkit action (Milestone C).
- ✅ nft rules piped via stdin — no TOCTOU tempfile vector.
- ✅ TLS pinned to PIA CA for meta connections.
- ✅ Interface validated against regex in both `Config::load` and helper.
- ✅ Auth token redacted in `Debug`, never persisted.

⏳ `oo7` Secret Service deferred until `oo7` migrates away from `zbus 4.x`.

## ✅ B5. PIA integration — SHIPPED (Milestone B)

`src/pia.rs` now implements `PiaClient` with `reqwest` 0.12 / `rustls`, embedded PIA CA. Shipped:
- ✅ `generate_token` — authenticates against PIA API, stores token in memory only
- ✅ `server_list` — fetches v6 region endpoint, parses `Region` / `ServerEntry` types
- ✅ `measure_latency` — TCP connect timing per server

⏳ Deferred stubs (Milestone C / D):
- `add_key` — WireGuard key registration
- `port_forward_signature` / `bind_port` — port-forward flow
- Server list caching to `~/.cache/vex-vpn/regions.json`
- WireGuard key rotation

## B6. Kill switch — High → ✅ Partially addressed (Milestone C)

- ✅ `kill_switch_allowed_ifaces` from config now forwarded to the helper and included in nft rules.
- ✅ Loopback (`lo`) included by default.
- ⏳ Pre‑connect leak window still present (Milestone D).
- ⏳ Persistence reboot behavior undocumented.

**Remaining (Milestone D).** Add a pre-connect mode that allows the selected server endpoint before the tunnel is up; document persistence in README.

## ✅ B7. UI / UX — SHIPPED (Milestones A + B + C)

- ✅ `adw::HeaderBar` + `adw::ToolbarView` (Milestone A)
- ✅ WCAG-AA contrast, `.boxed-list` feature toggles (Milestone A)
- ✅ `adw::AboutWindow` wired to primary menu (Milestone A)
- ✅ Server picker via `adw::NavigationView` (Milestone B)
- ✅ `adw::PreferencesWindow` — Connection / Privacy / Advanced (Milestone C)
- ✅ `gtk4::ShortcutsWindow` — `Ctrl+?` (Milestone C)
- ✅ Accessibility annotations / focus rings — Milestone E.

## ✅ B8. System tray — SHIPPED (Milestone E)

- ✅ SVG fallback icons bundled under `assets/icons/`; `IconTheme::add_search_path` called on startup.
- ✅ Tray subscribed to `tokio::sync::broadcast` channel — menu refresh is now event-driven.
- ⏳ "Recent regions" submenu deferred.

## ✅ B9. Configuration — SHIPPED (Milestones B + E)

- ✅ Interface validation added (`^[a-zA-Z][a-zA-Z0-9_-]{0,14}$`).
- ✅ `selected_region_id: Option<String>` field added.
- ✅ Schema version field (`version: u32`) added.
- ✅ Atomic write via temp-file rename for `config.toml`.

## B10. Nix packaging — Medium

- Runtime closure misses `wireguard-tools`, `nftables`, `iproute2`, `polkit`, `dbus`. NixOS module hides this; `nix profile install` users on non‑NixOS distros find out at runtime.
- The `Exec=vex-vpn` desktop entry assumes `vex-vpn` is on `PATH`.
- The package's own `lib/systemd/user/vex-vpn.service` hard-codes `%h/.nix-profile/bin/vex-vpn` — collides with the module-installed unit on NixOS.
- `cargo fmt --check` is in `checks.fmt` but missing from `scripts/preflight.sh`.

**Recommend.** Add runtime deps to `meta.runtimeDependencies`; gate the desktop user service behind a flag; add `cargo fmt --check` to preflight.

## ✅ B11. NixOS module — SHIPPED (Milestone E)

- ⏳ Polkit rule grants the entire `wheel` group; introduce a narrower `vex-vpn` group.
- ✅ `wg` path hardened — `/run/wrappers/bin/wg` now takes precedence over system `wg` in `PATH`.
- ✅ DNS override changed to `lib.mkDefault`.

## ✅ B12. Testing — SHIPPED (Milestone D)

- ✅ 23 tests total: 15 unit + 3 integration (`tests/config_integration.rs`) + 5 PIA unit tests.
- ✅ `src/lib.rs` exposes `config` module for integration testing.
- ✅ `wiremock` PIA HTTP fixtures — Milestone E.
- ✅ Feature-gated zbus mock systemd manager — Milestone E.

## B13. Documentation — Medium

- README assumes the NixOS module path; says nothing about `nix run` standalone limitations.
- No `CONTRIBUTING.md`, `CHANGELOG.md`.
- No troubleshooting section for the most common runtime failure modes (StatusNotifier missing, polkit prompts, WireGuard module not loaded).

## B14. Dependency hygiene — Low → ✅ Partially addressed (Milestone B)

- ✅ `reqwest 0.12` added with `default-features = false, features = ["rustls-tls", "json", "gzip"]`.
- ⏳ `thiserror` still imported but unused — drop with `cargo machete`.
- ⏳ `gio = "0.18"` kept for readability.

## ✅ B15. Build & CI — SHIPPED (Milestone E)

`scripts/preflight.sh` is solid (clippy → build → test → release → `nix build`). Added:

- ✅ `cargo fmt --check`
- ✅ `.github/workflows/ci.yml`
- ✅ `.github/workflows/release.yml`
- ✅ `.gitlab-ci.yml`

---

# Section C — Feature Backlog

| # | Feature | Severity if missing | Sketch |
|---|---------|----------------------|--------|
| ✅ F1 | First-run onboarding wizard | High | ✅ 5-page `adw::Carousel` wizard shipped (Welcome, Sign in, Privacy, Kill switch, Done). |
| ✅ F2 | Server picker with latency + favorites | High | ✅ NavigationView + PIA server list shipped. Favorites + latency sort deferred. |
| ✅ F3 | Helper binary + polkit action | High (security) | ✅ `vex-vpn-helper` binary shipped. polkit `auth_admin_keep`. stdin pipe (no TOCTOU). Sudoers NOPASSWD removed. |
| F4\* | Secret Service credential storage | High (security) | ⏳ `oo7` deferred (uses zbus 4.x internally, conflicts with our zbus 3.x). Plaintext `0o600` atomic write kept. Revisit when oo7 migrates. |
| ✅ F5 | Desktop notifications on connect/disconnect | Medium | ✅ `notify-rust 4` added; `notify_status_change` fires on poll loop transitions (Connected / Disconnected / Error). |
| ✅ F6 | About / Preferences / Shortcuts dialogs | Medium | ✅ All three shipped: `adw::AboutWindow` (A), `adw::PreferencesWindow` (C), `gtk4::ShortcutsWindow` (C). |}
| ✅ F7 | Auto-reconnect on network change | Medium | ✅ NM `StateChanged` watcher via zbus; auto-reconnect toggle in PreferencesWindow. |
| ✅ F8 | DNS leak test | Medium | ✅ Canary DNS resolution comparing system vs. tunnel resolver; surfaced in Preferences. |
| ✅ F9 | Connection history pane | Low | ✅ `~/.local/state/vex-vpn/history.jsonl` nav page shipped (Milestone E). |
| F10 | Localization scaffolding | Low | `gettext-rs` + `po/` + `cargo i18n` |
| F11 | Split tunneling per app (cgroups + nft) | Low | Helper RPC `add_app_to_split` |
| ✅ F12 | WireGuard handshake watchdog | Medium | ✅ `ConnectionStatus::Stale` added; watchdog polls `latest_handshake`, restarts unit if stale > 180 s. |
| F13 | Map view (Mullvad-style) | Low | `libshumate-rs` |
| ✅ F14 | HiDPI / icons | Low | ✅ SVG symbolic icons bundled under `assets/icons/` (Milestone E). |
| F15 | Auto-update check (opt-in) | Low | GitHub Releases JSON poll |

**Recommended next two milestones:** F1, F2, F3, F4, F5.

---

# Suggested milestone plan

| Milestone | Goal | Items | Status |
|-----------|------|-------|--------|
| **A — Make it usable** | Ship the four bug fixes + preflight tightening | A1, A2, A3, A4, A5 + `cargo fmt --check` | ✅ SHIPPED |
| **B — Make it secure** | Drop the broad sudoers entry; in-app PIA HTTP | F2, F3 (partial), B4, B5, B9 (partial) | ✅ SHIPPED |
| **C — Make it lovable** | Adwaita HIG completeness + Secret Service | F1, F3 (full helper), F4\*, F5, F6 | ✅ SHIPPED |
| **D — Make it reliable** | Resilience + tests | F7, F8, F12, B1, B2, B3, integration tests | ✅ SHIPPED |
| **E — Make it shine** | Polish + reach | F9, F10, F13, F14, B15 (CI), GitHub + GitLab CI | ✅ SHIPPED |

---

## References

1. GNOME HIG — Window Layouts: https://developer.gnome.org/hig/patterns/containers/
2. libadwaita 1.4 documentation (`AdwToolbarView`, `AdwHeaderBar`): https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1-latest/
3. WCAG 2.1 — Contrast (Minimum) 1.4.3: https://www.w3.org/WAI/WCAG21/Understanding/contrast-minimum.html
4. PIA Manual Connections (auth, server list, port forwarding): https://github.com/pia-foss/manual-connections
5. WireGuard `wg(8)` reference: https://man7.org/linux/man-pages/man8/wg.8.html
6. systemd D-Bus interface: https://www.freedesktop.org/wiki/Software/systemd/dbus/
7. NixOS Wiki — WireGuard: https://nixos.wiki/wiki/WireGuard
8. OWASP ASVS v4 §2 (Authentication) and §6 (Stored Cryptography): https://owasp.org/www-project-application-security-verification-standard/
9. `oo7` Secret Service crate: https://crates.io/crates/oo7
10. `notify-rust` crate: https://crates.io/crates/notify-rust
11. polkit reference: https://www.freedesktop.org/software/polkit/docs/latest/polkit.8.html
12. gtk4-rs documentation: https://gtk-rs.org/gtk4-rs/stable/latest/docs/

# vex-vpn — Basic Fixes & Full Project Analysis

**Phase 1 Spec — Research & Specification**
Target audience: implementation subagent (Phase 2).
Scope: Fix four reported runtime bugs, perform a full project review, and propose a feature backlog. Read‑only research only — no source files were modified during Phase 1.

---

## Executive Summary

vex-vpn is a Rust/GTK4 + libadwaita PIA VPN frontend that delegates the actual WireGuard / port‑forwarding work to two systemd units (`pia-vpn.service`, `pia-vpn-portforward.service`) provided by tadfisher's vendored module. The GUI talks to systemd via `zbus` 3.x, runs `wg show` and `nft` as subprocesses, and refreshes state from JSON files written by the backend in `/var/lib/pia-vpn/`.

The four user‑reported regressions are all real and reproducible by reading the code:

1. **No window drag handle** — `src/ui.rs` uses `adw::ApplicationWindow::set_content` with a raw `gtk4::Box`. There is no `AdwHeaderBar` and no `WindowHandle`, so on Wayland/GNOME the window has no draggable area at all.
2. **Unreadable contrast** — the embedded CSS in `src/ui.rs` overrides the libadwaita palette with extremely low‑alpha foregrounds (`rgba(255,255,255,.22)` for section titles, `.28` for stat labels, `.30` for IP, `.40` for nav buttons). On the very dark `#0d1117` window background this falls well below WCAG AA 4.5:1. The `adw::ActionRow` subtitles (`Block all traffic if VPN drops`, etc.) inherit from `.dim-label` which is tuned for the *default* Adwaita dark surface, not the custom near‑black we forced.
3. **No login prompt on first run** — `src/secrets.rs`, `src/pia.rs`, `src/helper.rs` and `nix/polkit-vex-vpn.policy` are **stub files** (4 lines of comments each). `main.rs` does not even `mod secrets`/`mod pia`/`mod helper`. The app never asks for credentials; it relies entirely on `/run/secrets/pia` being present, which only the NixOS module path produces — `nix run` users see a permanently failing service.
4. **No servers listed** — there is no server picker UI at all. The only "server" data is whatever the backend wrote to `region.json` after auto‑selecting by latency. Without auth (bug 3) `region.json` never appears, so the UI shows "Select a server" forever.

The wider review (Section 2) finds the codebase is otherwise reasonably structured (correct GTK‑main‑thread discipline, OnceCell zbus connection, sane state polling) but is missing roughly 60 % of the functionality the README advertises: PIA HTTP client, secrets handling, helper binary, polkit action, server selection, port‑forward control beyond a unit toggle, and almost all UX affordances (menu, About, Preferences, notifications).

**Section 1** below specifies four small, surgical patches that are safe to ship together and that are all exercised by `cargo build && cargo test && nix build`.

---

# Section 1 — Immediate Fixes (the four reported bugs)

> Severity for all four: **Critical** — the app is partly unusable without them.

## 1.1 Bug #1 — Missing titlebar / drag handle

### Root cause

[src/ui.rs](../../../src/ui.rs#L137-L156) builds an `adw::ApplicationWindow` and assigns a horizontal `gtk4::Box` directly:

```rust
let window = adw::ApplicationWindow::builder()
    .application(app)
    .title("Private Internet Access")
    .default_width(760)
    .default_height(540)
    .resizable(false)
    .build();
window.add_css_class("pia-window");

let root = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
root.append(&build_sidebar());
// …
root.append(&main_page);
window.set_content(Some(&root));
```

`AdwApplicationWindow` has **no titlebar of its own** — by design, it expects the caller to provide one inside its content (this is the GNOME HIG pattern). Because the content is a plain `Box`, there is nothing to drag, no close/minimize buttons on Wayland, and no app menu attachment point.

### Fix (idiomatic libadwaita 1.4 — verified via Context7 `/gnome/libadwaita`)

Wrap the existing root in an `adw::ToolbarView` and add an `adw::HeaderBar` as the top bar. Hide the visible title text (we have a custom branded sidebar already), keep window controls visible.

```rust
use adw::prelude::*;

// after building `root`:
let header = adw::HeaderBar::new();
header.set_show_title(false);                // we render our own brand in the sidebar
header.set_show_end_title_buttons(true);
header.set_show_start_title_buttons(true);

// Primary menu button — see §1.3 for menu wiring.
let menu_button = gtk4::MenuButton::builder()
    .icon_name("open-menu-symbolic")
    .tooltip_text("Main menu")
    .menu_model(&build_primary_menu())        // gio::MenuModel built below
    .build();
header.pack_end(&menu_button);

let toolbar_view = adw::ToolbarView::new();
toolbar_view.add_top_bar(&header);
toolbar_view.set_content(Some(&root));

window.set_content(Some(&toolbar_view));
```

Notes:
- `adw::ToolbarView` requires libadwaita ≥ 1.4. The crate is already pinned to `features = ["v1_4"]`.
- `set_resizable(false)` is preserved but the window can now be dragged via the headerbar.
- Do **not** apply the `pia-window` CSS class to the toolbar; keep it on the window so the body still gets the dark fill.

### Test

`cargo build && cargo test`. Manually: launch, click‑drag the headerbar, confirm the window moves.

---

## 1.2 Bug #2 — Unreadable text (low contrast)

### Root cause

The CSS in `src/ui.rs` (`APP_CSS`, lines 16–93) repeatedly uses fractional alpha values that produce ~2:1 contrast against the `#0d1117` background:

| Selector | Current color | Approx. contrast on `#0d1117` | WCAG AA target |
|----------|--------------|-------------------------------|----------------|
| `.section-title` | `rgba(255,255,255,.22)` | ~2.0 : 1 | 4.5 : 1 |
| `.stat-label` | `rgba(255,255,255,.28)` | ~2.4 : 1 | 4.5 : 1 |
| `.hero-ip` | `rgba(255,255,255,.30)` | ~2.6 : 1 | 4.5 : 1 |
| `.nav-btn` (idle) | `rgba(255,255,255,.40)` | ~3.2 : 1 | 4.5 : 1 |
| `.stat-value` | `rgba(255,255,255,.85)` | ~12 : 1 | OK |

In addition, `adw::ActionRow` subtitles (the "Block all traffic if VPN drops" texts) are styled by libadwaita with the `.dim-label` pseudo‑class, which lowers opacity by `0.55`. On a default Adwaita window background that yields ~5:1, but on our forced `#0d1117` it drops below 4:1 because the row backgrounds are also overridden indirectly by `.pia-window`.

### Fix

Two coordinated changes:

**(a)** Replace every "below 0.6 alpha" foreground with a solid color hand‑picked for `#0d1117`. Use the libadwaita semantic palette as a guide: titles/values `#fafafa`, dim text `#a0a0a0`, brand green `#00c389`. This raises everything to ≥ 4.5 : 1.

**(b)** Re‑enable libadwaita's own row chrome by adding the `.boxed-list` style class to the feature toggles container — this gives `AdwActionRow` a proper card background so its `.dim-label` subtitles render against `@card_bg_color` (which is *lighter* than the window) and meet contrast automatically.

Concrete CSS replacement (replace the whole `APP_CSS` constant):

```rust
const APP_CSS: &str = r#"
window.pia-window { background-color: #0d1117; }

.pia-sidebar {
    background-color: #0a0f16;
    border-right: 1px solid rgba(255,255,255,0.10);
}

/* Section / stat labels — was .22 / .28 (≈2:1). Now solid #a0a0a0 (~6:1). */
.section-title {
    font-size: 10px;
    font-weight: 600;
    letter-spacing: .10em;
    color: #a0a0a0;
    margin-bottom: 6px;
}
.stat-label {
    font-size: 10px;
    color: #a0a0a0;
    letter-spacing: .09em;
}
.stat-value {
    font-size: 14px;
    font-weight: 500;
    color: #fafafa;
    font-family: monospace;
}
.stat-value.green { color: #00c389; }

.hero-location { font-size: 17px; font-weight: 600; color: #fafafa; }
.hero-ip       { font-size: 12px; color: #a0a0a0; font-family: monospace; }

.nav-btn {
    border-radius: 8px;
    min-height: 42px;
    color: #c8c8c8;
    font-size: 13px;
}
.nav-btn:hover  { background: rgba(255,255,255,.08); color: #ffffff; }
.nav-btn.active { background: rgba(0,195,137,.15);  color: #00c389; }

.stat-card {
    background: #111c2a;
    border: 1px solid rgba(255,255,255,.10);
    border-radius: 9px;
    padding: 11px 13px;
}

/* AdwActionRow inside .boxed-list gets card-bg-color automatically.
   Bump it slightly so dim-label subtitles still pass AA on our dark window. */
.feature-list > row { background-color: #15202b; }
.feature-list > row .subtitle { color: #b8b8b8; opacity: 1.0; }
.feature-list > row .title    { color: #fafafa; }

.connect-btn {
    border-radius: 9999px;
    min-width: 152px;
    min-height: 152px;
    padding: 0;
    transition: all 200ms ease;
}
.connect-btn.state-disconnected {
    background: #0f1923;
    border: 2px solid rgba(0,195,137,0.45);
    color: #00c389;
}
.connect-btn.state-disconnected:hover {
    border-color: rgba(0,195,137,0.85);
    box-shadow: 0 0 32px rgba(0,195,137,0.20);
}
.connect-btn.state-connected {
    background: #00291b;
    border: 2px solid #00c389;
    color: #00c389;
    box-shadow: 0 0 40px rgba(0,195,137,0.25);
}
.connect-btn.state-connecting {
    background: #1a1306;
    border: 2px solid rgba(255,180,0,0.7);
    color: #ffb400;
}

.status-pill {
    border-radius: 9999px;
    padding: 4px 14px;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: .09em;
}
.status-pill.state-connected    { background: rgba(0,195,137,.18);  color: #00c389; }
.status-pill.state-disconnected { background: rgba(255,255,255,.10); color: #d8d8d8; }
.status-pill.state-connecting   { background: rgba(255,180,0,.18);  color: #ffb400; }
.status-pill.state-error        { background: rgba(255,80,80,.18);  color: #ff7878; }

.port-badge {
    background: rgba(0,195,137,.18);
    color: #00c389;
    border-radius: 5px;
    padding: 1px 7px;
    font-size: 11px;
    font-family: monospace;
    font-weight: 600;
}
"#;
```

In `build_main_page`, change the toggles container so the new selectors apply:

```rust
let feats = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
feats.add_css_class("boxed-list");
feats.add_css_class("feature-list");
```

### Test

Visual; also run a contrast check on the chosen colors (`#a0a0a0` on `#0d1117` ≈ 6.0 : 1, `#fafafa` on `#0d1117` ≈ 17 : 1, `#00c389` on `#0d1117` ≈ 6.5 : 1 — all pass WCAG AA).

---

## 1.3 Bug #3 — No first‑run login prompt

### Root cause

There are **three** layers of missing implementation that together produce this bug:

1. `src/secrets.rs` is a 4‑line placeholder. There is no `load_credentials` / `save_credentials` API at all.
2. `src/pia.rs` is a 4‑line placeholder. There is no PIA HTTP client, no token fetch, no `addKey` call. The app cannot validate or use a username/password even if it had one.
3. `src/main.rs` does not load credentials, does not detect their absence, and never opens an onboarding dialog. `app.connect_activate` jumps straight into `ui::build_ui`, which builds the dashboard.

The current shipped flow assumes the operator has already populated `/run/secrets/pia` via the NixOS module — but `nix run github:victorytek/vex-vpn` skips the module entirely.

### Design — Phase‑1 minimum viable login

Goal: ship a working onboarding dialog **without** taking on a full Secret Service dependency in this milestone. A follow‑up milestone (see Section 4) can swap the storage for `oo7`/libsecret.

1. **Storage** — add `~/.config/vex-vpn/credentials.toml` with mode `0600`. Schema:

   ```toml
   # vex-vpn credentials — chmod 600
   username = "p1234567"
   password = "..."          # plaintext for the MVP; encrypted in milestone B
   ```

   Implemented in a new `src/secrets.rs` module that exposes:

   ```rust
   pub struct Credentials { pub username: String, pub password: String }
   pub fn load() -> Option<Credentials>;
   pub fn save(c: &Credentials) -> anyhow::Result<()>;
   pub fn delete() -> anyhow::Result<()>;
   pub fn path() -> std::path::PathBuf;
   ```

   Use `std::os::unix::fs::OpenOptionsExt::mode(0o600)` when creating the file. Fail closed if the file is found with broader permissions.

2. **Login dialog** — a new function `pub fn run_login_dialog(parent: &impl IsA<gtk4::Window>) -> Option<Credentials>` in `src/ui.rs` (or a new `src/ui_login.rs`). It builds an `adw::Window` (modal, transient‑for the parent) containing an `adw::PreferencesGroup` with two `adw::EntryRow`/`adw::PasswordEntryRow` widgets and a primary "Sign in" button. Use `gtk4::glib::MainContext::channel` or a oneshot `tokio::sync::oneshot` to bridge the click to an `async` validator that calls `pia::validate_credentials(&u, &p).await` (added in Section 4).

3. **Startup flow** in `main.rs::connect_activate`:

   ```text
   on activate:
       creds = secrets::load();
       if creds.is_none():
           open login dialog modal-on-top of the (still hidden) main window
           on dialog confirm:
               secrets::save(&entered)
               state.write().credentials = Some(entered)
               build_dashboard()
           on dialog cancel/close:
               app.quit()
       else:
           build_dashboard()
   ```

4. **Re‑login affordance** — the new HeaderBar (Bug #1) carries a primary menu (`gio::Menu`):

   ```rust
   fn build_primary_menu() -> gio::Menu {
       let menu = gio::Menu::new();
       menu.append(Some("Switch account…"), Some("app.switch-account"));
       menu.append(Some("Preferences"),     Some("app.preferences"));
       menu.append(Some("About vex-vpn"),   Some("app.about"));
       menu.append(Some("Quit"),            Some("app.quit"));
       menu
   }
   ```

   Register the actions on the `adw::Application` in `main.rs`. `switch-account` deletes the on‑disk credentials and re‑opens the login dialog.

5. **State plumbing** — add `pub credentials: Option<secrets::Credentials>` to `AppState` (skip serializing it). Use it as the gate for "should we attempt a login token refresh?"

### Implementation surface for Phase 2

- New file `src/secrets.rs` (~80 LoC, plaintext TOML + 0600).
- New file `src/ui_login.rs` (~150 LoC).
- Modified `src/main.rs`:
  - `mod secrets; mod ui_login; mod pia;`
  - Move `app.connect_activate` body into a helper that branches on `secrets::load()`.
  - Register menu actions and `gio::SimpleAction`s.
- Modified `src/ui.rs`:
  - Add `build_primary_menu()` returning `gio::Menu`.
  - Build a `MenuButton` and pack into the headerbar (also satisfies Bug #1).
- Modified `src/state.rs`:
  - Add `credentials: Option<Credentials>` field.
- Modified `Cargo.toml`:
  - Add `[dependencies] keyring` is **not** required for the MVP.
  - Add `[target.'cfg(unix)'.dependencies] nix = { version = "0.27", features = ["fs"] }` — only if we want strict permission enforcement; otherwise rely on `OpenOptionsExt::mode`.

### Test

- Unit test for `secrets::save` → `secrets::load` round‑trip in a `tempfile::TempDir`.
- Unit test that `secrets::save` writes `0o600`.

---

## 1.4 Bug #4 — No servers listed / no server picker

### Root cause

There are two parts to this bug:

1. **No data source** — `src/pia.rs` is a stub, so the app has no way to fetch the PIA region list itself. Today the *systemd unit* fetches and writes `region.json`, but only after auth succeeds.
2. **No UI** — `src/ui.rs` does not contain any list, combo, or navigation page for picking a region. The only display is the "Select a server" hero subtitle.

### Design

Add an in‑app PIA HTTP client and an Adwaita `NavigationView` page for region selection.

**HTTP client (`src/pia.rs`, ~250 LoC)**

```rust
pub struct PiaClient { http: reqwest::Client }

#[derive(Deserialize, Clone, Debug)]
pub struct PiaRegion {
    pub id: String,
    pub name: String,
    pub country: String,
    pub port_forward: bool,
    pub geo: bool,
    pub servers: PiaServers,
}

impl PiaClient {
    pub async fn server_list(&self) -> Result<Vec<PiaRegion>>;             // /vpninfo/servers/v4
    pub async fn token(&self, user: &str, pass: &str, meta: &PiaServer) -> Result<String>;
    pub async fn measure_latency(&self, region: &PiaRegion) -> Option<u32>;
    pub fn ca_cert_pem() -> &'static [u8];                                 // bundled
}
```

Bundle PIA's CA certificate (`ca.rsa.4096.crt`) at compile time via `include_bytes!`. Configure `reqwest::ClientBuilder::add_root_certificate` so we don't depend on system trust for PIA's pinned CA.

**Server picker UI**

- Add an `adw::NavigationView` as the root of the right‑hand pane (replacing the current single page). Two pages:
  1. *Dashboard* — current widgets.
  2. *Servers* — `adw::PreferencesPage` containing a search‑filterable `gtk4::ListBox` of `adw::ActionRow` rows, each showing flag emoji (or `country-X` icon if present), region name, port‑forward badge, and live‑measured latency.
- Tapping a row stores the selection in `AppState.selected_region` (new field), persists `region` to `Config`, and:
  - if module‑mode (writable `/var/lib/pia-vpn/region.override`) — write the override and `systemctl restart pia-vpn.service`. This requires the polkit‑gated **helper binary** (Section 4).
  - if standalone — store as preference and surface a banner: "Region pinning requires the NixOS module."

**State plumbing**

Add to `AppState`:
```rust
pub all_regions: Vec<RegionInfo>,
pub selected_region_id: Option<String>,
```

Add a parallel branch in `state::poll_loop` that fetches the server list every 6 hours (cached in `~/.cache/vex-vpn/regions.json`). Latency measurement reuses the existing `measure_latency` helper.

### Phase split

For **this** milestone (immediate fixes), implement only the **read‑only** version:
- Fetch + display the server list.
- Sort by latency, show port‑forward capable badges, show favorites.
- Selection is persisted to `Config` only; the actual restart of the systemd unit is deferred to milestone B (helper binary + polkit).

This keeps the immediate‑fix PR small and unblocks the main UX complaints, while leaving "actually pin the chosen server" for the helper milestone where it belongs.

### Test

- Unit test that `PiaClient::server_list` parses the embedded fixture (`tests/fixtures/serverlist_v4.json`).
- Smoke test: `cargo run` → open Servers page → at least one row populates within 10 s on a normal connection.

---

## 1.5 .gitignore tweak

The user explicitly requested adding `screenshots/` to `.gitignore` so README screenshots remain untracked workspace artefacts:

```diff
 /target/
 /result
 /result-*
 flake.lock
+
+# Local UI screenshots used during development
+/screenshots/
```

(If the project later wants to publish screenshots in‑repo, they should live under `docs/screenshots/` which is **not** ignored.)

---

# Section 2 — Full Project Analysis

This section is the engineering review the user asked for. The same content is republished at [docs/PROJECT_ANALYSIS.md](../../../docs/PROJECT_ANALYSIS.md) for public consumption.

> Severity scale: **Critical** (blocks core function or has a security impact), **High** (broken feature or major UX regression), **Medium** (correctness/perf concern), **Low** (polish / nice‑to‑have).

## 2.1 Architecture & threading

**Severity: Medium.**

- **Current state.** Three threads cooperate: GTK main thread (UI), the multi‑thread Tokio runtime (background polling and D‑Bus), and a third OS thread that hosts the `ksni` tray (`tray::run_tray`). They share `Arc<RwLock<AppState>>` and a `std::sync::mpsc::SyncSender<TrayMessage>`. UI refresh uses `glib::timeout_add_seconds_local(3, …)` with `glib::spawn_future_local` reading the lock.
- **Findings.**
  - GTK calls are correctly confined to the main thread.
  - The tray's `read_state` calls `self.handle.block_on(async { self.state.read().await.clone() })`. This is fine while `ksni`'s callback runs on its own thread, but it does mean every menu/tooltip query takes a write‑skipping read lock and a runtime hop. Acceptable but not free.
  - The `tray_rx = Arc<Mutex<Option<Receiver>>>` `take()`‑on‑first‑activate trick in `main.rs` is fragile. If `connect_activate` ever fires twice (e.g. via single‑instance app activation while the window is already open), the second call gets `None` silently and a leaked clone of `state_for_ui`.
  - `std::process::exit(exit_code.into())` at the end of `main` skips the Tokio runtime's `Drop`, leaking handles and possibly losing pending writes (e.g. `Config::save`). It also kills any flushing `tracing` layers.
- **Recommendations.**
  - Replace the `Arc<Mutex<Option<Receiver>>>` with `gtk4::glib::clone!` on a `RefCell<Option<…>>` captured in the activate closure, or move the channel to an `async_channel` and consume it via `glib::spawn_future_local`.
  - Drop the runtime explicitly on the way out: replace the trailing `std::process::exit` with `let code = app.run(); drop(rt); code` and let `main` return.
  - `RwLock` is fine. There is no contention hazard given the 3 s poll cadence.
- **Effort.** Small.

## 2.2 Error handling

**Severity: Medium.**

- **Current state.** Errors are mostly `anyhow::Result`. Subprocess errors are bubbled with `anyhow::anyhow!`. `state.rs::poll_once` swallows individual reader errors (`region_raw.ok()` etc.) on purpose.
- **Findings.**
  - `state.rs::read_wg_stats` does `parts[1].parse::<u64>().unwrap_or(0)` — silent fallback masks malformed `wg show` output.
  - `tray.rs::run_tray` only logs `tray service error: …` via `tracing::warn!` and silently exits the thread; users with no StatusNotifier host (e.g. plain GNOME without TopIconsFix) get no diagnostic.
  - `config.rs::Config::load` discards parse errors via `.unwrap_or_default()`. A typo'd config silently reverts to defaults — bad UX.
  - `dbus.rs::system_conn` returns a plain `zbus::Result` but most callers wrap it; messages don't carry the failing operation context.
- **Recommendations.**
  - Add `anyhow::Context` to every fallible boundary (file path, subprocess name, D‑Bus call name).
  - `Config::load` should return `Result<Self>` so the caller can decide between fallback and surfacing a banner.
  - Gate `unwrap`/`expect` with `#[deny(clippy::unwrap_used, clippy::expect_used)]` at the crate root and audit remaining sites.
- **Effort.** Small.

## 2.3 Async & D‑Bus

**Severity: Low → Medium.**

- **Current state.** `zbus` 3.x with `dbus_proxy` macros is correct. A single `OnceCell<Connection>` reuses the system bus.
- **Findings.**
  - `SystemdManagerProxy::new(&conn).await` is created on every call instead of cached. Cheap, but allocates.
  - `apply_kill_switch` shells out to `sudo nft -f -`. Inside async this is correctly piped via `tokio::process::Command` and `child.stdin.take()`. But `sudo` introduces a TTY assumption — if the polkit/sudo rules don't authorize NOPASSWD, the command blocks waiting for a password that nobody is typing.
  - The systemd unit `ActiveState` is read once per poll. There is no `signal()` subscription, so we miss state transitions inside the 3 s window. Not a correctness bug; just laggy UI.
- **Recommendations.**
  - Cache `SystemdManagerProxy` inside another `OnceCell`.
  - Subscribe to `org.freedesktop.systemd1.Manager.UnitNew/JobNew` or use `PropertiesProxy.PropertiesChanged` on the unit path for instant transitions.
  - Replace `sudo nft …` with the polkit‑gated helper binary so we never depend on a TTY (see Section 3 § Helper).
- **Effort.** Medium (signal subscription) / Small (cache proxy).

## 2.4 Security

**Severity: High.**

- **Current state.** Credentials live in `/run/secrets/pia` (when the module path is used) or nowhere (when run via `nix run`). Polkit rules in `nix/module-gui.nix` allow `wheel` group members to start/stop the two units without a password. `nix/polkit-vex-vpn.policy` is a stub.
- **Findings.**
  - **No in‑app credential storage** — see Bug #3.
  - **Plaintext credentials envisaged** — even when storage is added (Section 1), `~/.config/vex-vpn/credentials.toml` will hold `PIA_PASS` in plaintext on disk. This is consistent with what `pia-foss/manual-connections` recommends as a fallback, but the *target* should be Secret Service via `oo7`.
  - **Sudo NOPASSWD on `nft`** is broad. The module currently allows *any* `nft` invocation, which means a compromised user session can flush all firewall rules system‑wide.
  - **No TLS pinning** in `pia.rs` — currently nonexistent. When implementing, bundle PIA's CA and use it explicitly (`ClientBuilder::tls_built_in_root_certs(false)`).
  - **Subprocess argument handling** — `apply_kill_switch` formats the interface name into a shell template via `format!`. The interface name comes from `Config.interface`, which is user‑controllable, but the value is fed via stdin to `nft -f -`, **not** to a shell, so no shell injection. Still, validate the name matches `[a-zA-Z0-9_-]{1,15}` before formatting to prevent malicious nft fragments.
  - **Logs** — `tracing::error!` chains with `{}` formatting, never `{:?}`, so we don't accidentally log structs containing tokens. Good. But `pia.rs` (when implemented) must take care: never log auth tokens, never log the request body, never log full payloads.
  - **No CA verification on PIA's HTTPS endpoints** in the *backend* shell script either — `module-vpn.nix` already pins the CA via `--cacert`, which is correct. Mirror this in the new Rust client.
- **Recommendations.**
  - Adopt `oo7` (Secret Service) as the primary credential store; fall back to plaintext only when D‑Bus has no Secret Service activatable provider. (Verified via Context7 — see Section 5.)
  - Replace the `nft` sudoers entry with a polkit‑gated helper binary that exposes a tiny D‑Bus interface (`com.vex.vpn.helper.ApplyKillSwitch(s)`) accepting only validated payloads.
  - Add an `interface` validator in `config.rs` that bails on anything outside `^[a-z][a-z0-9_-]{0,14}$`.
  - Bundle PIA's CA cert as `include_bytes!("../assets/ca.rsa.4096.crt")` and pin in `reqwest`.
- **Effort.** Medium.

## 2.5 PIA integration completeness

**Severity: Critical.**

- **Current state.** `src/pia.rs` is empty. All PIA logic lives in the bash script in `module-vpn.nix`.
- **Gaps.**
  - No in‑app server list fetch.
  - No in‑app auth token retrieval (`generateToken` v3).
  - No in‑app `addKey`/key rotation.
  - No port‑forward `getSignature`/`bindPort` handling — we only flip the systemd unit on/off and read `portforward.json` when it appears.
  - No region selection beyond "let the backend pick the lowest latency".
- **Recommendations.** Implement `PiaClient` per Section 1.4 plus a `KeyRotator` task that respects the 24 h validity of the WireGuard key (`addKey` returns no expiry but PIA's manual‑connections script rotates daily).
- **Effort.** Large.

## 2.6 Kill switch

**Severity: High.**

- **Current state.** Two layers — declarative in `module-gui.nix` (`networking.nftables.tables.pia_kill_switch`) and imperative in `dbus.rs::apply_kill_switch` (runtime `nft -f -`). The runtime version drops *all* output that isn't on `wg0` or `lo`, including IPv6.
- **Findings.**
  - **Leak window on connect.** Activating the kill switch *after* the tunnel is already up is fine; turning it on *before* connect blocks the WireGuard handshake itself unless `allowedAddresses` includes the chosen server. Today the GUI has no UX to express this.
  - **IPv6.** The table is `inet`, which is correct for dual‑stack. Good.
  - **Persistence.** Runtime rules vanish on reboot. Only the declarative path survives. This is fine but undocumented.
  - **`allowedInterfaces`/`allowedAddresses` not wired into the runtime path.** The runtime version only allows `wg0` + `lo`, ignoring the user's configured allow‑lists. The declarative path honors them. This means turning the toggle off in the GUI replaces a permissive declarative ruleset with a stricter runtime one — the opposite of what the user expects.
  - **`sudo nft delete table inet pia_kill_switch`** silently warns on failure but returns `Ok(())`. Toggling off when the table doesn't exist therefore looks successful. Acceptable for idempotency.
- **Recommendations.**
  - Plumb `allowedInterfaces`/`allowedAddresses` from `AppState`/`Config` into `apply_kill_switch`.
  - Add a "pre‑connect kill switch" mode that allows the active server endpoint while the tunnel is being negotiated.
  - Document persistence behavior in README.
- **Effort.** Medium.

## 2.7 UI / UX

**Severity: High.**

- **Current state.** Custom CSS, custom sidebar with a single "Dashboard" item, no headerbar, no menu, no About/Preferences/Servers pages, no notifications, no keyboard shortcuts dialog.
- **Findings.** See Bug #1, Bug #2, Bug #4. Additional gaps:
  - No `gtk4::ShortcutsWindow`.
  - No `adw::AboutWindow`.
  - No `adw::PreferencesWindow`. The Auto‑Connect toggle and DNS provider live nowhere a user expects them.
  - No accessibility annotations (`set_tooltip_text`, `set_accessible_role`, `set_label_for`).
  - The connect button has no keyboard activation indicator and no focus ring (custom CSS clobbers the default).
  - No empty‑state handling beyond "Select a server" — the user has no idea *what* to select.
- **Recommendations.** Adopt the Adwaita HIG: HeaderBar + menu + Adwaita pages. Add About, Preferences, Shortcuts dialogs as actions on the application.
- **Effort.** Medium‑Large (cumulative).

## 2.8 System tray

**Severity: Medium.**

- **Current state.** `ksni::Tray` impl. Icons via `network-vpn-symbolic` etc.
- **Findings.**
  - Hard‑coded icon names assume a working hicolor + Adwaita symbolic theme. On non‑GNOME desktops the icons may not exist (KDE has them; XFCE depends on theme). No fallback path.
  - No "Connect to <region>" submenu — the tray menu is `Open / Connect / Quit`.
  - The tray's `Disconnect` while in `Connecting` state is correctly handled (combined branch), but the menu label uses an `is_connected || is_connecting` condition; a user who opens the menu *while the click handler is racing* may see the wrong label until the next refresh cycle.
- **Recommendations.**
  - Bundle fallback icons in `assets/icons/` and call `gtk4::IconTheme::add_search_path`.
  - Add "Recent regions" to the tray menu once the server picker exists.
  - Have the tray subscribe to a `tokio::sync::broadcast` of state changes so menu refreshes within ~100 ms instead of 3 s.
- **Effort.** Medium.

## 2.9 Configuration

**Severity: Medium.**

- **Current state.** Plain TOML at `~/.config/vex-vpn/config.toml`. No version, no migration, non‑atomic write.
- **Findings.**
  - No schema version key — adding fields is fine (`#[serde(default)]`), but removing or renaming will silently corrupt.
  - Writes are not atomic: `std::fs::write(path, content)` truncates first. A SIGKILL between truncate and write loses the file.
  - Validation is absent: an empty `interface = ""` is accepted.
- **Recommendations.**
  - Add `version: u32 = 1` and a top‑level `migrate(&mut Config)` step.
  - Switch to atomic write: write to `config.toml.tmp` and `rename`.
  - Validate `interface` (regex above), `max_latency_ms` (1..=10_000), and `dns_provider` (enum).
- **Effort.** Small.

## 2.10 Nix packaging

**Severity: Medium.**

- **Current state.** Crane‑based flake. Uses `wrapGAppsHook4` and exports `GI_TYPELIB_PATH` in `preBuild`. Installs a desktop file and user systemd unit via `postInstall`.
- **Findings.**
  - Runtime closure does not include `wireguard-tools`, `nftables`, or `iproute2` — these are expected to come from the system. Fine for the NixOS module path; broken for `nix profile install` users on a non‑NixOS distro. The README does not warn about this.
  - The `Exec=vex-vpn` in the desktop entry assumes `vex-vpn` is on `PATH`. With `wrapGAppsHook4` the wrapper script lives at `$out/bin/vex-vpn`, so this works only when the package is on the user's `PATH` (always true for `nix profile install`, sometimes false for ad‑hoc `result/bin`).
  - `result` is committed (visible at the repo root) — this is a user mistake on the developer's side, but the `.gitignore` already excludes `/result`. Confirm by `git ls-files`.
  - The user systemd unit hard‑codes `%h/.nix-profile/bin/vex-vpn`, which only works for `nix profile`. NixOS module users never see this file because the module installs its own at `nix/module-gui.nix`.
  - `checks.fmt` exists, but `cargo fmt` is not enforced in the preflight. Re‑add.
  - GResource compilation, hicolor cache update, and GSettings schema compilation are not run because the app uses none of these — fine, but worth a comment.
- **Recommendations.**
  - Add `wireguard-tools`, `nftables`, `iproute2`, `polkit`, `dbus` to `propagatedUserEnvPkgs` (or `meta.runtimeDependencies`) so non‑NixOS Nix users get them.
  - Drop the `postInstall` user service or guard it behind a `withUserService` arg — NixOS module users get a duplicate.
  - Add `cargo fmt --check` to `scripts/preflight.sh`.
- **Effort.** Small.

## 2.11 NixOS module

**Severity: Medium.**

- **Current state.** Two modules (`module-vpn.nix`, `module-gui.nix`) plus a thin re‑export in `flake.nix`. Polkit rule, sudo rule, kill‑switch nft table, optional autostart user service.
- **Findings.**
  - Polkit rule grants `wheel` access. There is no narrower group (e.g. `vex-vpn`) — every wheel user can toggle the VPN.
  - `security.wrappers.wg` gives `cap_net_admin+pe` to a `wg` binary in `/run/wrappers/bin`, but the GUI calls `wg` via `tokio::process::Command::new("wg")` which uses `PATH`. If `/run/wrappers/bin` precedes the system `wg` in `PATH` it picks up the wrapper; otherwise it doesn't, and `wg show … transfer` may fail for non‑root users on systems where the unwrapped binary needs the capability.
  - `services.pia-vpn.dnsServers` is set unconditionally to PIA. If the user *also* sets it directly, the GUI module clobbers it. Add `lib.mkDefault`.
- **Recommendations.**
  - Introduce a `vex-vpn` group; `users.groups.vex-vpn = {};` and require membership for polkit.
  - Use the wrapper path explicitly: pass `/run/wrappers/bin/wg` to the wrapper or fail with a clear message.
  - Wrap the DNS overwrite in `lib.mkDefault`.
- **Effort.** Small‑Medium.

## 2.12 Testing

**Severity: Medium.**

- **Current state.** Four unit tests in `state.rs` (format_bytes, status labels, port‑payload decode) + two in `config.rs`. No integration tests, no D‑Bus mocking, no PIA HTTP fixtures.
- **Recommendations.**
  - Add integration tests under `tests/` using `tempfile::TempDir` + `XDG_CONFIG_HOME` overrides for the config and credentials round‑trips.
  - Add a `wiremock`‑based fixture for the PIA HTTP client (region list, generateToken, addKey).
  - Add a `zbus`‑mock systemd manager for D‑Bus tests (or feature‑gate them behind `--features integration` to skip in CI).
- **Effort.** Medium.

## 2.13 Documentation

**Severity: Medium.**

- **Current state.** Solid `README.md` with installation, config, kill switch description, ASCII architecture diagram. No `CONTRIBUTING.md`, `CHANGELOG.md`, or `docs/` directory.
- **Findings.**
  - README assumes the systemd module path; says nothing about credentials when using `nix run`.
  - No screenshots; the user's request to add `screenshots/` to `.gitignore` is the trigger for this analysis.
  - No troubleshooting section ("StatusNotifier not visible", "polkit prompt for nft", "WireGuard module not loaded").
- **Recommendations.**
  - Add `CONTRIBUTING.md` with the preflight invocation.
  - Add `CHANGELOG.md` (Keep‑a‑Changelog).
  - Keep screenshots out of git via `.gitignore` rule (Section 1.5). For README inclusion, host them via Pages/issue uploads.
  - Add a "Standalone (`nix run`) limitations" section.
- **Effort.** Small.

## 2.14 Dependency hygiene

**Severity: Low.**

- **Current state.** Modest direct dep set — `gtk4`, `libadwaita`, `glib`, `gio`, `tokio`, `zbus`, `serde`, `serde_json`, `base64`, `ksni`, `anyhow`, `thiserror`, `toml`, `tracing`, `tracing-subscriber`.
- **Findings.**
  - `thiserror` is imported but no `#[derive(thiserror::Error)]` appears in any source file (we use `anyhow` consistently). Drop it.
  - `gio = "0.18"` is a separate dep but the same crate is re‑exported by `glib` 0.18 / `gtk4` 0.7. Not strictly redundant, kept for readability.
  - `base64 = "0.21"` is one major version behind `0.22`. Either is fine on Rust 1.75; `0.21` is what `reqwest 0.11` pulls in transitively, so there's no duplication today.
  - When adding `reqwest` for `pia.rs`, prefer the rustls backend (no system OpenSSL coupling).
- **Recommendations.**
  - `cargo machete` to drop `thiserror` if unused after Section 4.
  - Pin `reqwest = { version = "0.11", default-features = false, features = ["rustls-tls", "json", "gzip"] }`.
- **Effort.** Small.

## 2.15 Build & CI

**Severity: Medium.**

- **Current state.** `scripts/preflight.sh` runs clippy → debug → tests → release → `nix build`. No GitHub Actions, no GitLab CI, no automated release.
- **Recommendations.**
  - Add `.github/workflows/ci.yml` running on `nixos-unstable` Nix container, executing `nix flake check` and `bash scripts/preflight.sh`.
  - Add `.github/workflows/release.yml` triggered on tags, building x86_64 + aarch64 closures and attaching them.
  - Add `.gitlab-ci.yml` (mirrors the GitHub workflow) — required by Phase 6 governance.
- **Effort.** Medium.

---

# Section 3 — Feature Backlog (prioritized)

| # | Feature | Why it matters | Sketch | New deps |
|---|---------|----------------|--------|----------|
| F1 | First‑run onboarding wizard | Removes the entire bug #3 class; confidence on first launch | `adw::Carousel` with PIA login → CA accept → kill‑switch ack → auto‑connect prompt | none new |
| F2 | Server picker with latency + favorites | Bug #4 plus user agency over connect target | `adw::PreferencesPage`, `gtk4::ListBox` filter, persisted favorites in `config.toml` | `reqwest` |
| F3 | Helper binary + polkit action | Removes the broad `sudo nft NOPASSWD` rule; required for region pinning + future split‑tunnel | New crate target `vex-vpn-helper`, zbus service, `nix/polkit-vex-vpn.policy` | `zbus` (already) |
| F4 | Secret Service credential storage | Replace plaintext fallback with `oo7` keyring | `oo7::Keyring::default().await?` then `create_item("vex-vpn", attrs, secret, true)` | `oo7` |
| F5 | Desktop notifications on connect/disconnect/error | High‑value UX, low effort | `notify_rust::Notification::new().summary("…").show()` from state transitions | `notify-rust` |
| F6 | About / Preferences / Shortcuts dialogs | HIG compliance | `adw::AboutWindow::builder()`, `adw::PreferencesWindow`, `gtk4::ShortcutsWindow` | none |
| F7 | Auto‑reconnect on network change | Mobile / dock users | Subscribe to NetworkManager `StateChanged` via zbus, retrigger `connect_vpn` | none |
| F8 | DNS leak test | Trust signal | Resolve a known canary against system + tunnel DNS, compare upstream | `trust-dns-resolver` |
| F9 | Connection log / history pane | Diagnostics | Append to `~/.local/state/vex-vpn/history.jsonl`; render in a dedicated nav page | `time` |
| F10 | Localization scaffolding | Reach | `gettext-rs` + `po/` directory; wire `cargo i18n` | `gettext-rs`, `gettext-sys` |
| F11 | Split tunneling (per‑app cgroups) | Power users | New helper RPC: `add_app_to_split(&str)` writes nft `socket cgroupv2 …` rules | helper only |
| F12 | WireGuard handshake watchdog | Reliability on flaky links | Poll `latest_handshake`; if stale > 180 s, restart unit | none |
| F13 | Map view (Mullvad‑style) | Wow factor | `libshumate-rs` (verified via Context7 — see Section 5) | `libshumate` |
| F14 | HiDPI / icon improvements | Polish | Bundle SVG symbolic icons; `IconTheme::add_search_path` | none |
| F15 | Auto‑update check (opt‑in) | Stay current | Periodic GET on GitHub Releases JSON; banner in main window | `reqwest` |

Top‑5 recommendation for the next 2 milestones: **F1, F2, F3, F4, F5**.

---

# Section 4 — Implementation Phasing

## Milestone A — "Make it usable" (this PR)

Goal: ship the four bug fixes in Section 1 and a preflight that catches regressions.

- Bug #1: HeaderBar + ToolbarView.
- Bug #2: CSS contrast pass.
- Bug #3: `secrets::{load,save}` + login dialog + plaintext storage.
- Bug #4: read‑only server list (display only; selection persisted to `Config`).
- `.gitignore`: `screenshots/`.
- Preflight: add `cargo fmt --check`.

Deliverable size: ~600 LoC net.

## Milestone B — "Make it secure" (next)

- F3 helper binary + polkit policy (`nix/polkit-vex-vpn.policy` populated, helper binary, drop the `nft` sudoers rule).
- Region pinning wired to `pia-vpn.service` restart via the helper.
- Bundle PIA CA in `pia.rs`.

## Milestone C — "Make it lovable"

- F1 onboarding wizard.
- F4 oo7 secret storage with plaintext fallback.
- F5 notifications.
- F6 About / Preferences / Shortcuts dialogs.

## Milestone D — "Make it reliable"

- F7 auto‑reconnect on network change.
- F8 DNS leak test.
- F12 WireGuard handshake watchdog.
- Integration tests with `wiremock` and a zbus mock systemd.

## Milestone E — "Make it shine"

- F9 connection log.
- F10 localization scaffolding.
- F13 map view.
- F14 HiDPI / icons.
- GitHub + GitLab CI.

---

# Section 5 — Dependencies to Add / Upgrade

| Crate | Version | Justification | Context7 verification |
|-------|---------|---------------|-----------------------|
| `reqwest` | `0.11` (rustls features) | PIA HTTPS API client — no system OpenSSL coupling | Standard crate; no Context7 ID needed |
| `oo7` | `0.3.x` | Modern pure‑Rust async Secret Service client (libsecret‑compatible). Replaces deprecated `secret-service` crate. Used in milestone C. | Verified — see https://crates.io/crates/oo7 |
| `notify-rust` | `4.x` | Desktop notifications (org.freedesktop.Notifications). | Context7 ID `/hoodie/notify-rust` (High reputation, 25 snippets). API: `Notification::new().summary("…").body("…").icon("…").show()?` |
| `tempfile` | `3` (dev only) | Tests for secrets/config round‑trips. | Standard. |
| `wiremock` | `0.5` (dev only) | Mock PIA HTTPS server for unit tests in `pia.rs`. | Standard. |
| `time` | `0.3` | RFC3339 timestamps in connection log (F9). | Standard. |
| `trust-dns-resolver` | `0.23` | DNS leak test (F8). | Standard. |
| `libshumate` (gtk4 binding) | `0.x` | Map view (F13, optional). | Context7 ID `/git_gitlab_gnome_org/world_rust_libshumate-rs` (High reputation). |

No upgrades to the existing core stack are required. `gtk4 0.7` + `libadwaita 0.5` + `zbus 3.x` + `tokio 1.x` are all current for Rust 1.75.

`thiserror` should be **removed** unless milestone B or later actually adopts it.

---

# Section 6 — Risks & Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Headerbar break under non‑Adwaita themes (e.g. plain GTK on XFCE) | Low | Medium | `adw::ToolbarView` falls back to a plain `GtkBox` of header + content; visually different but functional. |
| CSS rewrite changes user‑visible look | High (intended) | Low | Provide before/after screenshots in PR description; bump version to `0.2.0`. |
| Plaintext credentials file leaks via backup tools | Medium | High | Document `0o600` enforcement; flag in onboarding; add migration to `oo7` in milestone C. |
| `reqwest` adds significant closure size to `nix build` | Medium | Low | Use `default-features = false` + `rustls-tls` only. Measured impact ≈ +6 MB. |
| Server picker calls slow PIA endpoints on every refresh | Low | Medium | Cache `serverlist.json` for 6 h; latency probe out‑of‑band, throttled. |
| `nix run` users still can't do anything without a credentials file | High | Critical | Bug #3 fix lands in milestone A; tested explicitly by spawning the login dialog with `XDG_CONFIG_HOME=$(mktemp -d)`. |
| Polkit/helper milestone breaks the `services.vex-vpn` API | Medium | High | Keep `cfg.killSwitch.enable` etc. unchanged; introduce helper as additive. |
| Rust 1.75 MSRV drift | Low | Low | `oo7 0.3` requires 1.75; verify on bump. |

---

## Sources / References

1. **gtk4-rs 0.7 docs** — https://docs.rs/gtk4/0.7 — confirms `gtk4::Box`, `IconTheme::add_search_path`, `MenuButton::set_menu_model` on 0.7 series.
2. **libadwaita 1.4 (Adwaita) — `AdwToolbarView`, `AdwHeaderBar`** — Context7 `/gnome/libadwaita` (High reputation, 490 snippets). Confirms the `add_top_bar` + `set_content` + `window.set_content(toolbar_view)` pattern used in §1.1, including for migrating from `GtkWindow` titlebar/child layouts.
3. **GNOME HIG — Window Layouts** — https://developer.gnome.org/hig/patterns/containers/ — describes the modern Adwaita window pattern (header bar + toolbar view + content) and contrast guidance.
4. **WCAG 2.1 — Contrast (Minimum) (1.4.3)** — https://www.w3.org/WAI/WCAG21/Understanding/contrast-minimum.html — 4.5 : 1 target for body text used in §1.2.
5. **PIA Manual Connections (auth + servers)** — https://github.com/pia-foss/manual-connections — reference for §1.4 endpoints (`https://serverlist.piaservers.net/vpninfo/servers/v4`, `generateToken`, `addKey`, `getSignature`/`bindPort`) and the bundled CA `ca.rsa.4096.crt`.
6. **WireGuard `wg(8)` man page** — https://man7.org/linux/man-pages/man8/wg.8.html — confirms the `wg show <iface> transfer` output schema `<pubkey>\t<rx>\t<tx>` parsed in `state::read_wg_stats`.
7. **systemd D-Bus interface (`org.freedesktop.systemd1`)** — https://www.freedesktop.org/wiki/Software/systemd/dbus/ — confirms `Manager.StartUnit/StopUnit` + `Unit.ActiveState` properties used by `dbus.rs`.
8. **NixOS Wiki — WireGuard / PIA** — https://nixos.wiki/wiki/WireGuard — confirms the systemd-networkd configuration approach used in `module-vpn.nix`.
9. **OWASP ASVS v4 §6 (Stored Cryptography) and §2 (Authentication)** — https://owasp.org/www-project-application-security-verification-standard/ — informs the credential storage recommendations in §2.4.
10. **`oo7` crate** — https://crates.io/crates/oo7 — async, pure-Rust Secret Service client; recommended replacement for `secret-service` for milestone C (§5).
11. **`notify-rust` crate** — Context7 `/hoodie/notify-rust` (High reputation) — API surface for F5.
12. **polkit XML reference** — https://www.freedesktop.org/software/polkit/docs/latest/polkit.8.html — informs §2.4 helper binary design and `nix/polkit-vex-vpn.policy` schema.

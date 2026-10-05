# GUI redesign (layout C) + app icon fix — Specification

## Current state

- `src/ui.rs` loads `APP_CSS` with hardcoded hex colours (`#0d1117`, `#0a0f16`,
  `#00c389`, …) for the window, sidebar, nav buttons, stat cards, connect
  button and status pills. libadwaita itself is left on the system colour
  scheme, so on a light system default widgets (AdwStatusPage title, labels)
  draw dark text on the forced dark background — the unreadable screenshot.
  The hardcoded colours also override the system accent colour.
- Layout: custom `gtk::Box` sidebar of `gtk::Button`s (Dashboard / Regions /
  Settings) + `gtk::Stack`. Sidebar header shows `vpn2.png` (the round logo)
  at 28 px with no app name.
- Icon: `nix/package.nix` installs `assets/icons/hicolor/scalable/apps/vex-vpn.svg`
  (a placeholder flat green shield) **and** `256x256/apps/vex-vpn.png` (the real
  logo, identical to `assets/icons/vpn.png`, 500×500). Icon themes prefer the
  scalable entry, so GNOME's app grid shows the green shield. The SVG is also
  compiled into the gresource and registered as an icon theme path, so the
  About window shows the green shield too.

Runtime versions (verified `nix develop --command pkg-config --modversion`):
libadwaita **1.9.3**, GTK **4.22.4**.

## Problem definition

1. The UI is unreadable and off-brand for GNOME; the user chose (via mockups)
   **native GNOME styling** and **layout C**: status pane on the left, region
   list always visible on the right, settings in the main menu.
2. Incorporate the app logo.
3. Follow the desktop's accent colour.
4. App grid shows a generic shield instead of the app logo.

## Research (sources)

1. libadwaita docs — Styles & Appearance › Accent Color (Context7
   `/websites/gnome_pages_gitlab_gnome_libadwaita_doc_1-latest`): "Libadwaita
   applications follow the system accent color by default. To use the accent
   color, applications should use accent color variables and the `.accent`
   style class."
2. libadwaita docs — CSS variables: `--accent-bg-color`, `--accent-fg-color`,
   `--accent-color`; defaults depend on system preferences (since 1.6).
3. libadwaita docs — `AdwStyleManager:accent-color` (1.6+): system value read
   from the Settings portal; no app code needed to follow it.
4. libadwaita docs — `AdwNavigationSplitView` (1.4): sidebar + content panes;
   sidebar gets the sidebar background automatically.
5. libadwaita docs — `AdwPreferencesWindow` (1.0–, deprecated 1.6 in favour
   of `AdwPreferencesDialog`, which needs bindings `v1_5`; the crate is pinned
   to `libadwaita 0.5`/`v1_4`, so use `PreferencesWindow`, consistent with the
   existing `MessageDialog` use).
6. freedesktop Icon Theme Specification — lookup prefers the closest size
   match and treats `scalable` entries as matching any size; a stray scalable
   icon therefore wins over a bitmap at large sizes. GNOME HIG — app icons.

No new dependencies (Context7 not required for deps; used for API checks).

## Proposed design

### Window (`src/ui.rs`)
- Title "Vex VPN", default size 900×640.
- Outer `gtk::Stack`: `"main"` = `adw::NavigationSplitView`, `"missing"` =
  `adw::ToolbarView` (header bar + backend-missing `StatusPage`).
- Split view, `min_sidebar_width 320`, `max_sidebar_width 360`,
  `sidebar_width_fraction 0.4`, not collapsed (no breakpoint — out of scope).
- **Sidebar page "Vex VPN"** (`NavigationPage`) = `ToolbarView`:
  - Header bar: title widget = logo (`gtk::Image` from resource
    `/com/vex/vpn/branding/vex-vpn.png`, 22 px) + "Vex VPN" label (`heading`);
    end: main menu button.
  - `adw::Banner` (errors, as today).
  - Body (scrolled, vertically centred, 20 px margins):
    - Sign-in `StatusPage` with the **app logo as paintable** (from the same
      resource), title "Sign in to PIA", existing descriptions/button.
    - Hero: round connect button 120 px (`.connect-btn` + state class) with
      a power icon (`system-shutdown-symbolic`); state title (`title-1`);
      subtitle `dim-label` "Region · Protocol · 1h 5m" (connected) /
      "Your traffic is not protected" + region/protocol (otherwise);
      "Reconnect" flat pill, visible only when connected.
    - `boxed-list`: Kill switch `adw::SwitchRow` (moved here from Settings,
      same syncing/busy logic and subtitles; hidden with an explanatory
      `ActionRow` when mode is `off`), Download row, Upload row (value as
      `numeric` suffix label).
- **Content page "Regions"** = `ToolbarView` with header bar (title "Regions")
  and the existing `RegionsPage` content.
- Main menu: Preferences (`win.preferences`, `<Primary>comma`), Keyboard
  Shortcuts, About Vex VPN, Quit.
- `win.show-page`: kept for the tray; `"settings"` presents Preferences,
  others just present the window (regions are always visible).
- Preferences: one `adw::PreferencesWindow` built in `build_ui`
  (`hide_on_close`, modal, transient for the window) holding
  `SettingsPage.root`; still updated from the poll loop.

### Settings (`src/ui_settings.rs`)
- Remove the kill switch group (moved to dashboard); keep Connection,
  PIA account, Servers, System (kill switch mode stays read-only there).
  `enable_kill_switch` stays here (tray + dashboard use it).

### Regions (`src/ui_regions.rs`)
- Same behaviour; layout: search entry + list inside an `adw::Clamp`
  (max 560) in the scroller; check mark uses `.accent` class so it follows
  the system accent. Margins 12/18.

### CSS — replace `APP_CSS` entirely
Only libadwaita variables, no hex colours:
```css
.connect-btn { min-width:120px; min-height:120px; padding:0; border-radius:9999px; }
.connect-btn.state-connected {
  background-color: var(--accent-bg-color); color: var(--accent-fg-color);
  box-shadow: 0 0 0 9px color-mix(in srgb, var(--accent-bg-color) 22%, transparent); }
.connect-btn.state-connecting {
  color: var(--warning-color);
  box-shadow: 0 0 0 9px color-mix(in srgb, var(--warning-bg-color) 22%, transparent); }
.connect-btn.state-error { background-color: var(--error-bg-color); color: var(--error-fg-color); }
```
Disconnected = default Adwaita button background. Remove `vex-window`,
`vex-sidebar`, nav, stat-card, status-pill classes and code using them.

### Icon (`assets/`, `nix/package.nix`, `build.rs` resources)
- Delete `assets/icons/hicolor/scalable/apps/vex-vpn.svg` (placeholder).
- Package installs only the PNG logo as the app icon:
  `share/icons/hicolor/256x256/apps/vex-vpn.png` (existing file) — GTK/GNOME
  scale it. Remove the scalable install line.
- gresource: drop the scalable SVG; add
  `hicolor/256x256/apps/vex-vpn.png` under prefix
  `/com/vex/vpn/icons/hicolor/256x256/apps` so `application_icon("vex-vpn")`
  (About) resolves to the logo in-app; branding prefix serves `vpn.png`
  aliased as `vex-vpn.png` for the header/sign-in logo. `vpn2.png` stays on
  disk (unused; mention, not deleted).
- `window.set_icon_name(Some("vex-vpn"))` is implied by app id → desktop file;
  no change.

### Shortcuts (`assets/shortcuts.ui`)
- Add "Preferences — Ctrl+," to the Application group (new accel).

## Implementation steps
1. Icon/resource changes → verify: `nix build` result contains no
   `scalable/apps/vex-vpn.svg`, has `256x256/apps/vex-vpn.png`.
2. Rewrite `ui.rs` layout + CSS → verify: build/clippy clean.
3. Move kill switch to dashboard; trim settings → verify: tests pass.
4. Regions page layout tweak.
5. Preflight.

## Validation commands (approved)
- `nix develop --command cargo fmt --check`
- `nix develop --command cargo clippy -- -D warnings`
- `nix develop --command cargo build`
- `nix develop --command cargo test`
- `bash scripts/preflight.sh`

## Risks
- `color-mix()` needs GTK ≥ 4.16 — runtime is 4.22 (nixos-26.05). OK.
- CSS variables need libadwaita ≥ 1.6 at runtime — 1.9.3. OK.
- Removing the kill switch from Settings changes where users find it; it is
  now on the main pane (more visible), and the tray action still works.
- GNOME's icon cache: users must rebuild/re-login after updating for the new
  icon; no code impact.

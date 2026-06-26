# Spec: GUI Fixes — Dashboard Navigation & Branding

## Current State Analysis

- `build_sidebar()` in `src/ui.rs` creates a `dash_btn` (Dashboard nav button) but does **not** return it. Only `history_btn` and `profiles_btn` are returned and wired up. The dashboard button has no click handler and cannot navigate the user back to the root page.
- The sidebar logo area uses `gtk4::Image::from_icon_name("network-vpn-symbolic")` — no custom branding.
- The `.desktop` entry in `flake.nix` uses `Icon=network-vpn` (a generic system icon), not the project's custom PNG.
- `assets/vpn.png` and `assets/vpn2.png` exist but are not yet referenced anywhere.

## Problem Definition

1. **Dashboard button broken**: clicking Dashboard from Profiles or History pages does nothing.
2. **No branding**: the sidebar shows a generic system icon; the user provided `vpn2.png` for in-app branding.
3. **Desktop icon**: the `.desktop` entry should use `vpn.png` (the logo the user placed in assets).

## Proposed Solution

### Fix 1 — Dashboard Button Navigation

Modify `build_sidebar()` to also return `dash_btn`. In `build_ui()`, clone `dashboard_page` into the click closure and call `nav_view.pop_to_page(&dashboard_page)`. Manage active CSS class across all three nav buttons so the highlighted button always reflects the current page.

### Fix 2 — vpn2.png In-App Branding

- Move `assets/vpn2.png` → `assets/icons/vpn2.png` (inside the gresource compilation root).
- Add it to `assets/icons/icons.gresource.xml` under prefix `/com/vex/vpn/branding`.
- In `build_sidebar()`, replace `gtk4::Image::from_icon_name("network-vpn-symbolic")` with `gtk4::Image::from_resource("/com/vex/vpn/branding/vpn2.png")` and set a fixed pixel size.

### Fix 3 — .desktop Icon (vpn.png)

- Move `assets/vpn.png` → `assets/icons/hicolor/256x256/apps/vex-vpn.png`.
- Update `flake.nix` install phase to copy it into `$out/share/icons/hicolor/256x256/apps/vex-vpn.png`.
- Change the desktop entry `Icon=network-vpn` → `Icon=vex-vpn` so both the PNG (256x256) and existing SVG (scalable) are found under the same icon name.

## Implementation Steps

1. Copy `assets/vpn2.png` → `assets/icons/vpn2.png`
2. Copy `assets/vpn.png` → `assets/icons/hicolor/256x256/apps/vex-vpn.png`
3. Edit `assets/icons/icons.gresource.xml` — add branding gresource block with `vpn2.png`
4. Edit `src/ui.rs`:
   a. `build_sidebar()`: change return type to include `dash_btn`; update active state on each click
   b. `build_ui()`: destructure `dash_btn` from sidebar; connect it to `nav_view.pop_to_page(&dashboard_page)`; update active CSS on profiles/history clicks
   c. Replace sidebar logo `Image::from_icon_name` with `Image::from_resource`
5. Edit `flake.nix`: install `vex-vpn.png`; change `Icon=network-vpn` → `Icon=vex-vpn`

## Dependencies

No new external dependencies. All changes use existing GTK4/gresource infrastructure already in the project.

## Risks & Mitigations

- `from_resource()` will silently produce a broken image if the resource path is wrong. Mitigation: match path exactly to the gresource prefix + filename.
- `pop_to_page()` is a no-op if the page is already visible; that is safe behavior.
- Installing PNG at 256x256 when actual size differs is harmless for desktop display but non-ideal. Mitigation: use standard 256x256 slot as a best-effort; scalable SVG remains primary.

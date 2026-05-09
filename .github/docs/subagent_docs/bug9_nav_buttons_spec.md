# Bug 9 — Spec: Hide/Remove Unreachable Sidebar Nav Buttons

**File:** `src/ui.rs`  
**Severity:** Low  
**Spec Author:** Research Subagent  
**Date:** 2026-05-09  

---

## 1. Current State Analysis

### 1.1 Nav button creation (exact code location)

The three sidebar navigation buttons are created in `build_sidebar()` at
approximately line 231 of `src/ui.rs`, via a single `for` loop over a
constant array:

```rust
// Nav items: (icon-name, label, active)
let nav_items = [
    ("go-home-symbolic",         "Dashboard", true),
    ("network-server-symbolic",  "Servers",   false),
    ("preferences-system-symbolic", "Settings", false),
];

for (icon, label, active) in &nav_items {
    let btn = gtk4::Button::new();
    btn.add_css_class("nav-btn");
    if *active {
        btn.add_css_class("active");
    }
    btn.set_margin_start(8);
    btn.set_margin_end(8);
    btn.set_margin_bottom(2);

    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    row.set_margin_start(8);
    let img = gtk4::Image::from_icon_name(icon);
    img.set_pixel_size(16);
    let lbl = gtk4::Label::new(Some(label));
    lbl.set_halign(gtk4::Align::Start);
    lbl.set_hexpand(true);
    row.append(&img);
    row.append(&lbl);
    btn.set_child(Some(&row));
    sidebar.append(&btn);
}
```

**Key observations:**

- The loop variable `btn` is a new binding each iteration; no reference is
  retained after `sidebar.append(&btn)`.
- There are **no individual named variables** for the Servers or Settings
  buttons — they cannot be reached after the loop without iterating the
  sidebar's widget children (fragile, not idiomatic GTK).
- **No `connect_clicked` handler** is attached to any of the three buttons.
  Clicking Servers or Settings produces no action whatsoever.

### 1.2 Page-switching mechanism

There is **no `gtk4::Stack`, `adw::ViewStack`, or any other page-switching
construct**. `build_ui()` calls `build_sidebar()` for the left rail and
`build_main_page()` for the right content area; the result is two fixed
`gtk4::Box` widgets laid out horizontally inside a root `gtk4::Box`. There
is no mechanism to swap the content area based on nav selection.

### 1.3 Visibility of the offending buttons

Both the "Servers" and "Settings" buttons are unconditionally appended to
the sidebar and are visible to the user at all times. The `active` flag
only controls the `active` CSS class (green highlight); it does not affect
`visible`.

### 1.4 CSS applied to nav buttons

All three buttons share the `.nav-btn` CSS class:

```css
.nav-btn {
    border-radius: 8px;
    min-height: 42px;
    color: rgba(255,255,255,.4);
    font-size: 13px;
}
.nav-btn:hover { background: rgba(255,255,255,.05); color: white; }
.nav-btn.active { background: rgba(0,195,137,.08); color: #00c389; }
```

The Dashboard button additionally has the `active` class applied at
construction time (`if *active { btn.add_css_class("active"); }`).

---

## 2. Problem Definition

- Servers and Settings nav buttons appear in the sidebar, suggesting
  navigable pages exist behind them.
- Clicking either button does nothing — no handler, no page transition,
  no feedback.
- The README lists Settings (DNS provider, interface name, max latency)
  as a feature; `Config` has the backing fields (`dns_provider`,
  `interface`, `max_latency_ms`) but no Settings UI was implemented.
- This creates a confusing UX: buttons that look interactive but are
  completely inert.

---

## 3. Config Fields (for future reference)

`src/config.rs` — `Config` struct:

| Field            | Type   | Default   | Settings relevance        |
|------------------|--------|-----------|---------------------------|
| `auto_connect`   | bool   | false     | (already in Features panel)|
| `interface`      | String | "wg0"     | Settings: interface name  |
| `max_latency_ms` | u32    | 100       | Settings: max latency     |
| `dns_provider`   | String | "pia"     | Settings: DNS provider    |

These fields are fully serialised/deserialised to
`~/.config/vex-vpn/config.toml` but have no corresponding UI controls.

---

## 4. Proposed Solution

### 4.1 Decision: remove entries, not `set_visible(false)`

Two approaches exist:

| Approach | Mechanism | Pros | Cons |
|---|---|---|---|
| **A — Remove array entries** | Delete the Servers/Settings tuples from `nav_items` | Fewest lines changed; no dead widgets in tree; zero risk | Slightly larger diff if entries are reintroduced later |
| **B — `set_visible(false)`** | After the loop, iterate sidebar children or refactor to named bindings | Idiomatic "hide until ready" | Requires breaking the loop or touching more code; widgets still exist in memory |

**Recommendation: Approach A (remove entries).**

Because the `build_sidebar()` loop discards `btn` after each iteration,
calling `set_visible(false)` would require either:
- Refactoring the loop into three separate named bindings (larger change), or
- Iterating `sidebar`'s children by index (fragile, non-idiomatic).

Removing the two entries from the `nav_items` array is a **one-tuple-
deletion change** that is minimal, safe, and semantically correct — the
features do not exist yet, so neither should the buttons.

### 4.2 Exact change

**File:** `src/ui.rs`  
**Function:** `build_sidebar()`  

**Before:**
```rust
let nav_items = [
    ("go-home-symbolic",            "Dashboard", true),
    ("network-server-symbolic",     "Servers",   false),
    ("preferences-system-symbolic", "Settings",  false),
];
```

**After:**
```rust
let nav_items = [
    ("go-home-symbolic", "Dashboard", true),
];
```

No other changes are required. The loop, button construction code, CSS,
and all other structures remain untouched.

### 4.3 Layout impact assessment

- The sidebar is a `gtk4::Box` with `vexpand` on a spacer at the bottom.
  Removing two buttons simply shrinks the nav rail height by ~88 px
  (2 × `min-height: 42px` + margins). The spacer absorbs the difference.
- No panics can result: no code holds a reference to these buttons after
  construction.
- No tests reference the Servers or Settings nav buttons.

---

## 5. Risks and Mitigations

| Risk | Likelihood | Mitigation |
|---|---|---|
| Future implementer expects buttons to already exist | Low | Git history preserves the original entries; re-adding is trivial |
| Layout breaks when only one nav item is present | Very low | Spacer widget ensures sidebar fills height regardless of nav item count |
| Clippy/lint warnings | None | Pure array-literal reduction; no new imports, no dead code |
| Test breakage | None | No tests assert on sidebar nav button count or visibility |

---

## 6. Implementation Steps

1. Open `src/ui.rs`.
2. Locate `build_sidebar()` (approx. line 220–280).
3. Find the `nav_items` array literal.
4. Delete the two non-Dashboard tuples, leaving only:
   ```rust
   let nav_items = [
       ("go-home-symbolic", "Dashboard", true),
   ];
   ```
5. Run `nix develop --command cargo clippy -- -D warnings` — expect 0 warnings.
6. Run `nix develop --command cargo build` — expect successful compilation.
7. Run `nix develop --command cargo test` — all tests pass.
8. Run `nix develop --command cargo build --release` — successful.
9. Run `nix build` — Crane-based build completes.

---

## 7. Out of Scope

- Implementing a real Servers page (server list, region picker).
- Implementing a real Settings page (DNS, interface, max latency fields).
- Adding a `gtk4::Stack` or `adw::ViewStack` for multi-page navigation.

These remain future work; this fix is purely cosmetic/UX correctness.

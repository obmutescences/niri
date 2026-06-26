# Niri Local Feature Integration Notes

This document records the local changes on top of upstream `niri`, why they were implemented the way they were, and what an AI-assisted merge with future upstream `main` must preserve.

It is written for a future "take upstream main, then re-integrate local features" workflow.

## Scope

This repo currently carries three local feature areas that matter during future merges:

1. Configurable blur and focus animation settings.
2. The follow-up fix that forces xray when visible background effects are rendered during focus animations.
3. The later `liquid_glass` integration ported from `/home/zerone/Downloads/Niri-glass`, but adapted to the current local codebase instead of copied blindly.

## Local Change Timeline

### Commit `8aea469d`

Message:

`feat: add configurable blur and focus animation settings`

Main idea:

- Add user-configurable focus animation behavior.
- Add user-configurable blur-related settings and thread them through layout/rendering.
- Update default config and config parsing so these settings are first-class.

Key files touched by that work:

- `niri-config/src/appearance.rs`
- `niri-config/src/layout.rs`
- `niri-config/src/lib.rs`
- `resources/default-config.kdl`
- `src/layout/tile.rs`
- `src/layout/floating.rs`
- `src/layout/scrolling.rs`
- `src/layout/mod.rs`
- `src/window/mapped.rs`
- `src/backend/tty.rs`
- `src/handlers/layer_shell.rs`
- `src/layout/tests.rs`

Important invariants from that change:

- Focus animation is not a one-off shader hack; it is a config-backed behavior.
- The config defaults in code and `resources/default-config.kdl` must stay aligned.
- `src/layout/tile.rs` is one of the primary merge hotspots for this feature.

### Commit `9c29883c`

Message:

`fix: force xray for visible background effects in focus animations`

Main idea:

- When a tile is rendered into an offscreen buffer for focus-scale animation, non-xray background effects can sample transparent framebuffer contents and look wrong.
- If a background effect is visibly active, and the render path is in the focus-animation offscreen case, force xray so the effect samples the correct backdrop.

Key files touched by that work:

- `src/render_helpers/background_effect.rs`
- `src/layout/tile.rs`
- `src/layout/mod.rs`
- `src/window/mapped.rs`
- `src/layer/mapped.rs`

Important invariants from that change:

- The `force_xray` path in `src/render_helpers/background_effect.rs` must survive future merges.
- Any future effect that makes `BackgroundEffect` visibly active must be included in the "is visible" logic, otherwise the force-xray protection becomes incomplete.

## `Niri-glass` Port: What Was Actually Integrated

The external directory `/home/zerone/Downloads/Niri-glass` contains:

- `src/render_helpers/liquid_glass.rs`
- `src/render_helpers/background_effect.rs`
- `src/render_helpers/framebuffer_effect.rs`
- `src/render_helpers/xray.rs`
- `src/render_helpers/mod.rs`
- `src/render_helpers/shaders/clipped_surface.frag`
- `src/render_helpers/shaders/mod.rs`
- `niri-config/src/appearance.rs`
- `install.sh`

Its `install.sh` simply copies whole files over an older `niri` tree. That approach was intentionally **not** used here.

Why not:

- Its `background_effect.rs` would have removed the local `force_xray` fix logic.
- Its `appearance.rs` is based on an older config baseline and would have regressed local focus animation / blur config work.
- Whole-file replacement is too brittle when local feature branches have already diverged.

### Actual integration strategy used here

Only the `liquid_glass` capability was ported, and it was wired into the current local branch:

- Add config parsing for `background-effect { liquid-glass { ... } }`.
- Add a small render-side options struct for liquid glass parameters.
- Thread those parameters through existing background effect rendering.
- Extend the existing clip/postprocess shader pipeline with liquid glass uniforms and shader logic.
- Preserve all local focus animation and force-xray behavior.

## Files Changed For The `liquid_glass` Integration

These are the files modified in the current worktree for the feature integration:

- `niri-config/src/appearance.rs`
- `niri-config/src/lib.rs`
- `niri-visual-tests/src/test_window.rs`
- `src/render_helpers/background_effect.rs`
- `src/render_helpers/clipped_surface.rs`
- `src/render_helpers/framebuffer_effect.rs`
- `src/render_helpers/mod.rs`
- `src/render_helpers/shaders/clipped_surface.frag`
- `src/render_helpers/shaders/mod.rs`
- `src/render_helpers/xray.rs`

One new file was added:

- `src/render_helpers/liquid_glass.rs`

### What each file now means

#### `niri-config/src/appearance.rs`

Added:

- `LiquidGlass`
- `LiquidGlassPart`
- `BackgroundEffectRule.liquid_glass`
- `BackgroundEffect.liquid_glass`
- merge logic from parsed rule to resolved effect
- `parse_liquid_glass` test

Important merge rule:

- Do not drop the `liquid_glass` field from either `BackgroundEffectRule` or `BackgroundEffect`.
- If upstream changes how background effects are represented, this field must be re-threaded into the new structure.

#### `src/render_helpers/liquid_glass.rs`

Added:

- `LiquidGlassOptions`
- conversion from `niri_config::LiquidGlass`

Important merge rule:

- Keep this as the narrow bridge between config types and render types.
- Do not duplicate config structs directly into render code.

#### `src/render_helpers/background_effect.rs`

Added:

- `Options.liquid_glass`
- liquid-glass-aware visibility logic
- conversion from config effect to render options
- uniform threading to both xray and non-xray render paths

Most important invariant:

- `effect_options_are_visible()` must include `effect.liquid_glass.is_some()`.
- `Options::is_visible()` must include `self.liquid_glass.is_some()`.

Reason:

- If liquid glass is not treated as "visible background effect", the local `force_xray` protection added in `9c29883c` becomes incomplete and focus animation rendering can break again.

#### `src/render_helpers/framebuffer_effect.rs`

Added:

- `liquid_glass` field on `FramebufferEffectElement`
- extra uniforms for the non-xray path

Important merge rule:

- If upstream refactors framebuffer postprocess uniforms, liquid glass uniforms must be carried along with the same draw path.

#### `src/render_helpers/xray.rs`

Added:

- `liquid_glass` field on `XrayElement`
- extra uniforms for the xray path

Important merge rule:

- Liquid glass must work in both xray and non-xray modes.
- If a future merge only preserves one path, behavior will silently differ depending on effect selection and rendering situation.

#### `src/render_helpers/clipped_surface.rs`

Added:

- explicit zero/default values for all liquid glass uniforms on normal clipped surfaces

This is easy to miss, but important.

Reason:

- These surfaces are not using liquid glass.
- If uniforms are added to the compiled shader program but not explicitly initialized on unrelated draw calls, some drivers or render paths can retain previous values and produce incorrect visuals.

#### `src/render_helpers/shaders/mod.rs`

Added:

- liquid glass uniform registration for both:
  - `clipped_surface`
  - `postprocess_and_clip`

Important merge rule:

- Shader source changes alone are not enough.
- Every new uniform must also be registered in shader compilation metadata.

#### `src/render_helpers/shaders/clipped_surface.frag`

Added:

- liquid glass shader logic
- extra uniforms
- the glass effect path layered into the existing clipped/postprocessed texture flow

Important merge rule:

- This file is a high-risk merge hotspot.
- Future upstream shader changes should be merged manually, not by naive replacement.

## Additional Validation-Only Fixes Done During This Integration

These two changes are not part of the glass feature itself, but were required so the repo validates cleanly against the current local code:

### `niri-config/src/lib.rs`

Updated:

- inline snapshot in `diff_empty_to_default`

Reason:

- The current local defaults had already diverged from the old inline expected snapshot.
- Without this update, `cargo test -p niri-config` failed even though the feature integration was correct.

### `niri-visual-tests/src/test_window.rs`

Added:

- `LayoutElement::is_floating()` implementation

Reason:

- The current trait surface requires it.
- Without it, `cargo clippy --workspace --all-targets` and `cargo test -p niri-visual-tests` failed.

## Behavior That Must Be Preserved In Future Merges

When merging future upstream `main`, AI must preserve all of the following:

1. Focus animation remains configurable from config, not hardcoded.
2. Blur settings remain configurable from config.
3. The focus-animation offscreen render path can force xray for visible background effects.
4. `liquid_glass` is considered a visible background effect.
5. Liquid glass parameters travel through both:
   - xray rendering
   - framebuffer/non-xray rendering
6. Shader uniform registration and shader source stay in sync.
7. Non-liquid-glass clipped surfaces explicitly pass zero/default liquid glass uniforms.

## Recommended Future AI Merge Strategy

If later you want to merge official upstream `main` into this branch, the safest AI workflow is:

1. Merge or rebase upstream `main` first.
2. Identify upstream changes touching these hotspots:
   - `niri-config/src/appearance.rs`
   - `niri-config/src/layout.rs`
   - `niri-config/src/lib.rs`
   - `resources/default-config.kdl`
   - `src/layout/tile.rs`
   - `src/layout/mod.rs`
   - `src/window/mapped.rs`
   - `src/layer/mapped.rs`
   - `src/render_helpers/background_effect.rs`
   - `src/render_helpers/framebuffer_effect.rs`
   - `src/render_helpers/xray.rs`
   - `src/render_helpers/clipped_surface.rs`
   - `src/render_helpers/shaders/mod.rs`
   - `src/render_helpers/shaders/clipped_surface.frag`
3. Re-apply local semantics, not raw file copies.
4. Re-run validation in `nix develop`.

### Things AI must NOT do

Do not do any of the following:

- Do not copy files from `Niri-glass/install.sh` over the current tree verbatim.
- Do not replace current `niri-config/src/appearance.rs` with the external one.
- Do not remove the `force_xray` logic while resolving conflicts in `background_effect.rs`.
- Do not port only the shader and forget the Rust-side uniform registration.
- Do not port only the xray path or only the non-xray path.
- Do not ignore `clipped_surface.rs` default uniform initialization.

### Conflict resolution priority

When upstream and local code conflict, use this priority order:

1. Preserve upstream API shape if it is required for compilation.
2. Re-insert local behavior into the new API shape.
3. Verify that the seven behavior invariants above still hold.

This is especially important for:

- `src/layout/tile.rs`
- `src/render_helpers/background_effect.rs`
- shader compilation setup

## Suggested AI Prompt For Future Merge Work

If you later ask AI to merge upstream `main`, give it this document and also tell it:

> Merge upstream main into this local niri branch. Preserve the local configurable focus animation and blur feature, preserve the force-xray fix for visible background effects during focus animation offscreen rendering, and preserve the liquid_glass integration. Do not copy Niri-glass files verbatim. Reconcile behavior semantically against the current upstream API. Re-run validation in nix develop with cargo fmt, cargo check, cargo clippy --workspace --all-targets, cargo test -p niri-config, and cargo test -p niri-visual-tests.

## Validation Commands

The current integration was validated with:

```bash
rtk nix develop -c cargo fmt --all
rtk nix develop -c cargo check
rtk nix develop -c cargo clippy --workspace --all-targets
rtk nix develop -c cargo test -p niri-config
rtk nix develop -c cargo test -p niri-visual-tests
```

All of the above passed after integration.

## Example Config For Liquid Glass

```kdl
window-rule {
    background-effect {
        blur true
        xray true
        liquid-glass {
            refraction-strength 3.0
            power-factor 3.0
            refraction-power 1.0
        }
    }
}
```

## Notes About Generated Files

During testing, snapshot tooling may generate temporary files such as:

- `niri-config/src/.lib.rs.pending-snap`

That file is not part of the feature design. Treat it as test-generated state unless you intentionally want to review/update snapshots.

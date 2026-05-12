# Vim-like keybindings — implementation plan

> **For Claude:** use `/planning:execute` to implement this plan task-by-task with fresh subagents.

**Goal:** Add a vim-like binding set to broot's Command mode (rename, delete, sort, navigation, panel toggles, clipboard ops) plus three new internals and a SortOverlay, without changing default behavior for non-modal users.

**Architecture:** Almost entirely additive. New `.with_key(...)` lines in `add_builtin_verbs` for ~20 bindings. Three new internals declared in `internals!` and handled in `panel_state.rs` / app-level intercept. One new overlay (`SortOverlay`) mirroring the existing `AddOverlay` and `GotoOverlay` wiring.

**Tech Stack:** Rust, broot's existing `crokey` key macros, `terminal_clipboard` crate (already a dep, feature-gated).

## Overview

broot has a `modal: true` config flag that flips a panel between `Mode::Input` (default — letters filter the tree) and `Mode::Command` (letters trigger verbs). Today only `j`, `k`, `h`, `L` are bound in Command mode. This plan extends the set with vim-conventional single-char bindings (`r`, `d`, `D`, `y`, `Y`, `x`, `o`, `b`, `c`, `C`, `g`, `G`, `q`, `R`, `n`, `N`, `=`) and adds six alt-modifier bindings (`alt-.`, `alt-g`, `alt-i`, `alt-s`, `alt-p`, `alt-t`) that work in both modes. A new SortOverlay (single-key-pick menu) is invoked by `o`. Two new clipboard internals (`copy_name`, `copy_file_content`) round out the set.

Source of truth for design decisions: `docs/plans/2026-05-12-vim-keybindings-design.md`.

## Context (from discovery)

- **Verb registration**: `src/verb/verb_store.rs::add_builtin_verbs` (`:64`), each internal added via `.add_internal(name)` then `.with_key(key!(...))`. Examples at `:99-104` (j/k bindings), `:289-290` (alt-b, alt-n).
- **Key resolution**: `src/command/panel_input.rs::find_key_verb` (`:340-379`), first-match-wins; user verbs registered before builtins so user overrides win.
- **Mode gating**: `src/command/panel_input.rs::is_key_allowed_for_verb` (`:414-427`). In `Mode::Input`, bare letters fall through to filter input; in `Mode::Command`, every key reaches `find_key_verb`. Alt-modifier keys bypass this gate regardless of mode.
- **Overlay pattern**: `Overlay` enum at `src/app/overlay/mod.rs:127-137` (current variants `Confirm`, `Goto`, `Add`, `Stub`). Three dispatch shims in `src/app/app.rs` (render, key, mouse). `AddOverlay` is the closest reference for SortOverlay because it's the most recently added.
- **Clipboard backend**: `terminal_clipboard::set_string` gated on the `clipboard` feature. Existing handler at `src/app/panel_state.rs:138-155` for `copy_path`.
- **Destructive-confirm intercept**: `src/app/app.rs:185-205` handles `:rm` (via `requires_confirm`) and `:trash` (via name-match). Binding `d`/`D` to these verbs picks up the confirm automatically.

## Development Approach

- **Testing approach**: Regular (code + tests in same task)
- complete each task fully before moving to the next
- make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
- **CRITICAL: all tests must pass before starting next task**
- **CRITICAL: update this plan file when scope changes during implementation**
- run `cargo test` after each task
- maintain backward compatibility — default (non-modal) behavior for bare letters must not change

## Testing Strategy

- **unit tests**: required for every task
  - Verb registration / key resolution tests in `src/verb/verb_store.rs` (the file has existing key-resolution tests to extend)
  - Overlay handle_key tests inside `src/app/overlay/sort.rs`
  - Handler tests for new internals (added to existing `panel_state` tests where they exist; otherwise inline)
- **e2e tests**: broot does not ship an automated UI/e2e suite. Manual smoke testing covered in Post-Completion.

## Progress Tracking
- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope

## Solution Overview

Mostly additive, no architectural change. The plan is sequenced so dependencies flow forward:

1. Add the three new internals (their handlers depend on nothing).
2. Add the SortOverlay (depends on `open_sort_overlay` internal from step 1 to actually open).
3. Add the key bindings (depend on internals from steps 1 and 2 existing).
4. Replace alt-h with alt-.
5. Update docs.

## Technical Details

- **New internals**: `copy_name`, `copy_file_content`, `open_sort_overlay`. Declared in `src/verb/internal.rs::internals!` macro. The first two are clipboard ops handled in `panel_state.rs`; the third is an App-level intercept that returns `CmdResult::OpenOverlay(Overlay::Sort(SortOverlay::new()))`.
- **`copy_file_content` policy**: refuse non-regular files; refuse files > 10 MB (const, not config-tunable in v1); refuse non-UTF-8 bytes. All failures return `CmdResult::error` with a human message.
- **SortOverlay**: bordered box, title "Sort by", 7 lines like `[s] size`. Single-key dispatch returns `CloseAndRun(":sort_by_*")` or `CloseAndRun(":no_sort")`. `esc` / `q` close.
- **Bindings table**: see design doc for the full table. Bare-letter bindings fire only in Command mode (via the existing `is_key_allowed_for_verb` gate); alt-* bindings work in both modes.
- **alt-h → alt-. swap**: edit the existing `with_key(key!(alt-h))` line on `toggle_hidden` in `add_builtin_verbs`, change to `key!(alt-.)`.
- **Conflict policy**: no new code. `find_key_verb` is already first-match-wins; user verbs register before builtins.

## What Goes Where

- **Implementation Steps**: code changes, tests, docs.
- **Post-Completion**: manual smoke-test scenarios in a real terminal, release-note line about alt-h → alt-..

## Implementation Steps

### Task 1: Add `copy_name` internal

**Files:**
- Modify: `src/verb/internal.rs`
- Modify: `src/verb/verb_store.rs`
- Modify: `src/app/panel_state.rs`

- [ ] in `src/verb/internal.rs` add `copy_name: "copy file name to system clipboard" true,` to the `internals!` macro (alphabetical placement — between `copy_line` and `copy_path`)
- [ ] in `src/verb/verb_store.rs::add_builtin_verbs` add `self.add_internal(copy_name);` near the existing `copy_path` registration (around `:159`)
- [ ] in `src/app/panel_state.rs::on_internal_generic` extend the `Internal::copy_line | Internal::copy_path` arm to also match `Internal::copy_name`; when the internal is `copy_name`, take `selection.path.file_name().map(|s| s.to_string_lossy().to_string())` and feed that to `terminal_clipboard::set_string`; on `None` return `CmdResult::error("Selection has no file name")`. Keep `#[cfg(feature = "clipboard")]` gating identical to the existing arm.
- [ ] write unit test in `src/verb/verb_store.rs` (mod tests at the bottom) asserting `:copy_name` registers and resolves by name (no key check yet — key binding comes in Task 5)
- [ ] run `cargo test --features clipboard` and `cargo test --no-default-features` (or whatever the existing feature-matrix smoke test invocation is) — must pass before Task 2

### Task 2: Add `copy_file_content` internal

**Files:**
- Modify: `src/verb/internal.rs`
- Modify: `src/verb/verb_store.rs`
- Modify: `src/app/panel_state.rs`

- [ ] in `src/verb/internal.rs` add `copy_file_content: "copy file content to system clipboard" true,` to the `internals!` macro (after `copy_name`)
- [ ] in `src/verb/verb_store.rs::add_builtin_verbs` add `self.add_internal(copy_file_content);` next to the other copy internals
- [ ] in `src/app/panel_state.rs::on_internal_generic` add a new arm for `Internal::copy_file_content`:
  - require `selection.path` is a regular file (`metadata()?.is_file()`); else `CmdResult::error("Selection is not a regular file")`
  - require `metadata.len() <= 10 * 1024 * 1024`; define `MAX_COPY_FILE_CONTENT_BYTES: u64 = 10 * 1024 * 1024;` as a `const` at module scope or near the handler; else `CmdResult::error("File too large to copy as text")`
  - read with `fs::read(path)`; on IO error return `CmdResult::error` with the formatted error
  - validate UTF-8 with `String::from_utf8`; on failure return `CmdResult::error("Binary content, cannot copy as text")`
  - call `terminal_clipboard::set_string(content)`; on error return `CmdResult::error("Clipboard error while copying content")`
  - keep `#[cfg(feature = "clipboard")]` gating
- [ ] write unit tests for the handler:
  - directory selection returns the "not a regular file" error
  - oversized file returns the size error (use a tempfile padded over the const)
  - non-UTF-8 file returns the binary error (write 0xFF bytes to tempfile)
  - happy path with a small UTF-8 file succeeds (use a mock clipboard if available, else assert no error)
- [ ] run `cargo test --features clipboard` — must pass before Task 3

### Task 3: Add `open_sort_overlay` internal

**Files:**
- Modify: `src/verb/internal.rs`
- Modify: `src/verb/verb_store.rs`
- Modify: `src/app/app.rs`

- [ ] in `src/verb/internal.rs` add `open_sort_overlay: "open the sort overlay" false,` to the `internals!` macro (alphabetical — near the existing `bookmarks` / `add` entries)
- [ ] in `src/verb/verb_store.rs::add_builtin_verbs` add `self.add_internal(open_sort_overlay);` (no key yet — added in Task 5)
- [ ] in `src/app/app.rs::apply_command` add an App-level intercept arm for `Internal::open_sort_overlay` (placed alongside the `add` / `bookmarks` overlay-opening arms; see the bulk-rename intercept at `:185-205` for the canonical pattern). For now, return a placeholder error like `CmdResult::error(":open_sort_overlay overlay not yet built")` — Task 4 will swap it for the real overlay open.
- [ ] write unit test in `src/verb/verb_store.rs` asserting `:open_sort_overlay` registers
- [ ] run `cargo test` — must pass before Task 4

### Task 4: Build SortOverlay

**Files:**
- Create: `src/app/overlay/sort.rs`
- Modify: `src/app/overlay/mod.rs`
- Modify: `src/app/app.rs`

- [ ] create `src/app/overlay/sort.rs` with a `SortOverlay` struct (zero-sized or small — no per-instance state needed since picks are stateless). Implement `new()`, `render(area, screen, skin)`, `handle_key(key) -> OverlayOutcome`, `handle_mouse(...) -> OverlayOutcome`. Follow `AddOverlay` (`src/app/overlay/add.rs`) and `GotoOverlay` (`src/app/overlay/goto.rs`) as references.
  - `handle_key` dispatch table: `n` → `CloseAndRun(":no_sort")`, `s` → `CloseAndRun(":sort_by_size")`, `d` → `CloseAndRun(":sort_by_date")`, `c` → `CloseAndRun(":sort_by_count")`, `t` → `CloseAndRun(":sort_by_type")`, `f` → `CloseAndRun(":sort_by_type_dirs_first")`, `l` → `CloseAndRun(":sort_by_type_dirs_last")`, `esc` / `q` → `Close`, anything else → `Stay`
  - `render`: bordered box, title "Sort by", 7 body rows of the form `[<letter>] <label>`. Use `draw_frame` and `draw_frame_title` from `src/display/frame.rs`. Sizing identical to `ConfirmOverlay::render` short-body branch (`src/app/overlay/confirm.rs:155-205`): floor width 40, cap at 80% of screen width, height = `lines + 5` capped at 15.
  - Bail-out: if `area.width < 8 || area.height < 5`, return without drawing (mirrors `AddOverlay::render` at `add.rs:226`).
  - `draw_frame_title` is called with `selected: false`.
  - `handle_mouse` returns `Stay` for everything (overlays don't currently route clicks to body items; consistent with `GotoOverlay`).
- [ ] in `src/app/overlay/mod.rs` add `pub mod sort;` and add `Sort(SortOverlay)` to the `Overlay` enum (`:127-137`). Update the three impl shims (`render`, `handle_key`, `handle_mouse`) to forward the new variant to `SortOverlay::*`.
- [ ] in `src/app/app.rs` replace the Task 3 placeholder error with `CmdResult::OpenOverlay(Overlay::Sort(sort::SortOverlay::new()))`.
- [ ] write unit tests in `src/app/overlay/sort.rs`:
  - each of the 7 letters returns the correct `CloseAndRun(...)` with the matching verb string
  - `esc` and `q` return `Close`
  - arbitrary keys (e.g. `z`, `1`, `enter`) return `Stay`
- [ ] run `cargo test` — must pass before Task 5

### Task 5: Add bare-letter Command-mode key bindings

**Files:**
- Modify: `src/verb/verb_store.rs`

- [ ] in `add_builtin_verbs` add `.with_key(...)` lines for each of the following on the matching internal/external verb registration. Some verbs (e.g. `bulk_rename`, `bookmarks`) already have existing `with_key` lines for other keys; **append** the new `with_key` rather than replacing:

  | Key | Target |
  |---|---|
  | `key!('r')` | `bulk_rename` internal (inherits F2 behavior) |
  | `key!('d')` | `trash` (external `:trash` verb) — locate the external-verb registration and add `.with_key(key!('d'))` |
  | `key!('D')` (i.e. `key!(shift-d)`) | `rm` (external `:rm` verb) |
  | `key!('y')` | `copy_from_staging` |
  | `key!('Y')` (shift-y) | `copy_file_content` |
  | `key!('x')` | `move_from_staging` |
  | `key!('o')` | `open_sort_overlay` |
  | `key!('b')` | `bookmarks` (append to existing alt-b) |
  | `key!('c')` | `copy_name` |
  | `key!('C')` (shift-c) | `copy_path` |
  | `key!('=')` | `toggle_stage` (append to existing `+`) |
  | `key!('g')` | `select_first` |
  | `key!('G')` (shift-g) | `select_last` |
  | `key!('q')` | `quit` (append to existing ctrl-q) |
  | `key!('R')` (shift-r) | `refresh` (append to existing F5) |
  | `key!('n')` | `next_match` (append to existing tab) |
  | `key!('N')` (shift-n) | `previous_match` (append to existing backtab) |

  Note: if a target external verb (`:rm`, `:trash`, `:rename`) doesn't currently have a `with_key` and the external builder doesn't expose `with_key` directly, follow the same pattern used elsewhere in `add_builtin_verbs` for binding keys to externals (search for existing examples — likely `with_key` is on `Verb` so should work uniformly).
- [ ] extend `verb_store.rs` unit tests (the existing mod tests at the bottom of the file) to assert each new binding resolves to the expected verb under `find_key_verb`. Use one test case per key. For the gated bare-letter keys, also assert resolution returns the verb regardless of mode (mode is enforced upstream by `is_key_allowed_for_verb`, not by `find_key_verb`, so a unit-level test on the verb store should match unconditionally — confirm this matches existing tests).
- [ ] write a regression test asserting that `key!('r')` resolves to `bulk_rename` first (because the internal is registered before the external `rename`), preserving the existing F2 priority invariant documented in CLAUDE.md.
- [ ] run `cargo test` — must pass before Task 6

### Task 6: Add Command-mode mode-gating pin test

**Files:**
- Modify: `src/command/panel_input.rs` (test mod)

- [ ] add a unit test in `panel_input.rs`'s test module (or create one if absent) asserting `is_key_allowed_for_verb(key!('r'), Mode::Input) == false` and `is_key_allowed_for_verb(key!('r'), Mode::Command) == true`. Repeat for `'d'`, `'g'`, `'q'` (representative sample of new bindings).
- [ ] add a pin test asserting alt-modifier keys are allowed in `Mode::Input` (e.g. `is_key_allowed_for_verb(key!(alt-.), Mode::Input) == true`) — this guards the alt-* bindings working without the modal flag.
- [ ] run `cargo test` — must pass before Task 7

### Task 7: Add alt-modifier bindings + alt-h → alt-. swap

**Files:**
- Modify: `src/verb/verb_store.rs`

- [ ] **replace** the existing `with_key(key!(alt-h))` on `toggle_hidden` with `with_key(key!(alt-.))`. (Do not add a second `with_key` — this is a replacement, not an addition.)
- [ ] add `.with_key(key!(alt-g))` to `toggle_git_status`
- [ ] add `.with_key(key!(alt-i))` to `toggle_ignore` (note: `toggle_ignore` and `toggle_git_ignore` are separate internals; this binds to `toggle_ignore` per design)
- [ ] add `.with_key(key!(alt-s))` to `toggle_staging_area`
- [ ] add `.with_key(key!(alt-p))` to `toggle_preview`
- [ ] add `.with_key(key!(alt-t))` to `toggle_tree`
- [ ] extend `verb_store.rs` unit tests to assert each alt-* binding resolves to the right internal
- [ ] add a pin test asserting `key!(alt-h)` does NOT resolve to `toggle_hidden` (catches accidental re-adds)
- [ ] run `cargo test` — must pass before Task 8

### Task 8: Verify acceptance criteria

- [ ] re-read `docs/plans/2026-05-12-vim-keybindings-design.md` and confirm every binding in the two tables is implemented
- [ ] confirm three new internals (`copy_name`, `copy_file_content`, `open_sort_overlay`) are declared, registered, and handled
- [ ] confirm SortOverlay is a fourth production variant in the `Overlay` enum and all three dispatch shims handle it (per the CLAUDE.md "Overlay routing" invariant)
- [ ] confirm `alt-h` no longer resolves to `toggle_hidden` (replaced by `alt-.`)
- [ ] run full test suite: `cargo test`
- [ ] run with both feature flags: `cargo test --features clipboard` and `cargo test --no-default-features`
- [ ] manual smoke test in a real terminal with `modal: true` in conf.hjson — see Post-Completion for scenarios

### Task 9: Update documentation

**Files:**
- Modify: `README.md`
- Modify: `CLAUDE.md`
- Move: `docs/plans/2026-05-12-vim-keybindings.md` → `docs/plans/completed/`
- Move: `docs/plans/2026-05-12-vim-keybindings-design.md` → `docs/plans/completed/`

- [ ] update `README.md` with a short "Command mode bindings" section listing the new bare-letter and alt-modifier bindings, near the existing keybinding docs
- [ ] update `CLAUDE.md` "Overlay routing" section to add `Sort` to the list of `Overlay` variants (currently lists `Confirm`, `Goto`, `Add`, `Stub`)
- [ ] update `CLAUDE.md` if a new architectural pattern emerged (e.g. the `copy_file_content` size cap if it becomes load-bearing; otherwise skip)
- [ ] `mkdir -p docs/plans/completed` if not present
- [ ] move both this plan and the design doc to `docs/plans/completed/`

## Post-Completion

*Items requiring manual intervention or external systems — no checkboxes, informational only*

**Manual verification scenarios** (run in a real terminal with `modal: true` set):

- Press space to enter Input mode, type `r` — should append `r` to the filter (no rename). Press esc to return to Command mode, press `r` — should open the rename prompt.
- Press `d` on a regular file — should open the trash confirm overlay; press `D` — should open the rm confirm overlay.
- Press `o` — SortOverlay opens. Press `s` — sort by size and overlay closes. Open again with `o`, press esc — overlay closes without action.
- Press `gg`'s replacement `g` — selection jumps to top of tree. Press `G` — selection jumps to bottom.
- Press `c` — clipboard contains the file's name. Press `C` — clipboard contains the full path. Press `Y` on a small UTF-8 file — clipboard contains the file content. Press `Y` on a directory — error in status row. Press `Y` on a 100 MB file — error in status row.
- Press `alt-.` — hidden files toggle. Press `alt-h` — should now be unbound (no action).
- Press `alt-s`, `alt-p`, `alt-t` — stage panel, preview panel, tree level toggle respectively.
- With `modal: false` (default) in a fresh conf: type `r` — should filter, not rename. Press `alt-.` — should still toggle hidden (alt-modifiers work in both modes).

**Release notes** (when shipping):

- One-line breaking-change note: `alt-h` no longer toggles hidden files; use `alt-.` instead. Users with `alt-h` muscle memory can re-bind in `conf.hjson`.
- Highlight the new vim-like Command mode bindings as the headline feature.

**External system updates**: none.

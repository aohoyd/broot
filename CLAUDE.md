# CLAUDE.md

Project-specific notes for future agents working on broot. Each section
records a non-obvious invariant that production code already depends on;
reading the code alone will not surface most of these. If a change here
starts to read like a feature changelog, delete it.

## Frame inset and area math

Every `Areas` carries two rectangles. `Areas::state_outer` is the full
panel rect including the 1-cell frame edge on every side;
`Areas::state` is the interior rect, inset by 1 cell on each side
(`src/display/areas.rs:14-22`). The frame drawer paints into
`state_outer`. All panel content (tree, preview, stage, input row,
status row) paints into `state`. Mixing them either draws on top of the
frame or leaves a 1-cell gap.

`BrowserState::page_height` returns `screen.height - 4`
(`src/browser/browser_state.rs:142-148`): minus 1 for the input row,
1 for the status row, and 1 for each of the top/bottom frame edges.
Any new `PanelState` impl that does its own page arithmetic must
mirror this — see `move_selection` / `try_select_next_filtered` for the
call sites that drive scroll math.

Click hit-testing uses `state_outer`
(`src/app/app_panels.rs:420`). A click on the frame border still
selects the panel underneath; do not switch this to `state` or the
1-cell border becomes a dead zone.

Frame helpers live in `src/display/frame.rs`. `draw_frame` and
`draw_frame_title` both take `&StyleMap` — broot has no `Palette` type
(elio does, but we never imported it). The `frame_title` style key was
added to `StyleMap` at `src/skin/style_map.rs:230`. Re-using `selected_line`
or another nearby key for the title will silently swap styles and the
test in `frame.rs` will keep passing because it only checks glyph bytes.

## Overlay routing — single field, single render hook, single key hook

`App::overlay: Option<Overlay>` (`src/app/app.rs:81`) is the only
floating-modal state. Do not introduce a parallel flag or a stack —
the variants of `Overlay` (`src/app/overlay/mod.rs:127-137`,
currently `Confirm`, `Goto`, `Add`, plus a test-only `Stub`) cover
every floating-modal need we have, and the rest of the routing
assumes "at most one".

Render hook: `display_panels` post-passes the overlay after every panel
has been drawn (`src/app/app.rs:983` and `:993` pass
`self.overlay.as_ref()` down). Key/mouse hook: when `overlay.is_some()`,
the event loop dispatches to `overlay.handle_key` /
`overlay.handle_mouse` before `Panel::apply_command` ever sees the
event (`src/app/app.rs:1028-1049`).

The four `OverlayOutcome` variants decide what happens after the
handler runs (`src/app/app.rs:867-898`):

- `Stay` — event consumed, overlay remains.
- `Close` — overlay dropped. The Close arm also clears
  `App::pending_bulk_rename` so a cancelled bulk-rename confirm doesn't
  leave a stale plan that a later direct `:bulk_rename_apply` could
  pick up.
- `CloseAndRun(cmd)` — overlay dropped, `App::skip_confirm = true`,
  then `cmd` re-enters `apply_command`. The `skip_confirm` flag
  (`src/app/app.rs:87`, cleared at `:205`) is the loop-avoidance
  signal — without it the destructive verb would re-open the same
  overlay. Cleared unconditionally on every dispatch.
  `App::pending_bulk_rename: Option<RenameRun>` is the sibling
  payload field for bulk rename — the rename plan rides through the
  same `CloseAndRun(":bulk_rename_apply")` re-dispatch but lives on
  `App` rather than inside the command (see the "Bulk rename"
  sub-section below for the full pattern).
- `CloseAndFocus(path)` — overlay dropped, a synthetic `:focus <path>`
  `VerbInvocation` is dispatched.

Adding a new overlay variant means editing three places only: the
`Overlay` enum, and each of the three dispatch shims (`render`,
`handle_key`, `handle_mouse`). There is no extra plumbing — no event
filter, no panel callback, no `CmdResult` variant beyond the existing
`OpenOverlay` (`src/app/app.rs:515`).

### Add modal — `Internal::add` / `AddOverlay`

`Internal::add` is the create-file-or-directory entry point. It is
**browser-only** by design: the handler lives in
`BrowserState::on_internal` (`src/browser/browser_state.rs:803`); for
every other panel type the wildcard arm in `on_internal_generic`
returns `CmdResult::Keep` so the keypress is silently consumed.

The target directory for the modal is chosen by
`resolve_add_target_dir` (`src/browser/browser_state.rs:59`): if the
current selection is a directory, create inside it; otherwise create
alongside the selection (i.e. its parent). The bound key is `alt-n`
(`src/verb/verb_store.rs:292`).

`AddOverlay::try_commit` returns `OverlayOutcome::CloseAndFocus(full)`
on success — the App synthesizes a `:focus <full>` invocation so the
just-created entry becomes selected and the tree scrolls to it. The
overlay also bails out as a no-op render when
`area.width < 8 || area.height < 5` (`src/app/overlay/add.rs:226`) so
pathologically small terminals don't draw a half-frame.

Overwrite policy: `try_commit` refuses to clobber an existing entry.
After validation passes, the helper probes `full.exists()` and, if
true, sets `self.error = "file or directory already exists"` and
returns `Stay` — the user picks a different name. This matches the
codebase's "destructive operations require a confirm overlay" policy:
silently truncating an existing file (which `fs::File::create` would
do) would bypass the confirm intercept entirely, and a `:add` modal
that surprises a user by wiping their work is a footgun. There is no
"force overwrite" path — destructive replacement is the job of
`:rm` + `:add`, both of which already prompt.

`maybe_bulk_stage_confirm` skips `Internal::add` (alongside the
bulk-rename internals) so pressing `alt-n` while the stage panel is
active with 2+ staged paths opens the Add modal directly rather than
a spurious "Run :add on N files?" prompt.

### Bulk rename — App-level intercept and `pending_bulk_rename` payload

The `Internal::bulk_rename` and `Internal::bulk_rename_apply` arms do
**not** live in a `PanelState::on_internal` — they fire at the App
level, intercepted at the top of `App::apply_command` (after the
confirm intercept, before panel dispatch). The reason: the entry leg
reads `app_state.stage`, suspends the TUI to run `$EDITOR`, plans the
rename set, and opens a `ConfirmOverlay` — all `App`-level concerns
that don't reduce to a panel-state callback. The lookup uses the
`resolved_internal(cmd, con)` helper which recognises all three
command shapes (`Command::Internal`, `Command::VerbTrigger`,
`Command::VerbInvocate`) that can resolve to an internal.

`App::pending_bulk_rename: Option<bulk_rename::RenameRun>` is the
payload field, mirroring the `skip_confirm` single-field discipline.
The entry leg (`run_bulk_rename`) populates it then opens the confirm
overlay with `Command::from_raw(":bulk_rename_apply", true)` as the
pending command. The overlay's `CloseAndRun` re-dispatches that
command, which lands back in `apply_command`, the intercept matches
again, and the apply leg (`run_bulk_rename_apply`) `mem::take`s the
field. No `Command` enum payload changes — the run never travels
through a command.

**F2 dual-registration**: both the internal `bulk_rename` (in
`add_builtin_verbs` directly before the rename external) and the
external `rename` verb bind F2. `find_key_verb` returns the first
verb in registration order whose filters pass, so the internal wins.
The internal's stage-size branching is what surfaces the inline
single-file flow when the stage is empty or has one path: it looks
up the external `rename` verb by name (via
`find_external_rename_verb_id`) and synthesizes a
`Command::VerbTrigger` to drive it, leaving the external's existing
arg-prompt behaviour unchanged.

**Apply partial-failure semantics**: `bulk_rename::apply` is two-phase
(`src/bulk_rename/mod.rs`'s `apply` function); cycles are resolved by
renaming the source to `<name>.broot-bulk-tmp-{idx}` first, then
renaming the temp onto the final target in a second pass. On the
first `fs::rename` error the apply returns
`Err((PathBuf, io::Error))` immediately — entries before the failure
stay applied, no rollback. The status row gets the failing path and
error message, the stage is NOT cleared (so the user can re-run from
the surviving subset), and all panels refresh so the tree reflects
the partial state. Phase-1 temps that survive a phase-2 failure are
intentionally left on disk under their `.broot-bulk-tmp-{idx}` names.

The bulk-stage confirm intercept (`maybe_bulk_stage_confirm`) never
runs for `Internal::bulk_rename` / `Internal::bulk_rename_apply`
because the App-level intercept catches them first — they don't
reach the deny-list check at all.

## Verb confirmation system

`Verb::requires_confirm: bool` (`src/verb/verb.rs:81`) is the explicit
always-on destructive-verb signal, set via `Verb::with_confirm(true)`
at registration time (`src/verb/verb_store.rs:338-343` for the built-in
`:rm` external verb). Users override it per-verb through
`VerbConf::confirm: Option<bool>` — see the apply path at
`src/verb/verb_store.rs:571`.

`:trash` is **not** registered with `with_confirm`. Its confirmation
fires from a hardcoded name/internal check inside
`App::maybe_destructive_confirm` — the function recognises both
`Internal::trash` directly and a `:trash` verb whose `execution`
maps to `Internal::trash`. This is intentional: keeping the
`requires_confirm` flag off lets users opt out by re-binding the
internal in their own `conf.hjson` (because the `confirm` field only
overrides the per-verb flag, not the App-level intercept). If you ever
add another internal that should always confirm, follow the same
pattern: enumerate it explicitly in the intercept, do not pile a
`requires_confirm`-equivalent on `Internal`.

Do not heuristic-detect destructive verbs from the shell exec string
(e.g. matching `rm -rf` in the exec pattern). External verbs default to
`requires_confirm: false`; if a user writes a destructive external verb
they must opt in via `confirm: true` in their `conf.hjson`. The unit
test at `src/verb/verb_store.rs:756-781` pins this behaviour.

The intercept lives in `App::apply_command` and runs three checks in
order (`src/app/app.rs:185-205`). At most one overlay opens per
dispatch; the first match wins:

1. Bulk staging — `App::maybe_bulk_stage_confirm`
   (`src/app/app.rs:669`). Fires only when the stage panel is the
   active panel and `app_state.stage.len() >= 2`. The internal-side
   logic is a **deny-list**: confirm only when the resolved verb is
   external, or when the resolved internal is in
   `is_stage_consuming_internal` (`src/app/app.rs:1310`). Everything
   else — navigation, app-level verbs (`:quit`, `:help`, `:back`,
   `:escape`, `:refresh`), panel switching, every `:toggle_*`, every
   `:sort_by_*`, every `:input_*`, search, bookmarks, stage-management
   itself, `:focus`, the bulk-rename and add modals — bypasses
   automatically. The deny-list has nine entries:

   | Internal | Why it fans out |
   |---|---|
   | `copy_from_staging` | Copies N staged files to a destination |
   | `move_from_staging` | Moves N staged files to a destination |
   | `trash` | Sends N staged files to trash |
   | `open_stay` | Opens N files externally without leaving broot |
   | `open_leave` | Opens N files externally and exits broot |
   | `open_preview` | Opens N files in the preview pane |
   | `print_path` | Prints all staged absolute paths to stdout |
   | `print_relative_path` | Prints all staged relative paths |
   | `print_tree` | Prints the tree of staged paths |

   New internals that iterate the stage MUST be added here, or they
   will silently bypass the confirm and run without a user warning.
   The doc-comment above `is_stage_consuming_internal` repeats this
   invariant.
2. Overwrite check — resolved destination of `:cp`/`:mv` already
   `exists()`.
3. Per-verb `requires_confirm` (and the `Internal::trash` shape, which
   is recognised from the resolved `Verb`, not from any per-internal
   flag — `src/app/app.rs:604`, `:629`, `:650`).

`skip_confirm` and an already-open overlay both bypass the intercept
(`src/app/app.rs:185`). The bookkeeping is symmetric: a re-dispatched
command from `CloseAndRun` clears its bypass on the way through
(`:205`), so a verb that itself opens a new overlay (none exist today,
but the door is open) still gets the next round of checks.

## Bookmarks config plumbing

`Conf::bookmarks: Option<Vec<BookmarkConf>>` (`src/conf/conf.rs:135`).
The serde field is necessary but not sufficient — the merge line at
`src/conf/conf.rs:267` is what actually copies user values onto the
running `Conf`. Forgetting that line is the single most common way a
new config field "doesn't work" in broot; the comment at lines 262-266
records why this particular field uses plain `overwrite!` rather than
`overwrite_vec!`: a user supplying an empty list must replace the
defaults, and `overwrite_vec!` would append.

`BookmarkEntry` runtime expansion happens once, at `AppContext`
construction (`src/app/app_context.rs:252`, calling
`build_bookmarks`), not at navigation time. `~`, `${HOME}`,
`${XDG_CONFIG_HOME}`, and `trash://` (platform-resolved) are expanded
exactly once. Bookmarks that can't be resolved are dropped with a
warning at that point.

Built-in defaults live in `default_bookmarks`
(`src/app/bookmark.rs:166-176`) and fire when the `bookmarks` key is
absent from the user's `conf.hjson` (`build_bookmarks` matches on
`None`). When you edit the defaults you must also edit the
commented-out example block
in `resources/default-conf/conf.hjson:106-122` — these are the
literals users start from when they uncomment, and they must stay in
sync.

Duplicate-key policy: first definition wins, later duplicates are
dropped with `cli_log::warn!` (`src/app/bookmark.rs::materialise`,
the `out.iter().any(...)` guard near the top of the loop). The
comparison is ASCII-case-insensitive, mirroring the goto modal's
case-insensitive single-char jump dispatch — so `h` and `H` are not
both bindable. Ordering in the user list is therefore load-bearing.

## Icon defaults

`AppContext::from` defaults `icon_theme` to `"nerdfont"` when the
config field is absent (`src/app/app_context.rs:193`). The previous
default — no icons at all — is no longer reachable without an explicit
opt-out.

The literal string `"none"` (case-insensitive) is the opt-out, handled
in `icon_plugin` (`src/icon/mod.rs:16`). Returning `None` there causes
the renderer to skip the icon column entirely. Any other unknown
string also returns `None` but is treated as a misconfiguration rather
than an intentional disable; users who want no icons should write
`icon_theme: none`.

Nerd Font glyphs are PUA codepoints. `unicode-width` reports them as
1 cell, and the renderer pattern at
`src/display/displayable_tree.rs:314-316` is `icon` + space — two cells
in total. This was reduced from a 3-cell prefix in a prior pass after a
user bug report about over-wide icon columns; the column math elsewhere
in the tree assumes this 2-cell prefix. If you change the spacing,
audit the callers that compute `cw.allowed`-style budgets and the
content-extract path that subtracts a small constant from `cw.allowed`.

## Footer-zone theming

All thirteen footer-zone style keys default to background `rgb(6, 11, 20)`
(panel_alt), so the status row, input row, and flag hints share the
dark-blue chrome. The keys are: `status_normal`, `status_italic`,
`status_bold`, `status_code`, `status_ellipsis`, `status_job`,
`flag_label`, `flag_value`, `input`, `purpose_normal`,
`purpose_italic`, `purpose_bold`, `purpose_ellipsis`. See the table at
`src/skin/style_map.rs:161-241`.

Two intentional exceptions: `status_error` keeps `rgb(224, 90, 90)`
(cut_bar red) so errors visually dominate the row; `mode_command_mark`
inverts with `rgb(255, 178, 86)` (accent_warn) bg + `rgb(9, 16, 27)`
fg + Bold so the command-mode caret stays the most prominent glyph on
screen.

User `skin:` overrides still beat these defaults — the override path
through the `StyleMap!` macro (`src/skin/style_map.rs:69-105`) is
unchanged. The pin test `footer_zone_uses_panel_alt_bg` at
`src/skin/style_map.rs:298-326` prevents silent palette drift; if you
intentionally edit the defaults, update the test alongside or it will
catch you. `mode_command_mark` has its own pinned test
(`mode_command_mark_uses_accent_warn_bg` at
`src/skin/style_map.rs:371-381`) because its bg differs by design.

## Preview frame title

`PreviewState::frame_title(max_width)` (`src/preview/preview_state.rs:166-198`)
returns `"{filename}  •  {info}"` when `Preview::info_string()` returns
`Some` (`Preview::info_string` in `src/preview/preview.rs`), else just
the filename. The bullet character is U+2022 (` • `) surrounded by 2
spaces each side. Truncation policy: when the full string overflows
`max_width`, the filename is truncated from the right with `…`; the
info clause (short and informative) is never truncated. If even the
filename + bullet won't fit, the title falls back to a bare truncated
filename — no `•` appears. Empty / missing filename renders as
`"???"`. `frame_title` returns `String` per the trait at
`src/app/panel_state.rs:1154-1162`, not `Option<String>` — there is no
no-op signal; the default and the `PreviewState` override both always
return a non-empty fallback (`default_frame_title_for_type` for the
default; `"???"` for the override's missing-filename case).

The body row that used to hold filename + count is gone — content
paints from `state_area.top` directly (was `state_area.top + 1`), and
`self.preview_area = state_area.clone()` at
`src/preview/preview_state.rs:350` (no longer insets the area). The
per-variant `display_info` painters (in `src/preview/dir_view.rs`,
`src/syntactic/text_view.rs`, `src/hex/hex_view.rs`,
`src/image/image_view.rs`, `src/tty/tty_view.rs`) and the
`Preview::display_info` dispatcher were deleted with the body-row
removal; their text replacement is `info_string`, consumed by
`frame_title`.

## Tree root row + aux status

`tree.lines[0]` still exists in memory as the root, but is **never
painted** by `DisplayableTree::write_on`. The body loop iterates
`tree.lines[1..]` starting at `state_area.top` (not
`state_area.top + 1`); see `src/display/displayable_tree.rs` where
`line_index = (y as usize) + 1 + tree.scroll`. The scrollbar offset
starts at `area.top` and spans `area.height`, with total content rows
`tree.lines.len() - 1`. The scrollbar paint loop writes to absolute
row `y + self.area.top` (not the loop counter `y`) and compares
against `compute_scrollbar`'s absolute `sctop`/`scbottom` — get this
wrong and the thumb paints over the frame's top corner.

Scroll math is interlocked with the +1 offset:
- `Tree::try_scroll` uses `lines.len() - 1` as the scrollable length
  and `lines.len() - 1 - page_height` as the max scroll.
- `Tree::make_selection_visible` mirrors the same: "everything fits"
  is `page_height >= lines.len() - 1`, and the clamp-to-bottom branch
  is `scroll = lines.len() - 1 - page_height`.

Click mapping: `Tree::try_select_y` maps a body y to
`lines[y + 1 + scroll]`. Out-of-bounds returns `false` and leaves
selection alone. `BrowserState::on_click` first subtracts the cached
`body_top` (set during `display`) before calling `try_select_y` —
clicks arrive in absolute terminal coords, the body row math is body-
relative. `BrowserState::on_double_click` reconstructs the
`line_index` from `y` the same way and compares to `tree.selection`,
not to `y` (the post-shift selection is `line_index`, not `y`).

`BrowserState::page_height` is unchanged at `screen.height - 4` — the
freed root-row cell becomes one extra visible entry, the geometry
math doesn't move. `Internal::back`, `Internal::focus`, and
`BrowserState::open_selection_stay_in_broot` all key off
`tree.selection == 0`, which is still valid because the data model
didn't move; only rendering shifted.

The three pieces of aux info that used to decorate the root row (git
status summary, total size when `show_sizes`, mount-space bar when
`show_root_fs`) now live at the right end of the wide status row.
The data flows through `PanelState::status_aux` (trait default
`None` at `src/app/panel_state.rs:1173`; only `BrowserState` overrides
at `src/browser/browser_state.rs:973`). The carrier type
`StatusAux` lives at `src/display/status_aux.rs`. Paint geometry lives
in `src/display/status_line.rs:37-101`: aux is suppressed when
`status.error == true` (errors win the row), when the row is too
narrow (`aux_w + 4 > total_after_leading`), or when the aux is empty.
Active-panel gating happens upstream — `WIDE_STATUS = true` means only
the active panel calls `write_status`, so aux follows automatically
without per-panel checks.

The dead `write_root_line` painter was deleted entirely during the
migration (along with the orphan `GitStatusDisplay`, `ComputationResult`,
`UnicodeWidthChar`, `UnicodeWidthStr` imports in
`displayable_tree.rs`). Restoring root-row rendering would mean
rebuilding that painter and gating the body loop on a new toggle —
not a trivial revert.

## Frame title as selectable root

The frame title is the visual carrier for the (still-hidden) tree
root row's selection state. `PanelState::title_selected()` is the
trait hook (default `false` at `src/app/panel_state.rs:1190`); only
`BrowserState` overrides it, returning `displayed_tree().selection == 0`
(`src/browser/browser_state.rs:991`). Future tree-like panels can opt in
by overriding — every other panel type (Preview, Stage, Trash, Help,
Fs) keeps the default and its title never highlights.

Inactive panels still paint their title with the selection bg when
`title_selected()` returns true — this mirrors the body's
`selected_line` paint at `src/display/displayable_tree.rs:495`, which
gates on `in_app` but not on `active`. So an inactive browser panel
whose `selection == 0` shows a subtly-highlighted title using
`unfocused.styles.selected_line.bg`, the same way its body row would
have. Don't gate the title on `active` without also gating the body's
selection paint — they're a pair.

`draw_frame_title` (`src/display/frame.rs:137`) takes a
`selected: bool`. When true, it clones the `frame_title` compound
style and overlays `selected_line.get_bg()` onto it — fg/attrs of
`frame_title` are preserved. If a custom skin leaves `selected_line`
without a bg, the overlay is a no-op and the title falls back to the
unstyled `frame_title` look (no panic, no visible bg). No new style
key was added; the selected variant is a pure composition of two
existing keys.

`BrowserState::on_click` routes through `try_select_title_row(y)`
before `body_relative_y(y)`. The helper sets `selection = 0` when
`body_top > 0 && y == body_top - 1`. When it returns `true`,
`on_click` returns `CmdResult::Keep` immediately and skips the
body-relative `try_select_y` path — that short-circuit is what makes a
title-row click semantically distinct from a body row's coordinate
math. `on_double_click` follows the same pattern but additionally
dispatches to `open_selection_stay_in_broot` after the title-row
intercept, which means a double-click on the title navigates up
(open_selection treats `selection == 0` as "go to parent"). The
`body_top > 0` guard is load-bearing — on degenerate tiny terminals
the frame collapses and `body_top == 0`, where `body_top - 1` would
underflow `u16`. The narrow-panel case (`3 <= outer.width < 6`, where
the top frame edge is drawn but no title glyph is painted) is
deliberately not width-gated: a click on that edge still selects the
root, matching the "top edge is the root's seat" intent. Those panels
are pathological at that width anyway.

Four production call sites pass the `selected` arg: the panel render
in `src/app/app_panels.rs:712` forwards `panel.state().title_selected()`;
the three overlay renderers (`src/app/overlay/goto.rs:195`,
`src/app/overlay/confirm.rs:185`, `src/app/overlay/add.rs:244`) always
pass `false` — overlays have no selectable root.

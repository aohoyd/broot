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
(`src/browser/browser_state.rs:112-117`): minus 1 for the input row,
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

`App::overlay: Option<Overlay>` (`src/app/app.rs:77`) is the only
floating-modal state. Do not introduce a parallel flag or a stack —
the variants of `Overlay` (`src/app/overlay/mod.rs:82-90`,
currently `Confirm`, `Goto`, plus a test-only `Stub`) cover every
floating-modal need we have, and the rest of the routing assumes
"at most one".

Render hook: `display_panels` post-passes the overlay after every panel
has been drawn (`src/app/app.rs:784` and `:794` pass
`self.overlay.as_ref()` down). Key/mouse hook: when `overlay.is_some()`,
the event loop dispatches to `overlay.handle_key` /
`overlay.handle_mouse` before `Panel::apply_command` ever sees the
event (`src/app/app.rs:825-842`).

The four `OverlayOutcome` variants decide what happens after the
handler runs (`src/app/app.rs:672-704`):

- `Stay` — event consumed, overlay remains.
- `Close` — overlay dropped.
- `CloseAndRun(cmd)` — overlay dropped, `App::skip_confirm = true`,
  then `cmd` re-enters `apply_command`. The `skip_confirm` flag
  (`src/app/app.rs:83`, cleared at `:191`) is the loop-avoidance
  signal — without it the destructive verb would re-open the same
  overlay. Cleared unconditionally on every dispatch.
- `CloseAndFocus(path)` — overlay dropped, a synthetic `:focus <path>`
  `VerbInvocation` is dispatched.

Adding a new overlay variant means editing three places only: the
`Overlay` enum, and each of the three dispatch shims (`render`,
`handle_key`, `handle_mouse`). There is no extra plumbing — no event
filter, no panel callback, no `CmdResult` variant beyond the existing
`OpenOverlay` (`src/app/app.rs:467`).

## Verb confirmation system

`Verb::requires_confirm: bool` (`src/verb/verb.rs:81`) is the explicit
always-on destructive-verb signal, set via `Verb::with_confirm(true)`
at registration time (`src/verb/verb_store.rs:326-334` for the built-in
`:rm` external verb). Users override it per-verb through
`VerbConf::confirm: Option<bool>` — see the apply path at
`src/verb/verb_store.rs:562`.

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
order (`src/app/app.rs:157-190`). At most one overlay opens per
dispatch; the first match wins:

1. Bulk staging — `App::maybe_bulk_stage_confirm`
   (`src/app/app.rs:621`). Fires only when the stage panel is the
   active panel and `app_state.stage.len() >= 2`. Skips stage-management
   internals (`is_stage_management_internal`,
   `src/app/app.rs:1083-1097`) — those are `Internal::stage`,
   `unstage`, `toggle_stage`, `clear_stage`, `stage_all_directories`,
   `stage_all_files`, and `*_staging_area` — because they operate
   on the stage itself, not on its contents.
2. Overwrite check — resolved destination of `:cp`/`:mv` already
   `exists()`.
3. Per-verb `requires_confirm` (and the `Internal::trash` shape, which
   is recognised from the resolved `Verb`, not from any per-internal
   flag — `src/app/app.rs:556`, `:581`, `:602`).

`skip_confirm` and an already-open overlay both bypass the intercept
(`src/app/app.rs:171`). The bookkeeping is symmetric: a re-dispatched
command from `CloseAndRun` clears its bypass on the way through
(`:191`), so a verb that itself opens a new overlay (none exist today,
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
dropped with `cli_log::warn!` (`src/app/bookmark.rs:163-168`).
Ordering in the user list is therefore load-bearing.

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
1 cell, but the renderer pattern at
`src/display/displayable_tree.rs:320-323` is `icon` + space + space —
three cells in total. This is intentional: the glyphs visually need
extra breathing room in most terminals, and the column math elsewhere
in the tree depends on this fixed 3-cell prefix. Do not "fix" the
width counting to 2 cells.

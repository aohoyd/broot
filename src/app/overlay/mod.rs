//! Floating overlay layer.
//!
//! The overlay sits on top of the regular panel rendering and, when
//! present, captures input. Confirmation modals ([`ConfirmOverlay`]) and
//! the Goto/Bookmarks modal (Task 12) are implemented as overlay
//! variants.
//!
//! This module defines the contract — the [`OverlayState`] trait, the
//! [`Overlay`] enum that the [`App`](crate::app::App) holds at most one
//! of, and the [`OverlayOutcome`] returned by event handlers — plus a
//! `#[cfg(test)]` stub used by the routing tests.

mod add;
mod confirm;
mod goto;

pub use add::AddOverlay;
pub use confirm::ConfirmOverlay;
pub use goto::GotoOverlay;
// `ConfirmFocus` is an internal detail of the confirm overlay (the
// tests inside `confirm.rs` reference it directly via `super::`); it
// deliberately has no module-level re-export here to keep the public
// surface tight.

use {
    crate::{
        command::Command,
        skin::StyleMap,
    },
    crokey::{
        KeyCombination,
        crossterm::event::MouseEvent,
    },
    std::{
        cell::Cell,
        io::{
            self,
            Write,
        },
        path::PathBuf,
    },
    termimad::Area,
};

// =============================================================================
// shared helpers (used by overlay variants)
// =============================================================================
//
// `io_err` and `truncate_to_width` used to live here as duplicates of
// the same helpers in `crate::display::frame`. They are now re-exported
// from there to keep one definition of each. Variants import them
// through the same `super::{io_err, truncate_to_width}` path they used
// before, so the call sites are unchanged.

pub(crate) use crate::display::frame::{
    io_err,
    truncate_to_width,
};

/// Extension trait for `Cell<Option<T>>` that lets a `&Cell` hand back a
/// clone of its value while leaving the original in place. Used by the
/// overlay variants to read their cached hit-rectangles without taking
/// `&mut self`.
pub(crate) trait CellGetCloned<T: Clone> {
    fn get_cloned(&self) -> Option<T>;
}

impl<T: Clone> CellGetCloned<T> for Cell<Option<T>> {
    fn get_cloned(&self) -> Option<T> {
        // Cell::take + replace — preserves the value while letting us
        // hand a clone to the caller.
        let v = self.take();
        let cloned = v.clone();
        self.set(v);
        cloned
    }
}

/// Behaviour every overlay variant implements.
///
/// `render` is called by `display_panels` after all panels have been
/// drawn so the overlay paints on top. `handle_key` and `handle_mouse`
/// are called by the App-level event dispatcher *before* the event is
/// forwarded to a panel — when an overlay is active, it has exclusive
/// input.
pub trait OverlayState {
    /// Paint the overlay onto `w`. `screen` is the full terminal area;
    /// the overlay is responsible for computing its own sub-rectangle
    /// (typically via [`crate::display::frame::centered_rect`]).
    ///
    /// Generic over any `Write` implementation rather than the
    /// crate-wide `W` alias so unit tests can pass a `BufWriter<Sink>`
    /// (avoids spurious stderr noise during `cargo test`). At runtime
    /// `app_panels` still calls this with the production `W`.
    fn render<Wr: Write>(
        &self,
        w: &mut Wr,
        screen: Area,
        palette: &StyleMap,
    ) -> io::Result<()>;

    /// Handle a key combination. Return value indicates whether the
    /// overlay should stay, close, close + run a command, or close +
    /// focus a path.
    fn handle_key(
        &mut self,
        key: KeyCombination,
    ) -> OverlayOutcome;

    /// Handle a mouse event. Same outcome semantics as `handle_key`.
    fn handle_mouse(
        &mut self,
        ev: MouseEvent,
    ) -> OverlayOutcome;
}

/// The single overlay (if any) the `App` is currently displaying.
///
/// - `Confirm(ConfirmOverlay)` — yes/no destructive-action prompt
///   (Task 7); wired into rm/trash, mv/cp overwrite and bulk staging
///   by later tasks.
/// - `Goto(GotoOverlay)` — bookmark jump menu (Task 12).
/// - `Add(AddOverlay)` — create file or directory modal.
///
/// The `Stub` variant is test-only and exists so the dispatch shims
/// can be exercised before the real variants land.
pub enum Overlay {
    /// Yes/no confirmation prompt for destructive actions.
    Confirm(ConfirmOverlay),
    /// Bookmark / goto modal.
    Goto(GotoOverlay),
    /// Create file or directory modal.
    Add(AddOverlay),
    /// Test-only variant for routing tests.
    #[cfg(test)]
    Stub(StubOverlay),
}

impl Overlay {
    /// Dispatch `render` to the active variant. Generic over any
    /// `Write`; at runtime `app_panels` calls this with the production
    /// `W` (`BufWriter<Stderr>`).
    pub fn render<Wr: Write>(
        &self,
        w: &mut Wr,
        screen: Area,
        palette: &StyleMap,
    ) -> io::Result<()> {
        match self {
            Overlay::Confirm(o) => o.render(w, screen, palette),
            Overlay::Goto(o) => o.render(w, screen, palette),
            Overlay::Add(o) => o.render(w, screen, palette),
            #[cfg(test)]
            Overlay::Stub(s) => s.render(w, screen, palette),
        }
    }

    /// Dispatch `handle_key` to the active variant.
    pub fn handle_key(
        &mut self,
        key: KeyCombination,
    ) -> OverlayOutcome {
        match self {
            Overlay::Confirm(o) => o.handle_key(key),
            Overlay::Goto(o) => o.handle_key(key),
            Overlay::Add(o) => o.handle_key(key),
            #[cfg(test)]
            Overlay::Stub(s) => s.handle_key(key),
        }
    }

    /// Dispatch `handle_mouse` to the active variant.
    pub fn handle_mouse(
        &mut self,
        ev: MouseEvent,
    ) -> OverlayOutcome {
        match self {
            Overlay::Confirm(o) => o.handle_mouse(ev),
            Overlay::Goto(o) => o.handle_mouse(ev),
            Overlay::Add(o) => o.handle_mouse(ev),
            #[cfg(test)]
            Overlay::Stub(s) => s.handle_mouse(ev),
        }
    }
}

/// Result of a key/mouse event handled by an overlay.
#[derive(Debug)]
pub enum OverlayOutcome {
    /// Overlay stays open; event was consumed.
    Stay,
    /// Close the overlay; no further action.
    Close,
    /// Close the overlay then run `Command` through the normal
    /// `apply_command` machinery (used by `ConfirmOverlay`).
    CloseAndRun(Command),
    /// Close the overlay then navigate to `PathBuf` (used by
    /// `GotoOverlay`). The App synthesizes a `:focus <path>` command.
    CloseAndFocus(PathBuf),
}

// =============================================================================
// Test-only stub
// =============================================================================

#[cfg(test)]
pub struct StubOverlay {
    pub last_key: Option<KeyCombination>,
    pub last_mouse: Option<MouseEvent>,
    pub render_count: std::cell::Cell<u32>,
    pub outcome: OverlayOutcome,
}

#[cfg(test)]
impl StubOverlay {
    pub fn with_outcome(outcome: OverlayOutcome) -> Self {
        Self {
            last_key: None,
            last_mouse: None,
            render_count: std::cell::Cell::new(0),
            outcome,
        }
    }
}

#[cfg(test)]
impl OverlayState for StubOverlay {
    fn render<Wr: Write>(
        &self,
        _w: &mut Wr,
        _screen: Area,
        _palette: &StyleMap,
    ) -> io::Result<()> {
        self.render_count.set(self.render_count.get() + 1);
        Ok(())
    }

    fn handle_key(
        &mut self,
        key: KeyCombination,
    ) -> OverlayOutcome {
        self.last_key = Some(key);
        clone_outcome(&self.outcome)
    }

    fn handle_mouse(
        &mut self,
        ev: MouseEvent,
    ) -> OverlayOutcome {
        self.last_mouse = Some(ev);
        clone_outcome(&self.outcome)
    }
}

#[cfg(test)]
fn clone_outcome(o: &OverlayOutcome) -> OverlayOutcome {
    match o {
        OverlayOutcome::Stay => OverlayOutcome::Stay,
        OverlayOutcome::Close => OverlayOutcome::Close,
        OverlayOutcome::CloseAndRun(cmd) => OverlayOutcome::CloseAndRun(cmd.clone()),
        OverlayOutcome::CloseAndFocus(p) => OverlayOutcome::CloseAndFocus(p.clone()),
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use {
        super::*,
        crokey::crossterm::event::{
            KeyCode,
            KeyEvent,
            KeyModifiers,
            MouseButton,
            MouseEventKind,
        },
    };

    fn key_a() -> KeyCombination {
        KeyCombination::from(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE))
    }

    fn mouse_click() -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn stub_returns_stay() {
        let mut s = StubOverlay::with_outcome(OverlayOutcome::Stay);
        let out = s.handle_key(key_a());
        assert!(matches!(out, OverlayOutcome::Stay));
        assert_eq!(s.last_key, Some(key_a()));
    }

    #[test]
    fn stub_returns_close() {
        let mut s = StubOverlay::with_outcome(OverlayOutcome::Close);
        let out = s.handle_key(key_a());
        assert!(matches!(out, OverlayOutcome::Close));
    }

    #[test]
    fn stub_returns_close_and_run() {
        let cmd = Command::from_raw(":help".to_string(), true);
        let mut s = StubOverlay::with_outcome(OverlayOutcome::CloseAndRun(cmd));
        let out = s.handle_key(key_a());
        assert!(matches!(out, OverlayOutcome::CloseAndRun(_)));
    }

    #[test]
    fn stub_returns_close_and_focus() {
        let path = PathBuf::from("/tmp/x");
        let mut s = StubOverlay::with_outcome(OverlayOutcome::CloseAndFocus(path.clone()));
        let out = s.handle_mouse(mouse_click());
        match out {
            OverlayOutcome::CloseAndFocus(p) => assert_eq!(p, path),
            _ => panic!("expected CloseAndFocus"),
        }
        assert_eq!(s.last_mouse.map(|m| (m.column, m.row)), Some((5, 5)));
    }

    #[test]
    fn stub_render_increments_counter() {
        let palette = StyleMap::no_term();
        let s = StubOverlay::with_outcome(OverlayOutcome::Stay);
        let mut buf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let area = Area::new(0, 0, 80, 24);
        s.render(&mut buf, area, &palette).unwrap();
        assert_eq!(s.render_count.get(), 1);
    }

    #[test]
    fn overlay_dispatches_handle_key_to_stub() {
        let mut overlay = Overlay::Stub(StubOverlay::with_outcome(OverlayOutcome::Close));
        let out = overlay.handle_key(key_a());
        assert!(matches!(out, OverlayOutcome::Close));
    }

    #[test]
    fn overlay_dispatches_handle_mouse_to_stub() {
        let mut overlay = Overlay::Stub(StubOverlay::with_outcome(OverlayOutcome::Stay));
        let out = overlay.handle_mouse(mouse_click());
        assert!(matches!(out, OverlayOutcome::Stay));
    }

    /// Routing simulation: model the App's overlay-event dispatch as a
    /// pure function so it can be unit-tested without spinning up the
    /// whole event loop. Mirrors the logic added in `app.rs`.
    fn route_key(
        overlay: &mut Option<Overlay>,
        key: KeyCombination,
    ) -> RoutingDecision {
        let Some(ov) = overlay.as_mut() else {
            return RoutingDecision::PanelHandlesIt;
        };
        match ov.handle_key(key) {
            OverlayOutcome::Stay => RoutingDecision::Consumed,
            OverlayOutcome::Close => {
                *overlay = None;
                RoutingDecision::Consumed
            }
            OverlayOutcome::CloseAndRun(cmd) => {
                *overlay = None;
                RoutingDecision::RunCommand(cmd)
            }
            OverlayOutcome::CloseAndFocus(p) => {
                *overlay = None;
                RoutingDecision::FocusPath(p)
            }
        }
    }

    #[derive(Debug)]
    enum RoutingDecision {
        PanelHandlesIt,
        Consumed,
        RunCommand(Command),
        FocusPath(PathBuf),
    }

    #[test]
    fn routing_no_overlay_lets_panel_handle() {
        let mut overlay: Option<Overlay> = None;
        let decision = route_key(&mut overlay, key_a());
        assert!(matches!(decision, RoutingDecision::PanelHandlesIt));
        assert!(overlay.is_none());
    }

    #[test]
    fn routing_stay_keeps_overlay() {
        let mut overlay = Some(Overlay::Stub(StubOverlay::with_outcome(
            OverlayOutcome::Stay,
        )));
        let decision = route_key(&mut overlay, key_a());
        assert!(matches!(decision, RoutingDecision::Consumed));
        assert!(overlay.is_some());
    }

    #[test]
    fn routing_close_clears_overlay() {
        let mut overlay = Some(Overlay::Stub(StubOverlay::with_outcome(
            OverlayOutcome::Close,
        )));
        let decision = route_key(&mut overlay, key_a());
        assert!(matches!(decision, RoutingDecision::Consumed));
        assert!(overlay.is_none());
    }

    #[test]
    fn routing_close_and_run_clears_overlay_and_returns_cmd() {
        let cmd = Command::from_raw(":help".to_string(), true);
        let mut overlay = Some(Overlay::Stub(StubOverlay::with_outcome(
            OverlayOutcome::CloseAndRun(cmd),
        )));
        let decision = route_key(&mut overlay, key_a());
        assert!(matches!(decision, RoutingDecision::RunCommand(_)));
        assert!(overlay.is_none());
    }

    #[test]
    fn routing_close_and_focus_clears_overlay_and_returns_path() {
        let path = PathBuf::from("/tmp/x");
        let mut overlay = Some(Overlay::Stub(StubOverlay::with_outcome(
            OverlayOutcome::CloseAndFocus(path.clone()),
        )));
        let decision = route_key(&mut overlay, key_a());
        match decision {
            RoutingDecision::FocusPath(p) => assert_eq!(p, path),
            _ => panic!("expected FocusPath"),
        }
        assert!(overlay.is_none());
    }
}

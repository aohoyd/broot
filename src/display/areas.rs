use {
    super::*,
    crate::app::Panel,
    termimad::Area,
};

/// the areas of the various parts of a panel. It's
/// also where a state usually checks how many panels
/// there are, and their respective positions
#[derive(Debug, Clone)]
pub struct Areas {
    /// Outer rectangle of the panel state region, including the
    /// elio-style frame border. This is the rectangle that the frame
    /// drawer renders into, and that mouse-hit-testing should use
    /// (a click anywhere on the frame still selects the panel).
    pub state_outer: Area,
    /// Interior rectangle for content, inset by 1 cell on every side
    /// from `state_outer`. This is what state implementations should
    /// draw their content into. If the outer is too small to host a
    /// frame (width or height < 3), this is clamped to a 0x0 rect.
    pub state: Area,
    pub status: Area,
    pub input: Area,
    pub purpose: Option<Area>,
    pub pos_idx: usize, // from left to right
    pub nb_pos: usize,  // number of displayed panels
}

const MINIMAL_PANEL_HEIGHT: u16 = 4;
const MINIMAL_PANEL_WIDTH: u16 = 8;
const MINIMAL_SCREEN_WIDTH: u16 = 16;

enum Slot<'a> {
    Panel(usize),
    New(&'a mut Areas),
}

impl Areas {
    /// compute an area for a new panel which will be inserted
    pub fn create(
        present_panels: &mut [Panel],
        layout_instructions: &LayoutInstructions,
        mut insertion_idx: usize,
        screen: Screen,
        with_preview: bool, // slightly larger last panel
    ) -> Self {
        if insertion_idx > present_panels.len() {
            insertion_idx = present_panels.len();
        }
        let mut areas = Areas {
            state_outer: Area::uninitialized(),
            state: Area::uninitialized(),
            status: Area::uninitialized(),
            input: Area::uninitialized(),
            purpose: None,
            pos_idx: 0,
            nb_pos: 1,
        };
        let mut slots = Vec::with_capacity(present_panels.len() + 1);
        for i in 0..insertion_idx {
            slots.push(Slot::Panel(i));
        }
        slots.push(Slot::New(&mut areas));
        for i in insertion_idx..present_panels.len() {
            slots.push(Slot::Panel(i));
        }
        Self::compute_areas(
            present_panels,
            layout_instructions,
            &mut slots,
            screen,
            with_preview,
        );
        areas
    }

    pub fn resize_all(
        panels: &mut [Panel],
        layout_instructions: &LayoutInstructions,
        screen: Screen,
        with_preview: bool, // slightly larger last panel
    ) {
        let mut slots = Vec::new();
        for i in 0..panels.len() {
            slots.push(Slot::Panel(i));
        }
        Self::compute_areas(
            panels,
            layout_instructions,
            &mut slots,
            screen,
            with_preview,
        );
    }

    /// Compute the areas for all panels
    fn compute_areas(
        panels: &mut [Panel],
        layout_instructions: &LayoutInstructions,
        slots: &mut [Slot],
        screen: Screen,
        with_preview: bool, // slightly larger last panel
    ) {
        let computed = Self::compute_layout(
            slots.len(),
            layout_instructions,
            screen,
            with_preview,
        );
        for (slot_idx, new_areas) in computed.into_iter().enumerate() {
            let areas: &mut Areas = match &mut slots[slot_idx] {
                Slot::Panel(panel_idx) => &mut panels[*panel_idx].areas,
                Slot::New(areas) => areas,
            };
            *areas = new_areas;
        }
    }

    /// Pure geometry: compute one `Areas` per panel slot. Public to the
    /// crate's display module for the unit tests below; it deliberately
    /// avoids any dependency on `Panel` so it's trivially testable.
    fn compute_layout(
        nb_pos: usize,
        layout_instructions: &LayoutInstructions,
        screen: Screen,
        with_preview: bool, // slightly larger last panel
    ) -> Vec<Areas> {
        let screen_height = screen.height.max(MINIMAL_PANEL_HEIGHT);
        let screen_width = screen.width.max(MINIMAL_SCREEN_WIDTH);
        let n = nb_pos as u16;

        // compute auto/default panel widths
        let mut panel_width = if with_preview {
            3 * screen_width / (3 * n + 1)
        } else {
            screen_width / n
        };
        if panel_width < MINIMAL_PANEL_WIDTH {
            panel_width = panel_width.max(MINIMAL_PANEL_WIDTH);
        }
        let mut panel_widths = vec![panel_width; nb_pos];
        // The last panel absorbs the rounding remainder of the
        // screen-width / panel-count division. Use saturating
        // arithmetic and floor at MINIMAL_PANEL_WIDTH so a tiny screen
        // (e.g. width 16, 4 panels with the floor of 8 each) cannot
        // underflow.
        panel_widths[nb_pos - 1] = screen_width
            .saturating_sub((nb_pos as u16 - 1) * panel_width)
            .max(MINIMAL_PANEL_WIDTH);

        // adjust panel widths with layout instructions
        if nb_pos > 1 {
            for instruction in &layout_instructions.instructions {
                debug!("Applying {instruction:?}");
                debug!("panel_widths before: {panel_widths:?}");
                match *instruction {
                    LayoutInstruction::Clear => {} // not supposed to happen
                    LayoutInstruction::MoveDivider { divider, dx } => {
                        if divider + 1 >= nb_pos {
                            continue;
                        }
                        let (decr, incr, diff) = if dx < 0 {
                            (divider, divider + 1, (-dx) as u16)
                        } else {
                            (divider + 1, divider, dx as u16)
                        };
                        // saturating_sub: a width that already fell to (or
                        // below) the minimum yields 0, so we just refuse
                        // to shrink the panel further.
                        let diff = diff
                            .min(panel_widths[decr].saturating_sub(MINIMAL_PANEL_WIDTH));
                        panel_widths[decr] -= diff;
                        panel_widths[incr] += diff;
                    }
                    LayoutInstruction::SetPanelWidth { panel, width } => {
                        if panel >= nb_pos {
                            continue;
                        }
                        let width = width.max(MINIMAL_PANEL_WIDTH);
                        if width > panel_widths[panel] {
                            let mut diff = width - panel_widths[panel];
                            // as we try to increase the width of 'panel' we have to decrease the
                            // widths of the other ones
                            while diff > 0 {
                                let mut freed = 0;
                                let step = diff / (nb_pos as u16 - 1);
                                for i in 0..nb_pos {
                                    if i != panel {
                                        // saturating_sub: see the
                                        // MoveDivider branch above for the
                                        // same underflow guard.
                                        let step = step.min(
                                            panel_widths[i]
                                                .saturating_sub(MINIMAL_PANEL_WIDTH),
                                        );
                                        panel_widths[i] -= step;
                                        freed += step;
                                    }
                                }
                                if freed == 0 {
                                    break;
                                }
                                diff -= freed;
                                panel_widths[panel] += freed;
                            }
                        } else {
                            // we distribute the freed width among other panels
                            let freed = panel_widths[panel] - width;
                            panel_widths[panel] = width;
                            let denom = nb_pos as u16 - 1;
                            let step = freed / denom;
                            for i in 0..nb_pos {
                                if i != panel {
                                    panel_widths[i] += step;
                                }
                            }
                            // distribute the remainder (`freed - step*denom`) to
                            // the first non-`panel` slot. The original
                            // `freed - denom * freed` was a transcription bug
                            // (and would underflow for any non-zero freed).
                            let rem = freed - step * denom;
                            for i in 0..nb_pos {
                                if i != panel {
                                    panel_widths[i] += rem;
                                    break;
                                }
                            }
                        }
                    }
                }
                debug!("panel_widths after: {:?}", &panel_widths);
            }
        }

        // compute the areas of each slot
        let mut result: Vec<Areas> = Vec::with_capacity(nb_pos);
        let mut x = 0;
        for slot_idx in 0..nb_pos {
            let panel_width = panel_widths[slot_idx];
            let y = screen_height - 2;
            // The outer rectangle is the full panel region (status and input
            // rows live below it). The frame border sits on the edge of the
            // outer; the interior `state` is inset by 1 on every side. If the
            // outer is too small to host a frame (width or height < 3) the
            // interior is floored to 0x0 so callers don't underflow when
            // subtracting 2 from the dimensions.
            let outer = Area::new(x, 0, panel_width, y);
            let inner = if outer.width < 3 || outer.height < 3 {
                Area::new(outer.left, outer.top, 0, 0)
            } else {
                Area::new(
                    outer.left + 1,
                    outer.top + 1,
                    outer.width - 2,
                    outer.height - 2,
                )
            };
            let status = if WIDE_STATUS {
                Area::new(0, y, screen_width, 1)
            } else {
                Area::new(x, y, panel_width, 1)
            };
            let input_y = y + 1;
            let mut input = Area::new(x, input_y, panel_width, 1);
            if slot_idx == nb_pos - 1 {
                // the char at the bottom right of the terminal should not be touched
                // (it makes some terminals flicker) so the input area is one char shorter
                input.width -= 1;
            }
            let purpose = if slot_idx > 0 {
                // the purpose area is over the panel at left
                let area_width = panel_widths[slot_idx - 1] / 2;
                Some(Area::new(x - area_width, input_y, area_width, 1))
            } else {
                None
            };
            result.push(Areas {
                state_outer: outer,
                state: inner,
                status,
                input,
                purpose,
                pos_idx: slot_idx,
                nb_pos,
            });
            x += panel_width;
        }
        result
    }

    pub fn is_first(&self) -> bool {
        self.pos_idx == 0
    }
    pub fn is_last(&self) -> bool {
        self.pos_idx + 1 == self.nb_pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(width: u16, height: u16) -> Screen {
        Screen { width, height }
    }

    fn layout() -> LayoutInstructions {
        LayoutInstructions::default()
    }

    #[test]
    fn one_pane_80x24_has_outer_full_width_and_inner_inset_by_one() {
        let areas = Areas::compute_layout(1, &layout(), screen(80, 24), false);
        assert_eq!(areas.len(), 1);
        let a = &areas[0];
        // outer occupies the full panel region; status + input rows live
        // below in the bottom 2 rows of the screen (rows 22, 23).
        assert_eq!(a.state_outer.left, 0);
        assert_eq!(a.state_outer.top, 0);
        assert_eq!(a.state_outer.width, 80);
        assert_eq!(a.state_outer.height, 22);
        // inner is inset by 1 on each side: 80-2 = 78, 22-2 = 20.
        assert_eq!(a.state.left, 1);
        assert_eq!(a.state.top, 1);
        assert_eq!(a.state.width, 78);
        assert_eq!(a.state.height, 20);
        // status and input rows still anchored at the bottom.
        assert_eq!(a.status.top, 22);
        assert_eq!(a.input.top, 23);
    }

    #[test]
    fn two_pane_layout_splits_screen_and_each_pane_has_inset_inner() {
        let areas = Areas::compute_layout(2, &layout(), screen(80, 24), false);
        assert_eq!(areas.len(), 2);

        // each panel's outer width spans roughly half the screen; together
        // they cover the entire screen width.
        let total: u16 = areas.iter().map(|a| a.state_outer.width).sum();
        assert_eq!(total, 80);

        let left = &areas[0];
        let right = &areas[1];
        // left panel is anchored at column 0 and right panel begins
        // immediately after the left panel's outer rectangle.
        assert_eq!(left.state_outer.left, 0);
        assert_eq!(right.state_outer.left, left.state_outer.width);
        // each inner is the outer minus 2 columns and 2 rows.
        assert_eq!(left.state.width, left.state_outer.width - 2);
        assert_eq!(left.state.height, left.state_outer.height - 2);
        assert_eq!(right.state.width, right.state_outer.width - 2);
        assert_eq!(right.state.height, right.state_outer.height - 2);
        // inner left is shifted right by 1 from the outer left.
        assert_eq!(left.state.left, left.state_outer.left + 1);
        assert_eq!(right.state.left, right.state_outer.left + 1);
    }

    #[test]
    fn three_pane_layout_panels_are_contiguous_and_inset() {
        let areas = Areas::compute_layout(3, &layout(), screen(120, 30), false);
        assert_eq!(areas.len(), 3);

        // panels are contiguous: each next panel starts where the previous
        // panel's outer rectangle ends.
        for i in 1..areas.len() {
            let prev = &areas[i - 1];
            let cur = &areas[i];
            assert_eq!(
                cur.state_outer.left,
                prev.state_outer.left + prev.state_outer.width,
                "panel {i} should start where panel {} ends",
                i - 1,
            );
        }

        // total outer width covers the full screen.
        let total: u16 = areas.iter().map(|a| a.state_outer.width).sum();
        assert_eq!(total, 120);

        // each inner rect is inset by 1 in every direction relative to its
        // outer rect.
        for a in &areas {
            assert_eq!(a.state.left, a.state_outer.left + 1);
            assert_eq!(a.state.top, a.state_outer.top + 1);
            assert_eq!(a.state.width, a.state_outer.width - 2);
            assert_eq!(a.state.height, a.state_outer.height - 2);
        }
    }

    #[test]
    fn outer_left_matches_panel_x_and_inner_left_is_panel_x_plus_one() {
        // verify the explicit "no left-shift" property called out in the plan.
        let areas = Areas::compute_layout(2, &layout(), screen(80, 24), false);
        let left = &areas[0];
        let right = &areas[1];
        assert_eq!(left.state_outer.left, 0);
        assert_eq!(left.state.left, 1);
        let panel_x_for_right = left.state_outer.width;
        assert_eq!(right.state_outer.left, panel_x_for_right);
        assert_eq!(right.state.left, panel_x_for_right + 1);
    }

    #[test]
    fn tiny_terminal_clamps_inner_to_zero_without_overflow() {
        // With screen_height clamped up to MINIMAL_PANEL_HEIGHT (4), the
        // outer panel height is `4 - 2 = 2`, which is < 3 and must trigger
        // the floor-to-zero branch on `state` rather than underflow.
        let areas = Areas::compute_layout(1, &layout(), screen(80, 1), false);
        assert_eq!(areas.len(), 1);
        let a = &areas[0];
        assert_eq!(a.state_outer.height, 2);
        // Floor: when the outer is too small for a frame the inner is 0x0.
        assert_eq!(a.state.width, 0);
        assert_eq!(a.state.height, 0);
    }

    #[test]
    fn last_panel_width_does_not_underflow_on_tiny_screen() {
        // 4 panels on a 16-column screen: 16 / 4 = 4, floored to
        // MINIMAL_PANEL_WIDTH = 8 → panel_width = 8. The original
        // expression was `screen_width - (n-1)*panel_width`
        // = 16 - 24 = -8, which used to underflow as u16. Saturating
        // arithmetic must keep us at MINIMAL_PANEL_WIDTH instead.
        let areas = Areas::compute_layout(4, &layout(), screen(16, 24), false);
        assert_eq!(areas.len(), 4);
        for a in &areas {
            assert!(
                a.state_outer.width >= MINIMAL_PANEL_WIDTH,
                "panel width {} fell below MINIMAL_PANEL_WIDTH {}",
                a.state_outer.width,
                MINIMAL_PANEL_WIDTH,
            );
        }
    }

    #[test]
    fn floor_branch_is_consistent_for_every_pane() {
        // Sanity: across a range of pane counts and screens, the inner is
        // always either the inset rect or floored to 0x0 - never half-set.
        for nb_pos in 1..=4 {
            for screen_h in [1u16, 2, 3, 4, 8, 24] {
                let areas =
                    Areas::compute_layout(nb_pos, &layout(), screen(80, screen_h), false);
                assert_eq!(areas.len(), nb_pos);
                for a in &areas {
                    if a.state_outer.width < 3 || a.state_outer.height < 3 {
                        assert_eq!(a.state.width, 0, "nb_pos={nb_pos}, h={screen_h}");
                        assert_eq!(a.state.height, 0, "nb_pos={nb_pos}, h={screen_h}");
                    } else {
                        assert_eq!(a.state.width, a.state_outer.width - 2);
                        assert_eq!(a.state.height, a.state_outer.height - 2);
                        assert_eq!(a.state.left, a.state_outer.left + 1);
                        assert_eq!(a.state.top, a.state_outer.top + 1);
                    }
                }
            }
        }
    }
}

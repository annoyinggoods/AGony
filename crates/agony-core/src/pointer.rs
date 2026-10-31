//! The pure pointer-gesture machine (Part06): raw pointer events in,
//! gestures out. Double-click timing, wheel accumulation and hover
//! tracking are state here; the shell feeds plain events plus a
//! `now_ms` it reads from its own clock (time is a parameter, never a
//! call — clock reads stay out of this crate).
//!
//! The core knows no crossterm and no one-cell adjustment: it receives
//! already-translated cells (the shell's `cell_at` is the one place
//! that mapping lives).
//!
//! Items: `PointerEvent` — one translated input (`kind`, `button`,
//! `col`, `row`); `PointerKind` / `PointerButton` — the event's shape;
//! `Gesture` — what the machine recognized (`Click`, `DoubleClick`,
//! `Wheel`, `Hover`, `Leave`); `Gestures` — the state machine.
//! Core functions: `Gestures::on_event` — fold one event (with the
//! injected time) into at most one gesture; `Gestures::drain_wheel` —
//! the accumulated wheel rows, taken once per draw tick so a trackpad
//! flood becomes one scroll.
//! Depends on: nothing.
//! Called by: the shell's `app` (the one dispatch).
//! Calls: nothing.

/// The double-click window in milliseconds (Part06 6.2.2; a Part08
/// settings candidate). Strictly inside: at exactly 400 ms it is two
/// clicks.
pub const DOUBLE_CLICK_MS: u64 = 400;

/// Terminal convention: one wheel notch scrolls three rows (Part06
/// 6.2.3).
pub const WHEEL_ROWS_PER_NOTCH: i32 = 3;

/// Two presses within this many cells (Chebyshev) count as a pair.
const DOUBLE_CLICK_CELLS: u16 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerButton {
    Left,
    Middle,
    Right,
}

/// What happened, already translated to desktop cells. Wheel events
/// carry their signed row delta (one notch = [`WHEEL_ROWS_PER_NOTCH`],
/// positive = content moves down the view).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerKind {
    Down,
    Up,
    Drag,
    /// The pointer moved without a button (hover).
    Move,
    Wheel {
        rows: i32,
    },
    /// The pointer left the terminal (crossterm's `Gone`).
    Gone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerEvent {
    pub kind: PointerKind,
    pub button: PointerButton,
    pub col: u16,
    pub row: u16,
}

/// One recognized gesture: the only thing the shell's dispatcher sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    Click {
        button: PointerButton,
        col: u16,
        row: u16,
    },
    DoubleClick {
        button: PointerButton,
        col: u16,
        row: u16,
    },
    /// Accumulated wheel rows since the last draw tick.
    Wheel {
        rows: i32,
    },
    Hover {
        col: u16,
        row: u16,
    },
    Leave,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LastDown {
    button: PointerButton,
    col: u16,
    row: u16,
    at_ms: u64,
}

/// The gesture state: the last press (for pair detection) and the wheel
/// accumulator. All timing comes in through `now_ms`.
#[derive(Debug, Default)]
pub struct Gestures {
    last_down: Option<LastDown>,
    pending_wheel: i32,
}

impl Gestures {
    pub fn new() -> Gestures {
        Gestures::default()
    }

    /// Fold one event, at injected time `now_ms`, into at most one
    /// gesture. Wheel events only accumulate (drain them once per draw
    /// tick with [`Gestures::drain_wheel`]); `Up` and `Drag` carry no
    /// gesture (the shell's drag plumbing watches them raw).
    pub fn on_event(&mut self, event: PointerEvent, now_ms: u64) -> Option<Gesture> {
        match event.kind {
            PointerKind::Down => self.on_down(event, now_ms),
            PointerKind::Wheel { rows } => {
                self.pending_wheel = self.pending_wheel.saturating_add(rows);
                None
            }
            PointerKind::Move => Some(Gesture::Hover {
                col: event.col,
                row: event.row,
            }),
            PointerKind::Gone => Some(Gesture::Leave),
            PointerKind::Up | PointerKind::Drag => None,
        }
    }

    fn on_down(&mut self, event: PointerEvent, now_ms: u64) -> Option<Gesture> {
        let pair = self.last_down.take().is_some_and(|last| {
            last.button == event.button
                && now_ms.saturating_sub(last.at_ms) < DOUBLE_CLICK_MS
                && cells_apart(last.col, last.row, event.col, event.row) <= DOUBLE_CLICK_CELLS
        });
        if pair {
            return Some(Gesture::DoubleClick {
                button: event.button,
                col: event.col,
                row: event.row,
            });
        }
        self.last_down = Some(LastDown {
            button: event.button,
            col: event.col,
            row: event.row,
            at_ms: now_ms,
        });
        Some(Gesture::Click {
            button: event.button,
            col: event.col,
            row: event.row,
        })
    }

    /// The wheel rows accumulated since the last call (one draw tick's
    /// worth), as one gesture; None when nothing accumulated.
    pub fn drain_wheel(&mut self) -> Option<Gesture> {
        let rows = self.pending_wheel;
        self.pending_wheel = 0;
        (rows != 0).then_some(Gesture::Wheel { rows })
    }
}

/// Chebyshev distance between two cells (the pair tolerance is a
/// square, not a radius).
fn cells_apart(col_a: u16, row_a: u16, col_b: u16, row_b: u16) -> u16 {
    col_a.abs_diff(col_b).max(row_a.abs_diff(row_b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn down(button: PointerButton, col: u16, row: u16) -> PointerEvent {
        PointerEvent {
            kind: PointerKind::Down,
            button,
            col,
            row,
        }
    }

    fn move_to(col: u16, row: u16) -> PointerEvent {
        PointerEvent {
            kind: PointerKind::Move,
            button: PointerButton::Left,
            col,
            row,
        }
    }

    fn wheel(rows: i32) -> PointerEvent {
        PointerEvent {
            kind: PointerKind::Wheel { rows },
            button: PointerButton::Middle,
            col: 1,
            row: 1,
        }
    }

    #[test]
    fn a_press_emits_click_and_remembers_the_pair() {
        let mut gestures = Gestures::new();
        assert_eq!(
            gestures.on_event(down(PointerButton::Left, 10, 5), 1_000),
            Some(Gesture::Click {
                button: PointerButton::Left,
                col: 10,
                row: 5,
            }),
            "the first press of a pair is a plain Click"
        );
    }

    #[test]
    fn double_click_inside_the_window() {
        let mut gestures = Gestures::new();
        gestures.on_event(down(PointerButton::Left, 10, 5), 1_000);
        assert_eq!(
            gestures.on_event(
                down(PointerButton::Left, 10, 5),
                1_000 + DOUBLE_CLICK_MS - 1
            ),
            Some(Gesture::DoubleClick {
                button: PointerButton::Left,
                col: 10,
                row: 5,
            }),
            "strictly inside 400 ms is a double-click"
        );
    }

    #[test]
    fn double_click_boundary_is_strict() {
        let mut gestures = Gestures::new();
        gestures.on_event(down(PointerButton::Left, 10, 5), 1_000);
        assert_eq!(
            gestures.on_event(down(PointerButton::Left, 10, 5), 1_000 + DOUBLE_CLICK_MS),
            Some(Gesture::Click {
                button: PointerButton::Left,
                col: 10,
                row: 5,
            }),
            "at exactly the window edge it is a second click, not a pair"
        );
    }

    #[test]
    fn double_click_tolerates_one_cell_of_drift() {
        let mut gestures = Gestures::new();
        gestures.on_event(down(PointerButton::Left, 10, 5), 1_000);
        assert!(matches!(
            gestures.on_event(down(PointerButton::Left, 11, 6), 1_100),
            Some(Gesture::DoubleClick { .. })
        ));
        let mut far = Gestures::new();
        far.on_event(down(PointerButton::Left, 10, 5), 1_000);
        assert_eq!(
            far.on_event(down(PointerButton::Left, 12, 5), 1_100),
            Some(Gesture::Click {
                button: PointerButton::Left,
                col: 12,
                row: 5,
            }),
            "two cells away the pair breaks"
        );
    }

    #[test]
    fn pair_requires_the_same_button() {
        let mut gestures = Gestures::new();
        gestures.on_event(down(PointerButton::Left, 10, 5), 1_000);
        assert_eq!(
            gestures.on_event(down(PointerButton::Right, 10, 5), 1_100),
            Some(Gesture::Click {
                button: PointerButton::Right,
                col: 10,
                row: 5,
            })
        );
    }

    #[test]
    fn wheel_notches_coalesce_into_one_scroll() {
        let mut gestures = Gestures::new();
        assert_eq!(gestures.on_event(wheel(3), 1), None, "wheel only folds");
        let _ = gestures.on_event(wheel(3), 2);
        let _ = gestures.on_event(wheel(3), 3);
        assert_eq!(
            gestures.drain_wheel(),
            Some(Gesture::Wheel { rows: 9 }),
            "a trackpad flood drains as one scroll"
        );
        assert_eq!(gestures.drain_wheel(), None, "the second drain is empty");
    }

    #[test]
    fn wheel_signs_survive_folding() {
        let mut gestures = Gestures::new();
        let _ = gestures.on_event(wheel(-3), 1);
        let _ = gestures.on_event(wheel(-3), 2);
        assert_eq!(
            gestures.drain_wheel(),
            Some(Gesture::Wheel { rows: -6 }),
            "upward scrolls drain negative"
        );
        // Offsetting signs cancel to zero: no gesture, not Wheel { 0 }.
        let _ = gestures.on_event(wheel(-3), 3);
        let _ = gestures.on_event(wheel(3), 4);
        assert_eq!(gestures.drain_wheel(), None);
    }

    #[test]
    fn hover_and_leave() {
        let mut gestures = Gestures::new();
        assert_eq!(
            gestures.on_event(move_to(4, 7), 1),
            Some(Gesture::Hover { col: 4, row: 7 })
        );
        assert_eq!(
            gestures.on_event(
                PointerEvent {
                    kind: PointerKind::Gone,
                    button: PointerButton::Left,
                    col: 0,
                    row: 0
                },
                2
            ),
            Some(Gesture::Leave)
        );
    }

    #[test]
    fn up_and_drag_carry_no_gesture() {
        let mut gestures = Gestures::new();
        let drag = PointerEvent {
            kind: PointerKind::Drag,
            button: PointerButton::Left,
            col: 3,
            row: 3,
        };
        let up = PointerEvent {
            kind: PointerKind::Up,
            button: PointerButton::Left,
            col: 4,
            row: 4,
        };
        assert_eq!(gestures.on_event(drag, 1), None);
        assert_eq!(gestures.on_event(up, 2), None);
    }

    // ------------------------------------------- property tests (audit)
    use proptest::prelude::*;

    fn wheel_of(rows: i32) -> PointerEvent {
        PointerEvent {
            kind: PointerKind::Wheel { rows },
            button: PointerButton::Middle,
            col: 1,
            row: 1,
        }
    }

    /// kind 0=Down 1=Up 2=Drag 3=Move 4=Wheel 5=Gone.
    fn streamed_event(kind: u8, col: u16, row: u16, rows: i32) -> PointerEvent {
        let base = PointerEvent {
            kind: PointerKind::Move,
            button: PointerButton::Left,
            col,
            row,
        };
        match kind {
            0 => PointerEvent {
                kind: PointerKind::Down,
                ..base
            },
            1 => PointerEvent {
                kind: PointerKind::Up,
                ..base
            },
            2 => PointerEvent {
                kind: PointerKind::Drag,
                ..base
            },
            4 => wheel_of(rows),
            5 => PointerEvent {
                kind: PointerKind::Gone,
                ..base
            },
            _ => base,
        }
    }

    fn button_strategy() -> impl Strategy<Value = PointerButton> {
        proptest::sample::select(vec![
            PointerButton::Left,
            PointerButton::Middle,
            PointerButton::Right,
        ])
    }

    proptest! {
        #![proptest_config(crate::proptest_cfg::config())]

        /// A wheel flood folds to exactly its sum and drains once.
        #[test]
        fn prop_wheel_folds_the_sum_and_drains_once(
            events in proptest::collection::vec(-1000i32..1000, 0..50),
        ) {
            let mut gestures = Gestures::new();
            let mut expected = 0i64;
            for rows in events {
                expected += i64::from(rows);
                prop_assert!(
                    gestures.on_event(wheel_of(rows), 1).is_none(),
                    "wheel leaked a gesture"
                );
            }
            match gestures.drain_wheel() {
                Some(Gesture::Wheel { rows }) => prop_assert_eq!(i64::from(rows), expected),
                None => prop_assert_eq!(expected, 0),
                other => panic!("unexpected gesture {other:?}"),
            }
            prop_assert!(gestures.drain_wheel().is_none(), "the second drain is not empty");
        }

        /// A second press is a double-click exactly when the pair
        /// predicate holds: same button, strictly inside 400 ms, within
        /// one cell.
        #[test]
        fn prop_double_click_follows_the_pair_predicate(
            b1 in button_strategy(),
            b2 in button_strategy(),
            c1 in any::<u16>(),
            r1 in any::<u16>(),
            c2 in any::<u16>(),
            r2 in any::<u16>(),
            gap in 0..1_000u64,
        ) {
            let mut gestures = Gestures::new();
            let first = gestures.on_event(down(b1, c1, r1), 0);
            assert!(matches!(first, Some(Gesture::Click { .. })), "first press must click");
            let second = gestures.on_event(down(b2, c2, r2), gap);
            let pairs = b1 == b2
                && gap < DOUBLE_CLICK_MS
                && c1.abs_diff(c2).max(r1.abs_diff(r2)) <= DOUBLE_CLICK_CELLS;
            let expected = if pairs {
                Gesture::DoubleClick { button: b2, col: c2, row: r2 }
            } else {
                Gesture::Click { button: b2, col: c2, row: r2 }
            };
            prop_assert_eq!(second, Some(expected));
        }

        /// Arbitrary event streams keep the wheel ledger: what went in
        /// comes back out across drains, and the machine never wedges.
        #[test]
        fn prop_arbitrary_streams_keep_the_wheel_ledger(
            stream in proptest::collection::vec((0u8..6, any::<u16>(), any::<u16>(), -50i32..50), 0..60),
        ) {
            let mut gestures = Gestures::new();
            let mut fed = 0i64;
            let mut drained = 0i64;
            for (index, (kind, col, row, rows)) in stream.into_iter().enumerate() {
                let event = streamed_event(kind, col, row, rows);
                if let PointerKind::Wheel { rows } = event.kind {
                    fed += i64::from(rows);
                }
                let _ = gestures.on_event(event, index as u64 * 10);
                if let Some(Gesture::Wheel { rows }) = gestures.drain_wheel() {
                    drained += i64::from(rows);
                }
            }
            if let Some(Gesture::Wheel { rows }) = gestures.drain_wheel() {
                drained += i64::from(rows);
            }
            prop_assert_eq!(fed, drained, "wheel rows vanished or duplicated");
        }
    }
}

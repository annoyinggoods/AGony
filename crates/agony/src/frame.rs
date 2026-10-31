//! The frame: the one place the shell asks for screen regions.
//!
//! Core functions: `regions` — the core's `frame_regions` with the
//! taskbar's height and the settings' taskbar position (Part08).
//! Depends on: agony_core::layout (the math), agony_core::config (the
//! position enum).
//! Called by: app (layout each draw and on resize).
//! Calls: nothing.

use agony_core::config::TaskbarPosition;
use agony_core::layout::{FrameRegions, Rect};

pub const TASKBAR_ROWS: u16 = 1;

pub fn regions(area: Rect, position: TaskbarPosition) -> FrameRegions {
    agony_core::layout::frame_regions(area, TASKBAR_ROWS, position)
}

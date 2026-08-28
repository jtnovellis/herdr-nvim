//! Guillotine layout planner: given the rectangles of a tab's panes, produce
//! the sequence of `pane move` steps that rebuilds that arrangement inside
//! one half of the tab, next to a full-height sidebar.
//!
//! Adapted from ChmaraX/herdr-nvim (MIT); see THIRD_PARTY.md.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dir {
    Right,
    Down,
}

impl Dir {
    pub fn as_cli_arg(self) -> &'static str {
        match self {
            Dir::Right => "right",
            Dir::Down => "down",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneRect {
    pub pane_id: String,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MoveStep {
    pub pane: String,
    pub dir: Dir,
    pub target: String,
    pub ratio: f64,
}

pub struct RebuildPlan {
    /// The pane that stays in the tab and becomes the root of the rebuilt half.
    pub anchor: String,
    pub steps: Vec<MoveStep>,
}

const GAP: u32 = 2;

/// One partition axis: direction, bounds, and the rect accessors for it.
type Axis = (Dir, u32, u32, fn(&PaneRect) -> u32, fn(&PaneRect) -> u32);

fn bounds(rects: &[PaneRect]) -> (u32, u32, u32, u32) {
    let x0 = rects.iter().map(|r| r.x).min().unwrap();
    let y0 = rects.iter().map(|r| r.y).min().unwrap();
    let x1 = rects.iter().map(|r| r.x + r.w).max().unwrap();
    let y1 = rects.iter().map(|r| r.y + r.h).max().unwrap();
    (x0, y0, x1, y1)
}

fn x_start(r: &PaneRect) -> u32 {
    r.x
}
fn x_extent(r: &PaneRect) -> u32 {
    r.w
}
fn y_start(r: &PaneRect) -> u32 {
    r.y
}
fn y_extent(r: &PaneRect) -> u32 {
    r.h
}

/// A cut along one axis that separates the rects into two non-empty groups
/// that do not overlap (zero tolerance on the overlap check itself).
fn cut(
    rects: &[PaneRect],
    lo: u32,
    hi: u32,
    start: fn(&PaneRect) -> u32,
    extent: fn(&PaneRect) -> u32,
) -> Option<u32> {
    let end = |r: &PaneRect| start(r) + extent(r);
    let mut edges: Vec<u32> = rects
        .iter()
        .map(end)
        .filter(|&edge| edge > lo + GAP && edge + GAP < hi)
        .collect();
    edges.sort_unstable();
    edges.dedup();
    edges.into_iter().find(|&c| {
        let before_max_end = rects.iter().filter(|r| end(r) <= c).map(end).max();
        let after_min_start = rects.iter().filter(|r| end(r) > c).map(start).min();
        match (before_max_end, after_min_start) {
            (Some(before), Some(after)) => after >= before,
            _ => false,
        }
    })
}

fn partition(rects: &[PaneRect]) -> Result<(String, Vec<MoveStep>)> {
    if rects.len() == 1 {
        return Ok((rects[0].pane_id.clone(), vec![]));
    }
    let (x0, y0, x1, y1) = bounds(rects);
    let axes: [Axis; 2] = [
        (Dir::Right, x0, x1, x_start, x_extent),
        (Dir::Down, y0, y1, y_start, y_extent),
    ];
    for (dir, lo, hi, start, extent) in axes {
        if let Some(cut_pos) = cut(rects, lo, hi, start, extent) {
            let (first, second): (Vec<_>, Vec<_>) = rects
                .iter()
                .cloned()
                .partition(|r| start(r) + extent(r) <= cut_pos);
            let ratio = (cut_pos - lo) as f64 / (hi - lo) as f64;
            return combine(first, second, dir, ratio);
        }
    }
    bail!(
        "layout is not guillotine-partitionable ({} panes)",
        rects.len()
    )
}

fn combine(
    first: Vec<PaneRect>,
    second: Vec<PaneRect>,
    dir: Dir,
    ratio: f64,
) -> Result<(String, Vec<MoveStep>)> {
    let (first_head, first_steps) = partition(&first)?;
    let (second_head, second_steps) = partition(&second)?;
    let mut steps = vec![MoveStep {
        pane: second_head.clone(),
        dir,
        target: first_head.clone(),
        ratio,
    }];
    // Place the second branch's head first, then build both branches using
    // targets that are already present in the reconstructed layout.
    steps.extend(first_steps);
    steps.extend(second_steps);
    Ok((first_head, steps))
}

pub fn plan_rebuild(rects: &[PaneRect]) -> Result<RebuildPlan> {
    if rects.is_empty() {
        bail!("no panes");
    }
    let (anchor, steps) = partition(rects)?;
    Ok(RebuildPlan { anchor, steps })
}

/// Parse a `pane layout` response (`result.layout`) into rects relative to the
/// layout area's origin.
pub fn parse_pane_rects(layout: &Value) -> Result<Vec<PaneRect>> {
    let origin_x = u32_at(layout, "/area/x")?;
    let origin_y = u32_at(layout, "/area/y")?;
    let panes = layout
        .get("panes")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("pane layout has no panes array"))?;
    panes
        .iter()
        .map(|pane| {
            let pane_id = pane
                .get("pane_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("pane without pane_id in layout"))?
                .to_string();
            Ok(PaneRect {
                pane_id,
                x: u32_at(pane, "/rect/x")?.saturating_sub(origin_x),
                y: u32_at(pane, "/rect/y")?.saturating_sub(origin_y),
                w: u32_at(pane, "/rect/width")?,
                h: u32_at(pane, "/rect/height")?,
            })
        })
        .collect()
}

fn u32_at(value: &Value, pointer: &str) -> Result<u32> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| anyhow::anyhow!("layout JSON missing integer at {pointer}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(id: &str, x: u32, y: u32, w: u32, h: u32) -> PaneRect {
        PaneRect {
            pane_id: id.into(),
            x,
            y,
            w,
            h,
        }
    }

    fn fixture(name: &str) -> Vec<PaneRect> {
        let raw = match name {
            "1" => include_str!("../tests/fixtures/layout_1pane.json"),
            "3" => include_str!("../tests/fixtures/layout_3pane.json"),
            _ => include_str!("../tests/fixtures/layout_4pane.json"),
        };
        let value: Value = serde_json::from_str(raw).unwrap();
        parse_pane_rects(value.pointer("/result/layout").unwrap()).unwrap()
    }

    #[test]
    fn single_pane_plan_is_anchor_only() {
        let p = plan_rebuild(&[r("p1", 0, 0, 100, 50)]).unwrap();
        assert_eq!(p.anchor, "p1");
        assert!(p.steps.is_empty());
    }

    #[test]
    fn two_columns() {
        let p = plan_rebuild(&[r("a", 0, 0, 40, 50), r("b", 41, 0, 59, 50)]).unwrap();
        assert_eq!(p.anchor, "a");
        assert_eq!(p.steps.len(), 1);
        let s = &p.steps[0];
        assert_eq!((s.pane.as_str(), s.target.as_str()), ("b", "a"));
        assert_eq!(s.dir, Dir::Right);
        assert!((s.ratio - 0.4).abs() < 0.03);
    }

    #[test]
    fn asymmetric_three_pane() {
        let p = plan_rebuild(&[
            r("a", 0, 0, 40, 52),
            r("b", 41, 0, 59, 15),
            r("c", 41, 16, 59, 36),
        ])
        .unwrap();
        assert_eq!(p.anchor, "a");
        assert_eq!(p.steps.len(), 2);
        assert_eq!(p.steps[0].pane, "b");
        assert_eq!(p.steps[0].dir, Dir::Right);
        assert_eq!(p.steps[1].pane, "c");
        assert_eq!(p.steps[1].target, "b");
        assert_eq!(p.steps[1].dir, Dir::Down);
        assert!((p.steps[1].ratio - 0.3).abs() < 0.05);
    }

    #[test]
    fn grid_2x2() {
        let p = plan_rebuild(&[
            r("a", 0, 0, 50, 25),
            r("b", 51, 0, 49, 25),
            r("c", 0, 26, 50, 26),
            r("d", 51, 26, 49, 26),
        ])
        .unwrap();
        assert_eq!(p.steps.len(), 3);
    }

    #[test]
    fn overlapping_rects_error() {
        assert!(plan_rebuild(&[r("a", 0, 0, 60, 50), r("b", 30, 0, 70, 50)]).is_err());
        assert!(plan_rebuild(&[r("a", 0, 0, 50, 50), r("b", 49, 0, 51, 50)]).is_err());
        assert!(plan_rebuild(&[]).is_err());
    }

    #[test]
    fn fixtures_parse_and_plan() {
        assert_eq!(fixture("1").len(), 1);
        let three = fixture("3");
        assert_eq!(three.len(), 3);
        assert_eq!(three[0], r("w0:p1", 0, 0, 72, 52));
        let four = fixture("4");
        assert_eq!(plan_rebuild(&four).unwrap().steps.len(), four.len() - 1);
    }
}

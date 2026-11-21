//! Squarified treemap layout (Bruls, Huizing, van Wijk): items become rectangles whose areas
//! are proportional to their weights and whose shapes stay as close to squares as possible, so
//! labels fit and small items stay visible.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn area(&self) -> f32 {
        self.w * self.h
    }

    /// The rectangle shrunk by `d` on every side (never below zero size).
    pub fn inset(&self, d: f32) -> Rect {
        let w = (self.w - 2.0 * d).max(0.0);
        let h = (self.h - 2.0 * d).max(0.0);
        Rect { x: self.x + (self.w - w) / 2.0, y: self.y + (self.h - h) / 2.0, w, h }
    }
}

/// Worst aspect ratio (>= 1) of a row of areas laid along a side of length `side`.
fn worst(row: &[f64], sum: f64, side: f64) -> f64 {
    if sum <= 0.0 || side <= 0.0 {
        return f64::MAX;
    }
    let (mut min, mut max) = (f64::MAX, 0.0f64);
    for &a in row {
        min = min.min(a);
        max = max.max(a);
    }
    let s2 = side * side;
    ((s2 * max) / (sum * sum)).max((sum * sum) / (s2 * min))
}

/// Lays `weights` out inside `bounds`. `weights` should be sorted descending (the algorithm
/// still works otherwise, with worse shapes); zero weights get an empty rectangle.
/// The result has one rectangle per weight, in the same order. The rectangles tile `bounds`
/// without overlap; their areas are proportional to the weights.
pub fn squarify(weights: &[u64], bounds: Rect) -> Vec<Rect> {
    let mut out = vec![Rect::new(bounds.x, bounds.y, 0.0, 0.0); weights.len()];
    let total: f64 = weights.iter().map(|&w| w as f64).sum();
    if total <= 0.0 || bounds.w <= 0.0 || bounds.h <= 0.0 {
        return out;
    }
    let scale = bounds.area() as f64 / total;
    // work in f64 so long lists of tiny items do not drift
    let (mut x, mut y, mut w, mut h) = (bounds.x as f64, bounds.y as f64, bounds.w as f64, bounds.h as f64);
    let areas: Vec<f64> = weights.iter().map(|&v| v as f64 * scale).collect();
    let n = areas.iter().position(|&a| a <= 0.0).unwrap_or(areas.len());
    let mut i = 0;
    while i < n {
        // the row goes along the shorter side
        let side = w.min(h);
        let mut j = i + 1;
        let mut sum = areas[i];
        while j < n {
            let next = sum + areas[j];
            if worst(&areas[i..=j], next, side) <= worst(&areas[i..j], sum, side) {
                sum = next;
                j += 1;
            } else {
                break;
            }
        }
        // thickness of the row; the last row takes what is left exactly
        let last = j == n;
        if w >= h {
            // a column on the left, items stacked top to bottom
            let cw = if last { w } else { (sum / h).min(w) };
            let mut cy = y;
            for k in i..j {
                let ch = if k == j - 1 { y + h - cy } else { areas[k] / cw };
                out[k] = Rect::new(x as f32, cy as f32, cw as f32, ch as f32);
                cy += ch;
            }
            x += cw;
            w -= cw;
        } else {
            let rh = if last { h } else { (sum / w).min(h) };
            let mut cx = x;
            for k in i..j {
                let cw = if k == j - 1 { x + w - cx } else { areas[k] / rh };
                out[k] = Rect::new(cx as f32, y as f32, cw as f32, rh as f32);
                cx += cw;
            }
            y += rh;
            h -= rh;
        }
        i = j;
    }
    out
}

// ---------- sunburst

/// One folder or file of a sunburst, with its children one ring further out.
#[derive(Debug, Clone)]
pub struct RingNode {
    pub id: u32,
    pub weight: u64,
    pub children: Vec<RingNode>,
}

/// One ring segment: angles in radians, 0 at twelve o'clock, growing clockwise.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Arc {
    pub id: u32,
    /// 0 is the ring next to the centre.
    pub depth: u8,
    pub a0: f32,
    pub a1: f32,
}

/// Segments narrower than this (radians, about one degree) are left out with everything below them.
pub const MIN_SWEEP: f32 = 0.0175;

/// Lays `nodes` (sorted biggest first) around the full circle; each node's children share its
/// span in proportion to their weights, one ring further out.
pub fn sunburst(nodes: &[RingNode]) -> Vec<Arc> {
    let mut out = Vec::new();
    spread(nodes, 0, 0.0, std::f32::consts::TAU, nodes.iter().map(|n| n.weight).sum(), &mut out);
    out
}

fn spread(nodes: &[RingNode], depth: u8, from: f32, to: f32, total: u64, out: &mut Vec<Arc>) {
    if total == 0 {
        return;
    }
    let span = to - from;
    let mut a = from;
    for n in nodes {
        let sweep = span * (n.weight as f32 / total as f32);
        if sweep >= MIN_SWEEP && n.weight > 0 {
            out.push(Arc { id: n.id, depth, a0: a, a1: a + sweep });
            let sum: u64 = n.children.iter().map(|c| c.weight).sum();
            // a folder's children never claim more than the folder itself
            spread(&n.children, depth + 1, a, a + sweep, n.weight.max(sum), out);
        }
        a += sweep;
    }
}

/// Inner and outer radius of the ring at `depth`; `rings` rings share the space between `hole` and `outer`.
pub fn ring_bounds(depth: u8, hole: f32, outer: f32, rings: u8) -> (f32, f32) {
    let w = (outer - hole).max(0.0) / rings.max(1) as f32;
    (hole + w * depth as f32, hole + w * (depth as f32 + 1.0))
}

/// The segment under the point (`dx`, `dy`) measured from the centre, if any.
pub fn sunburst_hit(arcs: &[Arc], dx: f32, dy: f32, hole: f32, outer: f32, rings: u8) -> Option<usize> {
    let r = dx.hypot(dy);
    if r < hole || r >= outer {
        return None;
    }
    let mut ang = dx.atan2(-dy);
    if ang < 0.0 {
        ang += std::f32::consts::TAU;
    }
    let rw = (outer - hole) / rings.max(1) as f32;
    let depth = ((r - hole) / rw) as u8;
    arcs.iter().position(|a| a.depth == depth && ang >= a.a0 && ang < a.a1)
}

// ---------- skyline

/// Bars for `weights` (biggest first) standing on the bottom edge of `bounds`: the biggest is as
/// tall as the area, the rest in proportion, `gap` apart, between `min_bar` and `max_bar` wide.
/// Only as many bars as fit are returned (the rest of the list is the long tail).
pub fn skyline(weights: &[u64], bounds: Rect, gap: f32, min_bar: f32, max_bar: f32) -> Vec<Rect> {
    let max = weights.iter().copied().max().unwrap_or(0);
    if max == 0 || bounds.w < min_bar || bounds.h <= 0.0 {
        return Vec::new();
    }
    let n = weights.len().min(((bounds.w + gap) / (min_bar + gap)) as usize).max(1);
    let bar = ((bounds.w - gap * (n as f32 - 1.0)) / n as f32).clamp(min_bar, max_bar);
    weights[..n]
        .iter()
        .enumerate()
        .map(|(i, &w)| {
            let h = bounds.h * (w as f32 / max as f32);
            Rect::new(bounds.x + i as f32 * (bar + gap), bounds.y + bounds.h - h, bar, h)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlap(a: &Rect, b: &Rect) -> f32 {
        let w = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
        let h = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
        if w > 0.0 && h > 0.0 { w * h } else { 0.0 }
    }

    fn check(weights: &[u64], bounds: Rect) {
        let r = squarify(weights, bounds);
        assert_eq!(r.len(), weights.len());
        let total: u64 = weights.iter().sum();
        let mut sum = 0.0;
        for (i, (rc, &w)) in r.iter().zip(weights).enumerate() {
            let want = bounds.area() * (w as f32 / total as f32);
            assert!((rc.area() - want).abs() <= want * 0.001 + 0.05, "item {i}: area {} want {want}", rc.area());
            assert!(rc.x >= bounds.x - 0.01 && rc.y >= bounds.y - 0.01);
            assert!(rc.x + rc.w <= bounds.x + bounds.w + 0.01 && rc.y + rc.h <= bounds.y + bounds.h + 0.01);
            sum += rc.area();
        }
        assert!((sum - bounds.area()).abs() <= bounds.area() * 0.001, "fill: {sum} of {}", bounds.area());
        for a in 0..r.len() {
            for b in a + 1..r.len() {
                assert!(overlap(&r[a], &r[b]) <= 0.5, "overlap {a}/{b}: {:?} {:?}", r[a], r[b]);
            }
        }
    }

    #[test]
    fn fills_the_area_without_overlap() {
        check(&[6, 6, 4, 3, 2, 2, 1], Rect::new(0.0, 0.0, 600.0, 400.0));
        check(&[100, 50, 25, 12, 6, 3, 1, 1, 1], Rect::new(10.0, 20.0, 300.0, 700.0));
        check(&[1], Rect::new(0.0, 0.0, 50.0, 80.0));
        let many: Vec<u64> = (1..=500u64).rev().collect();
        check(&many, Rect::new(0.0, 0.0, 1000.0, 600.0));
    }

    #[test]
    fn zero_weights_and_empty_bounds() {
        let r = squarify(&[10, 5, 0, 0], Rect::new(0.0, 0.0, 100.0, 100.0));
        assert_eq!(r[2].area(), 0.0);
        assert!((r[0].area() + r[1].area() - 10000.0).abs() < 1.0);
        assert!(squarify(&[], Rect::new(0.0, 0.0, 10.0, 10.0)).is_empty());
        assert!(squarify(&[1, 2], Rect::new(0.0, 0.0, 0.0, 10.0)).iter().all(|r| r.area() == 0.0));
    }

    #[test]
    fn shapes_stay_reasonable() {
        let r = squarify(&[6, 6, 4, 3, 2, 2, 1], Rect::new(0.0, 0.0, 600.0, 400.0));
        for rc in &r {
            let ar = (rc.w / rc.h).max(rc.h / rc.w);
            assert!(ar < 4.0, "{rc:?}");
        }
    }

    fn node(id: u32, weight: u64, children: Vec<RingNode>) -> RingNode {
        RingNode { id, weight, children }
    }

    #[test]
    fn sunburst_splits_the_circle_by_weight() {
        let tree = vec![node(1, 60, vec![node(3, 30, vec![]), node(4, 30, vec![])]), node(2, 40, vec![])];
        let a = sunburst(&tree);
        assert_eq!(a.len(), 4);
        let tau = std::f32::consts::TAU;
        assert!((a[0].a1 - a[0].a0 - tau * 0.6).abs() < 1e-4);
        assert!((a[3].a0 - a[0].a1).abs() < 1e-4 && (a[3].a1 - tau).abs() < 1e-4);
        // children sit one ring out, inside the parent's span, and split it evenly
        assert_eq!((a[1].depth, a[2].depth), (1, 1));
        assert!((a[1].a0 - a[0].a0).abs() < 1e-4 && (a[2].a1 - a[0].a1).abs() < 1e-4);
        assert!(((a[1].a1 - a[1].a0) - (a[2].a1 - a[2].a0)).abs() < 1e-4);
    }

    #[test]
    fn sunburst_drops_slivers_and_finds_hits() {
        let tree = vec![node(1, 1_000_000, vec![]), node(2, 1, vec![node(9, 1, vec![])])];
        let a = sunburst(&tree);
        assert_eq!(a.len(), 1);
        // straight up from the centre is the start of the first segment; right of it is inside it, too
        assert_eq!(sunburst_hit(&a, 0.0, -30.0, 20.0, 100.0, 4), Some(0));
        assert_eq!(sunburst_hit(&a, 30.0, 0.0, 20.0, 100.0, 4), Some(0));
        assert_eq!(sunburst_hit(&a, 5.0, 0.0, 20.0, 100.0, 4), None);
        assert_eq!(sunburst_hit(&a, 0.0, -150.0, 20.0, 100.0, 4), None);
        assert_eq!(sunburst_hit(&a, 0.0, -90.0, 20.0, 100.0, 4), None);
        let (i, o) = ring_bounds(1, 20.0, 100.0, 4);
        assert_eq!((i, o), (40.0, 60.0));
    }

    #[test]
    fn skyline_heights_follow_weights() {
        let r = skyline(&[100, 50, 25, 0], Rect::new(10.0, 20.0, 400.0, 200.0), 4.0, 10.0, 60.0);
        assert_eq!(r.len(), 4);
        assert!((r[0].h - 200.0).abs() < 1e-3 && (r[1].h - 100.0).abs() < 1e-3 && r[3].h == 0.0);
        assert!(r.iter().all(|b| (b.y + b.h - 220.0).abs() < 1e-3 && b.w == 60.0));
        assert!((r[1].x - (10.0 + 64.0)).abs() < 1e-3);
        // too narrow for all: only the biggest ones are drawn
        let many: Vec<u64> = (1..=100u64).rev().collect();
        let r = skyline(&many, Rect::new(0.0, 0.0, 100.0, 50.0), 2.0, 8.0, 40.0);
        assert_eq!(r.len(), 10);
        assert!(skyline(&[], Rect::new(0.0, 0.0, 100.0, 50.0), 2.0, 8.0, 40.0).is_empty());
    }
}

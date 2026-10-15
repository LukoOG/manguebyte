//! Damage: the device rectangles a frame has to draw again.

use manguebyte_gfx::IRect;

/// The most damage rectangles a frame reports. More are merged, the pair
/// whose union wastes the fewest pixels first.
pub const MAX_DAMAGE: usize = 8;

/// Keeps the parts of `rects` inside `bounds`, drops the empty ones and
/// those inside another, and merges until at most `most` are left.
pub(crate) fn tidy(rects: &mut Vec<IRect>, bounds: IRect, most: usize) {
    rects.retain_mut(|rect| match rect.intersect(&bounds) {
        Some(inside) => {
            *rect = inside;
            true
        }
        None => false,
    });
    drop_covered(rects);
    let most = most.max(1);
    while rects.len() > most {
        let mut best = (0, 1, u64::MAX);
        for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                let union = rects[i].union(&rects[j]);
                let waste = union
                    .area()
                    .saturating_sub(rects[i].area())
                    .saturating_sub(rects[j].area());
                if waste < best.2 {
                    best = (i, j, waste);
                }
            }
        }
        let (i, j, _) = best;
        rects[i] = rects[i].union(&rects[j]);
        rects.swap_remove(j);
        drop_covered(rects);
    }
}

/// Removes every rectangle that lies inside another (of two equal ones,
/// the later).
fn drop_covered(rects: &mut Vec<IRect>) {
    let mut i = 0;
    while i < rects.len() {
        let covered = (0..rects.len()).any(|j| {
            j != i && rects[j].contains_rect(&rects[i]) && (rects[j] != rects[i] || j < i)
        });
        if covered {
            rects.remove(i);
        } else {
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectangles_inside_others_and_outside_the_bounds_go() {
        let bounds = IRect::new(0, 0, 100, 100);
        let mut rects = vec![
            IRect::new(10, 10, 20, 20),
            IRect::new(12, 12, 4, 4),
            IRect::new(10, 10, 20, 20),
            IRect::new(200, 0, 5, 5),
            IRect::new(90, 90, 20, 20),
        ];
        tidy(&mut rects, bounds, MAX_DAMAGE);
        assert_eq!(
            rects,
            [IRect::new(10, 10, 20, 20), IRect::new(90, 90, 10, 10)]
        );
    }

    #[test]
    fn too_many_are_merged_with_their_nearest() {
        let bounds = IRect::new(0, 0, 1000, 1000);
        // Two clusters of five small rectangles.
        let mut rects: Vec<IRect> = (0..5)
            .map(|i| IRect::new(i * 10, 0, 8, 8))
            .chain((0..5).map(|i| IRect::new(900 + i * 10, 900, 8, 8)))
            .collect();
        tidy(&mut rects, bounds, 2);
        rects.sort_by_key(|r| (r.x, r.y));
        assert_eq!(
            rects,
            [IRect::new(0, 0, 48, 8), IRect::new(900, 900, 48, 8)]
        );
    }
}

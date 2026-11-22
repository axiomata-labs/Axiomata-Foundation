//! The sizes of a split's children: fractions that are finite, sum to 1 and each reach the floor.

/// The smallest fraction of its split a pane may have.
///
/// Below roughly this a pane is a sliver with no reachable content and no grabbable divider — a state one careless
/// drag enters and another cannot leave. A drag that would push a neighbour under it stops at it instead.
pub const MIN_PANE_FRACTION: f64 = 0.08;

/// Fractions for `count` children: finite, summing to 1, each at least the floor ([`apply_floor`]).
///
/// A missing or unusable entry gets the average of the usable ones rather than zero: zero would be a child that exists,
/// cannot be seen and cannot be dragged back. Every size in the tree passes through here — a preset, a flattened split,
/// a stored layout — so none of them can bring back the sliver the floor exists to prevent.
pub(crate) fn normalize(sizes: &[f64], count: usize) -> Vec<f64> {
    let usable: Vec<Option<f64>> = (0..count)
        .map(|i| {
            sizes
                .get(i)
                .copied()
                .filter(|size| size.is_finite() && *size > 0.0)
        })
        .collect();
    let known: Vec<f64> = usable.iter().flatten().copied().collect();
    let fallback = if known.is_empty() {
        1.0
    } else {
        known.iter().sum::<f64>() / count_f64(known.len())
    };
    let filled: Vec<f64> = usable.iter().map(|size| size.unwrap_or(fallback)).collect();
    let total: f64 = filled.iter().sum();
    apply_floor(filled.iter().map(|size| size / total).collect())
}

/// Lifts every share to the floor, taking what that costs from the others in proportion.
///
/// The floor is [`MIN_PANE_FRACTION`], or an even share when a split holds so many children that the constant could
/// not be met by all of them. Lifting one child can push another under the floor in turn, so this repeats — at most
/// once per child, since each pass pins at least one more.
fn apply_floor(mut shares: Vec<f64>) -> Vec<f64> {
    let count = shares.len();
    if count == 0 {
        return shares;
    }
    let floor = MIN_PANE_FRACTION.min(1.0 / count_f64(count));
    for _ in 0..count {
        let pinned: Vec<bool> = shares.iter().map(|share| *share < floor).collect();
        let pinned_count = pinned.iter().filter(|p| **p).count();
        if pinned_count == 0 {
            break;
        }
        let room = 1.0 - count_f64(pinned_count) * floor;
        let rest: f64 = shares
            .iter()
            .zip(&pinned)
            .filter(|(_, pinned)| !**pinned)
            .map(|(share, _)| share)
            .sum();
        let free = count_f64(count - pinned_count);
        for (share, pinned) in shares.iter_mut().zip(&pinned) {
            *share = if *pinned {
                floor
            } else if rest > 0.0 {
                *share / rest * room
            } else {
                room / free
            };
        }
    }
    shares
}

/// A child count as a float: a split's children are far fewer than `f64` holds exactly.
#[allow(
    clippy::cast_precision_loss,
    reason = "a split has a handful of children"
)]
pub(crate) fn count_f64(count: usize) -> f64 {
    count as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sum(sizes: &[f64]) -> f64 {
        sizes.iter().sum()
    }

    #[test]
    fn sizes_are_scaled_to_one_and_a_missing_one_takes_the_average() {
        assert_eq!(normalize(&[3.0, 1.0], 2), [0.75, 0.25]);
        assert_eq!(normalize(&[2.0], 2), [0.5, 0.5]);
        assert!((sum(&normalize(&[0.0, -1.0], 2)) - 1.0).abs() < 1e-9);
        assert!((sum(&normalize(&[f64::NAN, f64::INFINITY, 1.0], 3)) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn no_share_falls_under_the_floor() {
        let sliver = normalize(&[0.001, 0.999], 2);
        assert!((sliver[0] - MIN_PANE_FRACTION).abs() < 1e-9);
        assert!((sum(&sliver) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn many_children_share_evenly_rather_than_break_the_sum() {
        let count = 16;
        let mut raw = vec![0.1 / 15.0; count];
        raw[0] = 0.9;
        let sizes = normalize(&raw, count);
        assert!((sum(&sizes) - 1.0).abs() < 1e-9);
        let least = sizes.iter().copied().fold(f64::INFINITY, f64::min);
        assert!((least - 1.0 / 16.0).abs() < 1e-9);
    }
}

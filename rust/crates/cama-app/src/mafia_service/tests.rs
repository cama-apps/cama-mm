use super::*;

#[test]
fn test_role_counts_table() {
    let table = [
        (4, (0, 0, 0, 0)),
        (5, (1, 1, 1, 0)),
        (6, (1, 1, 1, 0)),
        (7, (2, 1, 1, 0)),
        (9, (2, 1, 1, 0)),
        (10, (3, 1, 1, 1)),
        (12, (3, 1, 1, 1)),
        (13, (4, 1, 1, 1)),
        (15, (4, 1, 1, 1)),
        (16, (0, 0, 0, 0)),
    ];
    for (size, (mafia, doctor, detective, vigilante)) in table {
        assert_eq!(
            role_counts(size),
            RoleCounts {
                mafia,
                doctor,
                detective,
                vigilante,
            },
            "roster {size}"
        );
    }
}

#[test]
fn test_payout_splits_faction_pot_with_mvp_bonus() {
    let plan = compute_payouts(56, &[2, 3, 4, 5, 6, 7], Some(2), None);
    let base = (56 - MVP_BONUS) / 6;
    assert_eq!(plan.payout_per_winner, base);
    assert_eq!(plan.deltas[&2], base + MVP_BONUS + (56 - MVP_BONUS) % 6);
    assert_eq!(plan.deltas[&7], base);
    assert_eq!(plan.nonprofit_overflow, 0);
    assert_eq!(plan.deltas.values().sum::<i64>(), 56);
}

#[test]
fn test_payout_without_winners_sends_pot_to_nonprofit() {
    let plan = compute_payouts(40, &[], None, None);
    assert!(plan.deltas.is_empty());
    assert_eq!(plan.payout_per_winner, 0);
    assert_eq!(plan.nonprofit_overflow, 40);
}

#[test]
fn test_bookie_takes_capped_cut_before_faction_split() {
    let plan = compute_payouts(120, &[1, 2], None, Some(9));
    assert_eq!(plan.deltas[&9], BOOKIE_PAYOUT);
    assert_eq!(plan.deltas[&1], 35);
    assert_eq!(plan.deltas[&2], 35);
    assert_eq!(plan.nonprofit_overflow, 0);
}

#[test]
fn test_payout_cap_overflow() {
    let plan = compute_payouts(15 * 15, &[1, 2, 3, 4], Some(1), None);
    assert!(
        plan.deltas
            .values()
            .all(|amount| *amount <= MAX_WINNER_PAYOUT)
    );
    assert_eq!(plan.deltas.values().copied().max(), Some(MAX_WINNER_PAYOUT));
    assert_eq!(
        plan.deltas.values().sum::<i64>() + plan.nonprofit_overflow,
        225
    );
    assert!(plan.nonprofit_overflow > 0);
}

#[test]
fn test_mvp_bonus_clamped_to_small_faction_pot() {
    let plan = compute_payouts(60, &[1, 2, 3], Some(1), Some(9));
    let mvp_extra = plan.deltas[&1] - plan.payout_per_winner;
    assert_eq!(mvp_extra, 10);
    assert_eq!(
        plan.deltas.values().sum::<i64>() + plan.nonprofit_overflow,
        60
    );
}

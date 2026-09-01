use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Barrier};
use std::thread;

use super::*;

const GUILD_ID: GuildId = 42;

fn fixture() -> (InMemoryDisburseRepository, DisburseService) {
    let repository = InMemoryDisburseRepository::new();
    let service = DisburseService::new(repository.clone(), 100, 0.40);
    (repository, service)
}

fn setup_players(repository: &InMemoryDisburseRepository) {
    for (discord_id, username, balance) in [
        (1001, "Debtor1", -100),
        (1002, "Debtor2", -50),
        (1003, "Voter1", 100),
        (1004, "Voter2", 100),
        (1005, "Voter3", 100),
    ] {
        repository.add_player(GUILD_ID, discord_id, username);
        repository
            .set_balance(GUILD_ID, discord_id, balance)
            .expect("fixture player exists");
    }
}

fn setup_nonprofit_fund(repository: &InMemoryDisburseRepository) {
    assert_eq!(repository.add_to_fund(GUILD_ID, 300), Ok(300));
}

fn standard_fixture() -> (InMemoryDisburseRepository, DisburseService) {
    let (repository, service) = fixture();
    setup_players(&repository);
    setup_nonprofit_fund(&repository);
    (repository, service)
}

fn amounts(distributions: &[Distribution]) -> BTreeMap<DiscordId, i64> {
    distributions
        .iter()
        .map(|distribution| (distribution.discord_id, distribution.amount))
        .collect()
}

fn balance(repository: &InMemoryDisburseRepository, discord_id: DiscordId) -> i64 {
    repository
        .balance(GUILD_ID, discord_id)
        .expect("fixture player exists")
}

fn vote_twice(service: &DisburseService, method: DistributionMethod) {
    service
        .add_vote(GUILD_ID, 1003, method)
        .expect("first vote succeeds");
    service
        .add_vote(GUILD_ID, 1004, method)
        .expect("second vote succeeds");
}

#[test]
fn test_even_distribution_basic() {
    let split = calculate_even_distribution(
        200,
        &[
            Debtor {
                discord_id: 1001,
                balance: -100,
            },
            Debtor {
                discord_id: 1002,
                balance: -100,
            },
        ],
    );
    assert_eq!(amounts(&split), BTreeMap::from([(1001, 100), (1002, 100)]));

    let capped = calculate_even_distribution(
        200,
        &[
            Debtor {
                discord_id: 1001,
                balance: -10,
            },
            Debtor {
                discord_id: 1002,
                balance: -500,
            },
        ],
    );
    assert_eq!(amounts(&capped), BTreeMap::from([(1001, 10), (1002, 190)]));

    let debt_limited = calculate_even_distribution(
        100,
        &[
            Debtor {
                discord_id: 1001,
                balance: -30,
            },
            Debtor {
                discord_id: 1002,
                balance: -20,
            },
        ],
    );
    assert_eq!(
        amounts(&debt_limited),
        BTreeMap::from([(1001, 30), (1002, 20)])
    );

    let small_debts: Vec<_> = (1..=10)
        .map(|discord_id| Debtor {
            discord_id,
            balance: -5,
        })
        .collect();
    let many = calculate_even_distribution(100, &small_debts);
    assert_eq!(many.iter().map(|entry| entry.amount).sum::<i64>(), 50);
    assert!(many.iter().all(|entry| entry.amount == 5));
}

#[test]
fn test_proportional_distribution_capped() {
    let proportional = calculate_proportional_distribution(
        100,
        &[
            Debtor {
                discord_id: 1001,
                balance: -300,
            },
            Debtor {
                discord_id: 1002,
                balance: -200,
            },
        ],
    );
    let proportional_amounts = amounts(&proportional);
    assert!(proportional_amounts[&1001] >= 55);
    assert!(proportional_amounts[&1002] >= 35);
    assert_eq!(proportional_amounts.values().sum::<i64>(), 100);

    let capped = calculate_proportional_distribution(
        100,
        &[
            Debtor {
                discord_id: 1001,
                balance: -10,
            },
            Debtor {
                discord_id: 1002,
                balance: -1000,
            },
        ],
    );
    let capped_amounts = amounts(&capped);
    assert_eq!(capped_amounts.values().sum::<i64>(), 100);
    assert_eq!(capped_amounts[&1001], 1);
    assert_eq!(capped_amounts[&1002], 99);
    assert!(capped_amounts[&1001] <= 10);
}

#[test]
fn test_neediest_distribution_basic() {
    let distributions = calculate_neediest_distribution(
        200,
        &[
            Debtor {
                discord_id: 1001,
                balance: -100,
            },
            Debtor {
                discord_id: 1002,
                balance: -500,
            },
            Debtor {
                discord_id: 1003,
                balance: -50,
            },
        ],
    );
    assert_eq!(distributions, vec![Distribution::new(1002, 200)]);
    assert_eq!(
        calculate_neediest_distribution(
            200,
            &[Debtor {
                discord_id: 1001,
                balance: -50,
            }]
        ),
        vec![Distribution::new(1001, 50)]
    );
}

#[test]
fn test_can_propose_insufficient_fund() {
    let (repository, service) = fixture();
    setup_players(&repository);
    let check = service.can_propose(GUILD_ID);
    assert!(!check.is_allowed());
    assert_eq!(check.reason(), "insufficient_fund:0:100");
}

#[test]
fn test_can_propose_success() {
    let (_repository, service) = standard_fixture();
    let check = service.can_propose(GUILD_ID);
    assert!(check.is_allowed());
    assert_eq!(check.reason(), "");
}

#[test]
fn test_create_proposal() {
    let (repository, service) = standard_fixture();
    let proposal = service
        .create_proposal(GUILD_ID)
        .expect("eligible proposal is created");
    assert_eq!(proposal.fund_amount, 300);
    assert_eq!(proposal.status, ProposalStatus::Active);
    assert!(proposal.quorum_required >= 1);
    assert_eq!(proposal.votes, empty_vote_counts());
    assert_eq!(repository.fund(GUILD_ID), 0);
}

#[test]
fn test_cannot_create_duplicate_proposal() {
    let (repository, service) = standard_fixture();
    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let cloned_service = service.clone();
            let cloned_barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                cloned_barrier.wait();
                cloned_service.create_proposal(GUILD_ID)
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("proposal worker does not panic"))
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    assert_eq!(repository.fund(GUILD_ID), 0);
    assert_eq!(
        service.can_propose(GUILD_ID),
        ProposalEligibility::Blocked(ProposalBlock::ActiveProposalExists)
    );
}

#[test]
fn test_add_vote() {
    let (_repository, service) = standard_fixture();
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    let first = service
        .add_vote(GUILD_ID, 1003, DistributionMethod::Even)
        .expect("vote succeeds");
    assert_eq!(first.vote_count(DistributionMethod::Even), 1);
    assert_eq!(first.total_votes, 1);
    assert!(!first.quorum_reached);

    let changed = service
        .add_vote(GUILD_ID, 1003, DistributionMethod::Proportional)
        .expect("vote change succeeds");
    assert_eq!(changed.vote_count(DistributionMethod::Proportional), 1);
    assert_eq!(changed.vote_count(DistributionMethod::Even), 0);
    assert_eq!(changed.total_votes, 1);
}

#[test]
fn test_quorum_reached() {
    let (_repository, service) = standard_fixture();
    let proposal = service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    assert_eq!(proposal.quorum_required, 2);
    service
        .add_vote(GUILD_ID, 1003, DistributionMethod::Even)
        .expect("first vote succeeds");
    let second = service
        .add_vote(GUILD_ID, 1004, DistributionMethod::Even)
        .expect("second vote succeeds");
    assert!(second.quorum_reached);
}

#[test]
fn test_tie_breaker_even_wins() {
    let (_repository, service) = standard_fixture();
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    service
        .add_vote(GUILD_ID, 1003, DistributionMethod::Even)
        .expect("first vote succeeds");
    service
        .add_vote(GUILD_ID, 1004, DistributionMethod::Proportional)
        .expect("second vote succeeds");
    assert_eq!(
        service.check_quorum(GUILD_ID),
        (true, Some(DistributionMethod::Even))
    );
}

#[test]
fn test_execute_disbursement() {
    let (repository, service) = standard_fixture();
    let before_1001 = balance(&repository, 1001);
    let before_1002 = balance(&repository, 1002);
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    vote_twice(&service, DistributionMethod::Even);
    let result = service
        .execute_disbursement(GUILD_ID)
        .expect("quorate proposal executes");
    assert!(result.success);
    assert_eq!(result.method, DistributionMethod::Even);
    assert!(result.total_disbursed > 0);
    assert_eq!(result.recipient_count, 2);
    assert!(balance(&repository, 1001) > -100);
    assert!(balance(&repository, 1002) > -50);
    let delta_sum =
        balance(&repository, 1001) - before_1001 + balance(&repository, 1002) - before_1002;
    assert_eq!(delta_sum, result.total_disbursed);
    assert_eq!(
        result
            .distributions
            .iter()
            .map(|entry| entry.amount)
            .sum::<i64>(),
        result.total_disbursed
    );
}

#[test]
fn test_reserve_allocation_methods_consume_locked_fund_burn() {
    let (repository, service) = standard_fixture();
    let proposal = service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    vote_twice(&service, DistributionMethod::Burn);
    let result = service
        .execute_disbursement(GUILD_ID)
        .expect("burn executes");
    assert_eq!(result.method, DistributionMethod::Burn);
    assert_eq!(result.total_disbursed, proposal.fund_amount);
    assert_eq!(result.recipient_count, 0);
    assert_eq!(repository.fund(GUILD_ID), 0);
    assert_eq!(repository.next_match_pot(GUILD_ID), 0);
}

#[test]
fn test_reserve_allocation_methods_consume_locked_fund_next_match_pot() {
    let (repository, service) = standard_fixture();
    let proposal = service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    vote_twice(&service, DistributionMethod::NextMatchPot);
    let result = service
        .execute_disbursement(GUILD_ID)
        .expect("next-match allocation executes");
    assert_eq!(result.method, DistributionMethod::NextMatchPot);
    assert_eq!(result.total_disbursed, proposal.fund_amount);
    assert_eq!(result.recipient_count, 0);
    assert_eq!(repository.fund(GUILD_ID), 0);
    assert_eq!(repository.next_match_pot(GUILD_ID), proposal.fund_amount);
}

#[test]
fn test_can_propose_without_player_recipients_for_burn_or_next_pot() {
    let (repository, service) = fixture();
    repository
        .add_to_fund(GUILD_ID, 100)
        .expect("funding succeeds");
    assert_eq!(service.can_propose(GUILD_ID), ProposalEligibility::Allowed);
    let proposal = service
        .create_proposal(GUILD_ID)
        .expect("recipient-free reserve proposal is valid");
    assert_eq!(proposal.quorum_required, 1);
}

#[test]
fn test_execute_disbursement_aborts_on_missing_recipient() {
    let (repository, service) = fixture();
    repository.add_player(GUILD_ID, 2001, "Real");
    repository
        .set_balance(GUILD_ID, 2001, -100)
        .expect("real recipient exists");
    repository
        .add_to_fund(GUILD_ID, 300)
        .expect("funding succeeds");
    service
        .create_proposal(GUILD_ID)
        .expect("proposal reserves the fund");
    let fund_before = repository.fund(GUILD_ID);
    let balance_before = balance(&repository, 2001);
    let error = repository
        .complete_and_disburse_atomic(
            GUILD_ID,
            300,
            &[
                Distribution::new(2001, 150),
                Distribution::new(424_242, 150),
            ],
            DistributionMethod::Even,
        )
        .expect_err("missing recipient aborts the transaction");
    assert!(error.to_string().contains("recipient row is missing"));
    assert_eq!(balance(&repository, 2001), balance_before);
    assert_eq!(repository.fund(GUILD_ID), fund_before);
    assert!(repository.active_proposal(GUILD_ID).is_some());
    assert!(repository.last_disbursement(GUILD_ID).is_none());
}

#[test]
fn test_execute_proportional_conserves_recipient_total() {
    let (repository, service) = standard_fixture();
    let before = BTreeMap::from([
        (1001, balance(&repository, 1001)),
        (1002, balance(&repository, 1002)),
    ]);
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    vote_twice(&service, DistributionMethod::Proportional);
    let result = service
        .execute_disbursement(GUILD_ID)
        .expect("proposal executes");
    let delta_sum: i64 = before
        .iter()
        .map(|(discord_id, old_balance)| balance(&repository, *discord_id) - old_balance)
        .sum();
    assert_eq!(result.method, DistributionMethod::Proportional);
    assert_eq!(delta_sum, result.total_disbursed);
    assert_eq!(
        result
            .distributions
            .iter()
            .map(|entry| entry.amount)
            .sum::<i64>(),
        result.total_disbursed
    );
}

#[test]
fn test_execute_neediest_conserves_recipient_total() {
    let (repository, service) = standard_fixture();
    let before = balance(&repository, 1001);
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    vote_twice(&service, DistributionMethod::Neediest);
    let result = service
        .execute_disbursement(GUILD_ID)
        .expect("proposal executes");
    assert_eq!(result.method, DistributionMethod::Neediest);
    assert_eq!(result.recipient_count, 1);
    assert_eq!(result.distributions[0].discord_id, 1001);
    assert_eq!(
        balance(&repository, 1001) - before,
        result.distributions[0].amount
    );
    assert_eq!(result.distributions[0].amount, result.total_disbursed);
}

#[test]
fn test_reset_proposal() {
    let (repository, service) = standard_fixture();
    let proposal = service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    service
        .add_vote(GUILD_ID, 1003, DistributionMethod::Even)
        .expect("vote succeeds");
    assert_eq!(service.reset_proposal(GUILD_ID), Ok(true));
    let ballots = repository.archived_ballots(GUILD_ID, proposal.proposal_id);
    assert_eq!(ballots.len(), 1);
    assert_eq!(ballots[0].discord_id, 1003);
    assert_eq!(ballots[0].method, DistributionMethod::Even);
    assert_eq!(ballots[0].proposal_outcome, "reset");
    assert!(ballots[0].finalized_at >= ballots[0].voted_at);
    assert_eq!(service.can_propose(GUILD_ID), ProposalEligibility::Allowed);
}

#[test]
fn test_reset_no_proposal() {
    let (_repository, service) = fixture();
    assert_eq!(service.reset_proposal(GUILD_ID), Ok(false));
}

#[test]
fn test_disabled_mode_blocks_all_governance_mutations() {
    let (repository, service) = standard_fixture();
    let proposal = service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    service
        .add_vote(GUILD_ID, 1003, DistributionMethod::Even)
        .expect("vote succeeds");
    let disabled = DisburseService::new(repository, 100, 0.40).with_voting_enabled(false);
    assert_eq!(
        disabled.can_propose(GUILD_ID),
        ProposalEligibility::Blocked(ProposalBlock::MonetaryRecovery)
    );
    for error in [
        disabled
            .create_proposal(GUILD_ID)
            .expect_err("create is blocked"),
        disabled
            .add_vote(GUILD_ID, 1004, DistributionMethod::Even)
            .expect_err("vote is blocked"),
        disabled
            .execute_disbursement(GUILD_ID)
            .expect_err("execute is blocked"),
        disabled
            .force_execute(GUILD_ID)
            .expect_err("force execute is blocked"),
    ] {
        assert!(error.to_string().contains("monetary recovery mode"));
    }
    assert_eq!(
        disabled
            .get_proposal(GUILD_ID)
            .expect("read-only proposal remains visible")
            .proposal_id,
        proposal.proposal_id
    );
}

#[test]
fn test_enforcement_is_atomic_auditable_and_idempotent() {
    let (repository, service) = standard_fixture();
    let proposal = service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    service
        .add_vote(GUILD_ID, 1003, DistributionMethod::Even)
        .expect("first vote succeeds");
    service
        .add_vote(GUILD_ID, 1004, DistributionMethod::Stimulus)
        .expect("second vote succeeds");
    assert_eq!(repository.fund(GUILD_ID), 0);

    let disabled = DisburseService::new(repository.clone(), 100, 0.40).with_voting_enabled(false);
    assert_eq!(
        disabled.enforce_voting_moratorium(GUILD_ID),
        Ok(MoratoriumResult {
            cancelled: true,
            proposal_id: Some(proposal.proposal_id),
            fund_amount_returned: 300,
        })
    );
    assert!(disabled.get_proposal(GUILD_ID).is_none());
    assert_eq!(repository.fund(GUILD_ID), 300);
    let ballots = repository.archived_ballots(GUILD_ID, proposal.proposal_id);
    assert_eq!(
        ballots
            .iter()
            .map(|ballot| ballot.discord_id)
            .collect::<Vec<_>>(),
        vec![1003, 1004]
    );
    assert!(
        ballots
            .iter()
            .all(|ballot| ballot.proposal_outcome == MONETARY_RECOVERY_CODE)
    );
    assert_eq!(
        repository.active_vote_count(GUILD_ID, proposal.proposal_id),
        0
    );
    let entries = repository.ledger_entries(GUILD_ID, proposal.proposal_id);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].delta, 300);
    assert_eq!(entries[0].source, "disburse");
    assert!(entries[0].reason.contains("monetary recovery"));
    assert_eq!(
        entries[0].metadata["proposal_outcome"],
        MONETARY_RECOVERY_CODE
    );

    assert_eq!(
        disabled.enforce_voting_moratorium(GUILD_ID),
        Ok(MoratoriumResult {
            cancelled: false,
            proposal_id: None,
            fund_amount_returned: 0,
        })
    );
    assert_eq!(repository.fund(GUILD_ID), 300);
    assert_eq!(
        repository
            .ledger_entries(GUILD_ID, proposal.proposal_id)
            .len(),
        1
    );
}

#[test]
fn test_active_edict_blocks_all_governance_mutations() {
    let (repository, service) = standard_fixture();
    let proposal = service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    service
        .add_vote(GUILD_ID, 1003, DistributionMethod::Even)
        .expect("vote succeeds");
    let restricted = DisburseService::new(repository, 100, 0.40).with_edict_provider(|_| {
        Some(EconomyEdict {
            event_id: 7,
            name: "Ravage".to_owned(),
            severity: 3,
        })
    });
    assert_eq!(
        restricted.can_propose(GUILD_ID),
        ProposalEligibility::Blocked(ProposalBlock::EconomyEdict)
    );
    for error in [
        restricted
            .create_proposal(GUILD_ID)
            .expect_err("create is blocked"),
        restricted
            .add_vote(GUILD_ID, 1004, DistributionMethod::Even)
            .expect_err("vote is blocked"),
        restricted
            .execute_disbursement(GUILD_ID)
            .expect_err("execute is blocked"),
        restricted
            .force_execute(GUILD_ID)
            .expect_err("force execute is blocked"),
    ] {
        assert!(error.to_string().contains("Ravage — Level III"));
    }
    assert_eq!(
        restricted
            .get_proposal(GUILD_ID)
            .expect("read-only proposal remains visible")
            .proposal_id,
        proposal.proposal_id
    );
}

#[test]
fn test_get_last_disbursement() {
    let (_repository, service) = standard_fixture();
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    vote_twice(&service, DistributionMethod::Even);
    service
        .execute_disbursement(GUILD_ID)
        .expect("proposal executes");
    let last = service
        .get_last_disbursement(GUILD_ID)
        .expect("history exists");
    assert_eq!(last.method, DistributionMethod::Even);
    assert!(last.total_amount > 0);
    assert_eq!(last.recipient_count(), 2);
    assert_eq!(last.recipients.len(), 2);
    assert_ne!(
        service.can_propose(GUILD_ID).reason(),
        "active_proposal_exists"
    );
}

#[test]
fn test_no_history() {
    let (_repository, service) = fixture();
    assert!(service.get_last_disbursement(GUILD_ID).is_none());
}

#[test]
fn test_resolved_proposal_archives_each_voters_latest_ballot() {
    let (repository, service) = standard_fixture();
    let proposal = service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    service
        .add_vote(GUILD_ID, 1003, DistributionMethod::Even)
        .expect("initial vote succeeds");
    service
        .add_vote(GUILD_ID, 1003, DistributionMethod::Proportional)
        .expect("replacement vote succeeds");
    service
        .add_vote(GUILD_ID, 1004, DistributionMethod::Proportional)
        .expect("second voter succeeds");
    let before = BTreeMap::from([
        (1001, balance(&repository, 1001)),
        (1002, balance(&repository, 1002)),
    ]);
    let result = service
        .execute_disbursement(GUILD_ID)
        .expect("proposal executes");
    assert_eq!(result.method, DistributionMethod::Proportional);
    assert_eq!(
        before
            .iter()
            .map(|(discord_id, old)| balance(&repository, *discord_id) - old)
            .sum::<i64>(),
        result.total_disbursed
    );
    let ballots = repository.archived_ballots(GUILD_ID, proposal.proposal_id);
    assert_eq!(
        ballots
            .iter()
            .map(|ballot| ballot.discord_id)
            .collect::<Vec<_>>(),
        vec![1003, 1004]
    );
    assert!(
        ballots
            .iter()
            .all(|ballot| ballot.method == DistributionMethod::Proportional)
    );
    assert!(
        ballots
            .iter()
            .all(|ballot| ballot.proposal_outcome == "proportional")
    );
    assert_eq!(
        ballots
            .iter()
            .map(|ballot| ballot.finalized_at)
            .collect::<BTreeSet<_>>()
            .len(),
        1
    );
}

#[test]
fn test_stimulus_distribution_basic() {
    let four = [
        StimulusCandidate {
            discord_id: 1001,
            balance: 50,
        },
        StimulusCandidate {
            discord_id: 1002,
            balance: 40,
        },
        StimulusCandidate {
            discord_id: 1003,
            balance: 30,
        },
        StimulusCandidate {
            discord_id: 1004,
            balance: 20,
        },
    ];
    let distributions = calculate_stimulus_distribution(100, &four);
    assert_eq!(
        distributions.iter().map(|entry| entry.amount).sum::<i64>(),
        100
    );
    assert!(distributions.iter().all(|entry| entry.amount == 25));

    let three = &four[..3];
    assert_eq!(
        amounts(&calculate_stimulus_distribution(100, three)),
        BTreeMap::from([(1001, 34), (1002, 33), (1003, 33)])
    );
    assert!(calculate_stimulus_distribution(100, &[]).is_empty());
    assert_eq!(
        calculate_stimulus_distribution(100, &four[..1]),
        vec![Distribution::new(1001, 100)]
    );
}

#[test]
fn test_get_all_registered_players_for_lottery() {
    let (repository, _service) = fixture();
    repository.set_activity_now(2_000_000_000);
    for (discord_id, name) in [
        (1, "ActiveNow"),
        (2, "Recent"),
        (3, "Inactive"),
        (4, "NeverPlayed"),
    ] {
        repository.add_player(GUILD_ID, discord_id, name);
    }
    repository
        .set_last_match_at(GUILD_ID, 1, 2_000_000_000)
        .expect("player exists");
    repository
        .set_last_match_at(GUILD_ID, 2, 2_000_000_000 - 10 * 86_400)
        .expect("player exists");
    repository
        .set_last_match_at(GUILD_ID, 3, 2_000_000_000 - 30 * 86_400)
        .expect("player exists");
    assert_eq!(
        repository
            .lottery_eligible_players(GUILD_ID, 14)
            .into_iter()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([1, 2])
    );
}

#[test]
fn test_get_all_registered_players_for_lottery_includes_debtors() {
    let (repository, _service) = fixture();
    repository.set_activity_now(2_000_000_000);
    repository.add_player(GUILD_ID, 1, "Rich");
    repository.add_player(GUILD_ID, 2, "Debtor");
    repository
        .set_balance(GUILD_ID, 1, 100)
        .expect("player exists");
    repository
        .set_balance(GUILD_ID, 2, -100)
        .expect("player exists");
    for discord_id in [1, 2] {
        repository
            .set_last_match_at(GUILD_ID, discord_id, 2_000_000_000)
            .expect("player exists");
    }
    assert_eq!(
        repository
            .lottery_eligible_players(GUILD_ID, 14)
            .into_iter()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([1, 2])
    );
}

#[test]
fn test_get_players_by_games_played_basic() {
    let (repository, _service) = fixture();
    for discord_id in 101..104 {
        repository.add_player(GUILD_ID, discord_id, format!("Rich{discord_id}"));
        repository
            .set_balance(GUILD_ID, discord_id, 1000)
            .expect("player exists");
    }
    for discord_id in 1..4 {
        repository.add_player(GUILD_ID, discord_id, format!("Player{discord_id}"));
    }
    repository
        .set_games(GUILD_ID, 1, 10, 5)
        .expect("player exists");
    repository
        .set_games(GUILD_ID, 2, 20, 10)
        .expect("player exists");
    repository
        .set_games(GUILD_ID, 3, 5, 5)
        .expect("player exists");
    let players = repository.players_by_games_played(GUILD_ID);
    assert_eq!(players.len(), 3);
    assert_eq!(players[0].discord_id, 2);
    assert_eq!(players[1].discord_id, 1);
    assert_eq!(players[2].discord_id, 3);
    assert_eq!(players[0].games_played, 30);
}

#[test]
fn test_get_players_by_games_played_excludes_zero_games() {
    let (repository, _service) = fixture();
    for discord_id in 101..104 {
        repository.add_player(GUILD_ID, discord_id, format!("Rich{discord_id}"));
        repository
            .set_balance(GUILD_ID, discord_id, 1000)
            .expect("player exists");
    }
    repository.add_player(GUILD_ID, 1, "Veteran");
    repository.add_player(GUILD_ID, 2, "Newbie");
    repository
        .set_games(GUILD_ID, 1, 10, 5)
        .expect("player exists");
    let players = repository.players_by_games_played(GUILD_ID);
    assert_eq!(players.len(), 1);
    assert_eq!(players[0].discord_id, 1);
}

#[test]
fn test_get_stimulus_eligible_excludes_top_3() {
    let (repository, _service) = fixture();
    for discord_id in 1..=6 {
        repository.add_player(GUILD_ID, discord_id, format!("Player{discord_id}"));
        repository
            .increment_wins(GUILD_ID, discord_id)
            .expect("player exists");
    }
    for (discord_id, player_balance) in [(1, 1000), (2, 500), (3, 200), (4, 50), (5, 10), (6, 0)] {
        repository
            .set_balance(GUILD_ID, discord_id, player_balance)
            .expect("player exists");
    }
    assert_eq!(
        repository
            .stimulus_eligible_players(GUILD_ID)
            .iter()
            .map(|player| player.discord_id)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([4, 5, 6])
    );
}

#[test]
fn test_get_stimulus_eligible_excludes_debtors() {
    let (repository, _service) = fixture();
    for (discord_id, name, player_balance) in [
        (1, "Rich", 100),
        (2, "Mid", 50),
        (3, "Zero", 0),
        (4, "Debtor", -50),
    ] {
        repository.add_player(GUILD_ID, discord_id, name);
        repository
            .increment_wins(GUILD_ID, discord_id)
            .expect("player exists");
        repository
            .set_balance(GUILD_ID, discord_id, player_balance)
            .expect("player exists");
    }
    let eligible = repository.stimulus_eligible_players(GUILD_ID);
    assert!(!eligible.iter().any(|player| player.discord_id == 4));
    assert!(eligible.is_empty());
}

#[test]
fn test_get_stimulus_eligible_fewer_than_4_players() {
    let (repository, _service) = fixture();
    for (discord_id, player_balance) in [(1, 100), (2, 50), (3, 10)] {
        repository.add_player(GUILD_ID, discord_id, format!("Player{discord_id}"));
        repository
            .increment_wins(GUILD_ID, discord_id)
            .expect("player exists");
        repository
            .set_balance(GUILD_ID, discord_id, player_balance)
            .expect("player exists");
    }
    assert!(repository.stimulus_eligible_players(GUILD_ID).is_empty());
}

#[test]
fn test_can_propose_with_only_stimulus_eligible() {
    let (repository, service) = fixture();
    for discord_id in 1..=5 {
        repository.add_player(GUILD_ID, discord_id, format!("Player{discord_id}"));
        repository
            .increment_wins(GUILD_ID, discord_id)
            .expect("player exists");
        repository
            .set_balance(GUILD_ID, discord_id, 100 - discord_id * 10)
            .expect("player exists");
    }
    repository
        .add_to_fund(GUILD_ID, 300)
        .expect("funding succeeds");
    assert_eq!(service.can_propose(GUILD_ID), ProposalEligibility::Allowed);
    assert_eq!(repository.stimulus_eligible_players(GUILD_ID).len(), 2);
}

#[test]
fn test_can_propose_no_eligible_recipients() {
    let (repository, service) = fixture();
    for discord_id in 1..=3 {
        repository.add_player(GUILD_ID, discord_id, format!("Player{discord_id}"));
        repository
            .increment_wins(GUILD_ID, discord_id)
            .expect("player exists");
        repository
            .set_balance(GUILD_ID, discord_id, 100)
            .expect("player exists");
    }
    repository
        .add_to_fund(GUILD_ID, 300)
        .expect("funding succeeds");
    assert!(repository.stimulus_eligible_players(GUILD_ID).is_empty());
    assert_eq!(service.can_propose(GUILD_ID), ProposalEligibility::Allowed);
}

#[test]
fn test_execute_stimulus_disbursement() {
    let (repository, service) = fixture();
    for discord_id in 1..=6 {
        repository.add_player(GUILD_ID, discord_id, format!("Player{discord_id}"));
        repository
            .increment_wins(GUILD_ID, discord_id)
            .expect("player exists");
    }
    for (discord_id, player_balance) in [(1, 500), (2, 300), (3, 100), (4, 50), (5, 30), (6, 10)] {
        repository
            .set_balance(GUILD_ID, discord_id, player_balance)
            .expect("player exists");
    }
    repository
        .add_to_fund(GUILD_ID, 300)
        .expect("funding succeeds");
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    for voter in [1, 2, 3] {
        service
            .add_vote(GUILD_ID, voter, DistributionMethod::Stimulus)
            .expect("vote succeeds");
    }
    let result = service
        .execute_disbursement(GUILD_ID)
        .expect("stimulus executes");
    assert!(result.success);
    assert_eq!(result.method, DistributionMethod::Stimulus);
    assert_eq!(result.recipient_count, 3);
    assert_eq!(result.total_disbursed, 300);
    assert_eq!(balance(&repository, 4), 150);
    assert_eq!(balance(&repository, 5), 130);
    assert_eq!(balance(&repository, 6), 110);
    assert_eq!(balance(&repository, 1), 500);
    assert_eq!(balance(&repository, 2), 300);
    assert_eq!(balance(&repository, 3), 100);
}

#[test]
fn test_lottery_distribution_basic() {
    let players = [1001, 1002, 1003];
    let distributions = calculate_lottery_distribution(100, &players, 4);
    assert_eq!(distributions.len(), 1);
    assert_eq!(distributions[0].amount, 100);
    assert!(players.contains(&distributions[0].discord_id));
    assert_eq!(
        calculate_lottery_distribution(100, &[1001], 999),
        vec![Distribution::new(1001, 100)]
    );
    assert!(calculate_lottery_distribution(100, &[], 0).is_empty());
}

#[test]
fn test_social_security_distribution_basic() {
    let players = [
        GamesCandidate {
            discord_id: 1001,
            games_played: 30,
        },
        GamesCandidate {
            discord_id: 1002,
            games_played: 50,
        },
        GamesCandidate {
            discord_id: 1003,
            games_played: 20,
        },
    ];
    let distributions = calculate_social_security_distribution(100, &players);
    let by_player = amounts(&distributions);
    assert_eq!(by_player.values().sum::<i64>(), 100);
    assert!(by_player[&1002] >= by_player[&1001]);
    assert!(by_player[&1002] >= by_player[&1003]);
    assert_eq!(
        calculate_social_security_distribution(100, &players[..1]),
        vec![Distribution::new(1001, 100)]
    );
    let equal = [
        GamesCandidate {
            discord_id: 1001,
            games_played: 10,
        },
        GamesCandidate {
            discord_id: 1002,
            games_played: 10,
        },
    ];
    assert_eq!(
        calculate_social_security_distribution(100, &equal)
            .iter()
            .map(|entry| entry.amount)
            .sum::<i64>(),
        100
    );
    assert!(calculate_social_security_distribution(100, &[]).is_empty());
}

#[test]
fn test_social_security_distribution_zero_games() {
    let players = [
        GamesCandidate {
            discord_id: 1001,
            games_played: 0,
        },
        GamesCandidate {
            discord_id: 1002,
            games_played: 0,
        },
    ];
    assert!(calculate_social_security_distribution(100, &players).is_empty());
}

#[test]
fn test_richest_distribution_basic() {
    assert_eq!(
        calculate_richest_distribution(
            100,
            Some(RichestCandidate {
                discord_id: 1001,
                balance: 500,
            })
        ),
        vec![Distribution::new(1001, 100)]
    );
    assert_eq!(
        calculate_richest_distribution(
            500,
            Some(RichestCandidate {
                discord_id: 1001,
                balance: 10,
            })
        ),
        vec![Distribution::new(1001, 500)]
    );
    assert!(calculate_richest_distribution(100, None).is_empty());
}

#[test]
fn test_get_richest_player_basic() {
    let (repository, _service) = fixture();
    assert!(repository.richest_player(GUILD_ID).is_none());
    repository.add_player(GUILD_ID, 1, "Poor");
    repository
        .set_balance(GUILD_ID, 1, 50)
        .expect("player exists");
    assert_eq!(
        repository.richest_player(GUILD_ID),
        Some(RichestCandidate {
            discord_id: 1,
            balance: 50,
        })
    );
    repository.add_player(GUILD_ID, 2, "Middle");
    repository.add_player(GUILD_ID, 3, "Rich");
    for (discord_id, player_balance) in [(1, -100), (2, 100), (3, 1000)] {
        repository
            .set_balance(GUILD_ID, discord_id, player_balance)
            .expect("player exists");
    }
    assert_eq!(
        repository.richest_player(GUILD_ID),
        Some(RichestCandidate {
            discord_id: 3,
            balance: 1000,
        })
    );
}

#[test]
fn test_get_richest_player_includes_debtors() {
    let (repository, _service) = fixture();
    repository.add_player(GUILD_ID, 1, "VeryPoor");
    repository.add_player(GUILD_ID, 2, "LessPoor");
    repository
        .set_balance(GUILD_ID, 1, -100)
        .expect("player exists");
    repository
        .set_balance(GUILD_ID, 2, -50)
        .expect("player exists");
    assert_eq!(
        repository.richest_player(GUILD_ID),
        Some(RichestCandidate {
            discord_id: 2,
            balance: -50,
        })
    );
}

#[test]
fn test_cancel_wins_resets_proposal() {
    let (repository, service) = standard_fixture();
    let proposal = service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    vote_twice(&service, DistributionMethod::Cancel);
    let result = service
        .execute_disbursement(GUILD_ID)
        .expect("cancel executes");
    assert!(result.success);
    assert_eq!(result.method, DistributionMethod::Cancel);
    assert!(result.cancelled);
    assert_eq!(result.total_disbursed, 0);
    assert!(
        result
            .message
            .as_deref()
            .is_some_and(|message| message.to_ascii_lowercase().contains("cancelled"))
    );
    let ballots = repository.archived_ballots(GUILD_ID, proposal.proposal_id);
    assert_eq!(
        ballots
            .iter()
            .map(|ballot| ballot.discord_id)
            .collect::<Vec<_>>(),
        vec![1003, 1004]
    );
    assert!(
        ballots
            .iter()
            .all(|ballot| ballot.method == DistributionMethod::Cancel)
    );
    assert!(
        ballots
            .iter()
            .all(|ballot| ballot.proposal_outcome == "cancelled")
    );
}

#[test]
fn test_cancel_tiebreaker_loses() {
    let (_repository, service) = standard_fixture();
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    service
        .add_vote(GUILD_ID, 1003, DistributionMethod::Even)
        .expect("first vote succeeds");
    service
        .add_vote(GUILD_ID, 1004, DistributionMethod::Cancel)
        .expect("second vote succeeds");
    assert_eq!(
        service.check_quorum(GUILD_ID),
        (true, Some(DistributionMethod::Even))
    );
}

#[test]
fn test_execute_lottery_disbursement() {
    let (repository, service) = fixture();
    repository.set_activity_now(2_000_000_000);
    for discord_id in 1..=5 {
        repository.add_player(GUILD_ID, discord_id, format!("Player{discord_id}"));
        repository
            .increment_wins(GUILD_ID, discord_id)
            .expect("player exists");
        repository
            .set_balance(GUILD_ID, discord_id, 50)
            .expect("player exists");
        repository
            .set_last_match_at(GUILD_ID, discord_id, 2_000_000_000)
            .expect("player exists");
    }
    repository
        .add_to_fund(GUILD_ID, 300)
        .expect("funding succeeds");
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    service
        .add_vote(GUILD_ID, 1, DistributionMethod::Lottery)
        .expect("first vote succeeds");
    service
        .add_vote(GUILD_ID, 2, DistributionMethod::Lottery)
        .expect("second vote succeeds");
    let result = service
        .execute_disbursement(GUILD_ID)
        .expect("lottery executes");
    assert!(result.success);
    assert_eq!(result.method, DistributionMethod::Lottery);
    assert_eq!(result.recipient_count, 1);
    assert_eq!(result.total_disbursed, 300);
    let balances: Vec<_> = (1..=5)
        .map(|discord_id| balance(&repository, discord_id))
        .collect();
    assert!(balances.contains(&350));
    assert_eq!(balances.iter().filter(|&&value| value == 50).count(), 4);
}

#[test]
fn test_execute_social_security_disbursement() {
    let (repository, service) = fixture();
    for discord_id in 101..104 {
        repository.add_player(GUILD_ID, discord_id, format!("Rich{discord_id}"));
        repository
            .increment_wins(GUILD_ID, discord_id)
            .expect("player exists");
        repository
            .set_balance(GUILD_ID, discord_id, 1000)
            .expect("player exists");
    }
    for discord_id in 1..=4 {
        repository.add_player(GUILD_ID, discord_id, format!("Player{discord_id}"));
        repository
            .set_balance(GUILD_ID, discord_id, 50)
            .expect("player exists");
    }
    for (discord_id, wins, losses) in [(1, 20, 10), (2, 10, 10), (3, 5, 5), (4, 0, 0)] {
        repository
            .set_games(GUILD_ID, discord_id, wins, losses)
            .expect("player exists");
    }
    repository
        .add_to_fund(GUILD_ID, 300)
        .expect("funding succeeds");
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    for voter in [1, 2, 3] {
        service
            .add_vote(GUILD_ID, voter, DistributionMethod::SocialSecurity)
            .expect("vote succeeds");
    }
    let result = service
        .execute_disbursement(GUILD_ID)
        .expect("social security executes");
    assert!(result.success);
    assert_eq!(result.method, DistributionMethod::SocialSecurity);
    assert_eq!(result.recipient_count, 3);
    assert_eq!(result.total_disbursed, 300);
    assert!(
        balance(&repository, 1) > balance(&repository, 2)
            && balance(&repository, 2) > balance(&repository, 3)
    );
    assert_eq!(balance(&repository, 4), 50);
}

#[test]
fn test_execute_richest_disbursement() {
    let (repository, service) = fixture();
    for (discord_id, name, player_balance) in [
        (1, "Poor", -50),
        (2, "Middle", 100),
        (3, "Rich", 500),
        (4, "Voter1", 50),
        (5, "Voter2", 50),
    ] {
        repository.add_player(GUILD_ID, discord_id, name);
        repository
            .set_balance(GUILD_ID, discord_id, player_balance)
            .expect("player exists");
    }
    repository
        .add_to_fund(GUILD_ID, 300)
        .expect("funding succeeds");
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    service
        .add_vote(GUILD_ID, 4, DistributionMethod::Richest)
        .expect("first vote succeeds");
    service
        .add_vote(GUILD_ID, 5, DistributionMethod::Richest)
        .expect("second vote succeeds");
    let result = service
        .execute_disbursement(GUILD_ID)
        .expect("richest executes");
    assert!(result.success);
    assert_eq!(result.method, DistributionMethod::Richest);
    assert_eq!(result.recipient_count, 1);
    assert_eq!(result.total_disbursed, 300);
    assert_eq!(balance(&repository, 3), 800);
    assert_eq!(balance(&repository, 1), -50);
    assert_eq!(balance(&repository, 2), 100);
    assert_eq!(balance(&repository, 4), 50);
    assert_eq!(balance(&repository, 5), 50);
}

#[test]
fn test_richest_even_with_debtors() {
    let (repository, service) = fixture();
    for (discord_id, name, player_balance) in [
        (1, "VeryPoor", -100),
        (2, "LessPoor", -50),
        (3, "Voter", -75),
    ] {
        repository.add_player(GUILD_ID, discord_id, name);
        repository
            .set_balance(GUILD_ID, discord_id, player_balance)
            .expect("player exists");
    }
    repository
        .add_to_fund(GUILD_ID, 200)
        .expect("funding succeeds");
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    service
        .add_vote(GUILD_ID, 1, DistributionMethod::Richest)
        .expect("first vote succeeds");
    service
        .add_vote(GUILD_ID, 3, DistributionMethod::Richest)
        .expect("second vote succeeds");
    let result = service
        .execute_disbursement(GUILD_ID)
        .expect("richest executes");
    assert!(result.success);
    assert_eq!(result.method, DistributionMethod::Richest);
    assert_eq!(result.recipient_count, 1);
    assert_eq!(balance(&repository, 2), 150);
}

#[test]
fn test_fund_returns_on_vote_cancel() {
    let (repository, service) = standard_fixture();
    let initial_fund = repository.fund(GUILD_ID);
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    vote_twice(&service, DistributionMethod::Cancel);
    service
        .execute_disbursement(GUILD_ID)
        .expect("cancel executes");
    assert_eq!(repository.fund(GUILD_ID), initial_fund);
}

#[test]
fn test_fund_correct_after_execution() {
    let (repository, service) = standard_fixture();
    let initial_fund = repository.fund(GUILD_ID);
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    vote_twice(&service, DistributionMethod::Even);
    let result = service
        .execute_disbursement(GUILD_ID)
        .expect("proposal executes");
    assert_eq!(
        repository.fund(GUILD_ID),
        initial_fund - result.total_disbursed
    );
}

#[test]
fn test_fund_returns_when_no_eligible() {
    let (repository, service) = fixture();
    for discord_id in 1..=6 {
        repository.add_player(GUILD_ID, discord_id, format!("Player{discord_id}"));
        repository
            .increment_wins(GUILD_ID, discord_id)
            .expect("player exists");
        repository
            .set_balance(GUILD_ID, discord_id, 100 - discord_id * 10)
            .expect("player exists");
    }
    repository
        .add_to_fund(GUILD_ID, 300)
        .expect("funding succeeds");
    let initial_fund = repository.fund(GUILD_ID);
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    for voter in [1, 2, 3] {
        service
            .add_vote(GUILD_ID, voter, DistributionMethod::Neediest)
            .expect("vote succeeds");
    }
    let result = service
        .execute_disbursement(GUILD_ID)
        .expect("no-op execution succeeds");
    assert_eq!(result.total_disbursed, 0);
    assert_eq!(result.recipient_count, 0);
    assert_eq!(repository.fund(GUILD_ID), initial_fund);
    assert!(service.get_proposal(GUILD_ID).is_none());
}

#[test]
fn test_fund_accrued_during_proposal_preserved() {
    let (repository, service) = standard_fixture();
    let initial_fund = repository.fund(GUILD_ID);
    assert_eq!(initial_fund, 300);
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    assert_eq!(repository.fund(GUILD_ID), 0);
    repository
        .add_to_fund(GUILD_ID, 50)
        .expect("new fees accrue");
    assert_eq!(repository.fund(GUILD_ID), 50);
    assert_eq!(service.reset_proposal(GUILD_ID), Ok(true));
    assert_eq!(repository.fund(GUILD_ID), initial_fund + 50);
}

#[test]
fn test_get_individual_votes_no_proposal() {
    let (repository, service) = fixture();
    assert!(repository.individual_votes(GUILD_ID).is_empty());
    assert!(service.get_individual_votes(GUILD_ID).is_empty());
}

#[test]
fn test_get_individual_votes_chronological() {
    let (repository, service) = standard_fixture();
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    assert!(repository.individual_votes(GUILD_ID).is_empty());
    for (timestamp, discord_id, method) in [
        (1_700_000_010, 1003, DistributionMethod::Even),
        (1_700_000_020, 1004, DistributionMethod::Proportional),
        (1_700_000_030, 1005, DistributionMethod::Neediest),
    ] {
        repository.set_clock(timestamp);
        service
            .add_vote(GUILD_ID, discord_id, method)
            .expect("vote succeeds");
    }
    let votes = repository.individual_votes(GUILD_ID);
    assert_eq!(
        votes.iter().map(|vote| vote.discord_id).collect::<Vec<_>>(),
        vec![1003, 1004, 1005]
    );
    assert_eq!(
        votes.iter().map(|vote| vote.method).collect::<Vec<_>>(),
        vec![
            DistributionMethod::Even,
            DistributionMethod::Proportional,
            DistributionMethod::Neediest,
        ]
    );
    assert_eq!(
        votes.iter().map(|vote| vote.voted_at).collect::<Vec<_>>(),
        vec![1_700_000_010, 1_700_000_020, 1_700_000_030]
    );
}

#[test]
fn test_get_individual_votes_vote_change() {
    let (repository, service) = standard_fixture();
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    service
        .add_vote(GUILD_ID, 1003, DistributionMethod::Even)
        .expect("initial vote succeeds");
    service
        .add_vote(GUILD_ID, 1003, DistributionMethod::Proportional)
        .expect("replacement vote succeeds");
    let votes = repository.individual_votes(GUILD_ID);
    assert_eq!(votes.len(), 1);
    assert_eq!(votes[0].discord_id, 1003);
    assert_eq!(votes[0].method, DistributionMethod::Proportional);
}

#[test]
fn test_force_execute_with_single_vote() {
    let (_repository, service) = standard_fixture();
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    service
        .add_vote(GUILD_ID, 1003, DistributionMethod::Even)
        .expect("vote succeeds");
    assert_eq!(service.check_quorum(GUILD_ID), (false, None));
    let result = service
        .force_execute(GUILD_ID)
        .expect("force execute bypasses quorum");
    assert!(result.success);
    assert_eq!(result.method, DistributionMethod::Even);
    assert!(result.total_disbursed > 0);
}

#[test]
fn test_force_execute_picks_leading_method() {
    let (repository, service) = standard_fixture();
    for suffix in 6..=10 {
        let discord_id = 1000 + suffix;
        repository.add_player(GUILD_ID, discord_id, format!("Voter{suffix}"));
        repository
            .set_balance(GUILD_ID, discord_id, 100)
            .expect("player exists");
    }
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    service
        .add_vote(GUILD_ID, 1003, DistributionMethod::Even)
        .expect("vote succeeds");
    service
        .add_vote(GUILD_ID, 1004, DistributionMethod::Proportional)
        .expect("vote succeeds");
    service
        .add_vote(GUILD_ID, 1005, DistributionMethod::Proportional)
        .expect("vote succeeds");
    let result = service
        .force_execute(GUILD_ID)
        .expect("force execute succeeds");
    assert_eq!(result.method, DistributionMethod::Proportional);
}

#[test]
fn test_force_execute_no_votes_raises() {
    let (_repository, service) = standard_fixture();
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    assert_eq!(
        service
            .force_execute(GUILD_ID)
            .expect_err("zero votes are rejected"),
        DisburseError::NoVotes
    );
    assert!(service.get_proposal(GUILD_ID).is_some());
}

#[test]
fn test_force_execute_no_proposal_raises() {
    let (_repository, service) = fixture();
    assert_eq!(
        service
            .force_execute(GUILD_ID)
            .expect_err("missing proposal is rejected"),
        DisburseError::NoActiveProposal
    );
}

#[test]
fn test_force_execute_cancel_returns_funds() {
    let (repository, service) = standard_fixture();
    let initial_fund = repository.fund(GUILD_ID);
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    assert_eq!(repository.fund(GUILD_ID), 0);
    service
        .add_vote(GUILD_ID, 1003, DistributionMethod::Cancel)
        .expect("vote succeeds");
    let result = service
        .force_execute(GUILD_ID)
        .expect("forced cancel succeeds");
    assert!(result.cancelled);
    assert_eq!(result.total_disbursed, 0);
    assert_eq!(repository.fund(GUILD_ID), initial_fund);
}

#[test]
fn test_force_execute_completes_proposal() {
    let (repository, service) = standard_fixture();
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    service
        .add_vote(GUILD_ID, 1003, DistributionMethod::Even)
        .expect("vote succeeds");

    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let cloned_service = service.clone();
            let cloned_barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                cloned_barrier.wait();
                cloned_service.force_execute(GUILD_ID)
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("execution worker does not panic"))
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == Err(DisburseError::NoActiveProposal))
            .count(),
        1
    );
    assert!(service.get_proposal(GUILD_ID).is_none());
    assert_eq!(balance(&repository, 1001), 0);
    assert_eq!(balance(&repository, 1002), 0);
}

#[test]
fn test_fix1_concurrent_execute_disbursement_does_not_double_refund() {
    let (repository, service) = standard_fixture();
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    vote_twice(&service, DistributionMethod::Even);
    let before = balance(&repository, 1001) + balance(&repository, 1002);

    let barrier = Arc::new(Barrier::new(3));
    let handles = (0..2)
        .map(|_| {
            let service = service.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                service.execute_disbursement(GUILD_ID)
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().expect("execution thread"))
        .collect::<Vec<_>>();
    let successes = results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .collect::<Vec<_>>();
    assert_eq!(successes.len(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == Err(DisburseError::NoActiveProposal))
            .count(),
        1
    );
    let credited = balance(&repository, 1001) + balance(&repository, 1002) - before;
    assert_eq!(credited, successes[0].total_disbursed);
    assert_eq!(repository.fund(GUILD_ID) + credited, 300);
}

#[test]
fn test_fix1_concurrent_execute_and_force_execute_single_winner() {
    let (repository, service) = standard_fixture();
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    vote_twice(&service, DistributionMethod::Neediest);
    let before = balance(&repository, 1001) + balance(&repository, 1002);
    let barrier = Arc::new(Barrier::new(3));
    let execute_service = service.clone();
    let execute_barrier = Arc::clone(&barrier);
    let execute = thread::spawn(move || {
        execute_barrier.wait();
        execute_service.execute_disbursement(GUILD_ID)
    });
    let force_service = service.clone();
    let force_barrier = Arc::clone(&barrier);
    let force = thread::spawn(move || {
        force_barrier.wait();
        force_service.force_execute(GUILD_ID)
    });
    barrier.wait();
    let results = [
        execute.join().expect("execute thread"),
        force.join().expect("force thread"),
    ];
    let successes = results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .collect::<Vec<_>>();
    assert_eq!(successes.len(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == Err(DisburseError::NoActiveProposal))
            .count(),
        1
    );
    let credited = balance(&repository, 1001) + balance(&repository, 1002) - before;
    assert_eq!(credited, successes[0].total_disbursed);
    assert_eq!(repository.fund(GUILD_ID) + credited, 300);
}

#[test]
fn test_fix2_quorum_recheck_inside_lock_rejects_late_cancel_after_execute() {
    let (repository, service) = standard_fixture();
    service
        .create_proposal(GUILD_ID)
        .expect("proposal is created");
    vote_twice(&service, DistributionMethod::Even);
    let before = balance(&repository, 1001) + balance(&repository, 1002);
    let result = service
        .execute_disbursement(GUILD_ID)
        .expect("first execution succeeds");
    assert_eq!(
        service
            .add_vote(GUILD_ID, 1005, DistributionMethod::Cancel)
            .expect_err("completed proposal rejects late vote"),
        DisburseError::NoActiveProposal
    );
    assert_eq!(
        service
            .execute_disbursement(GUILD_ID)
            .expect_err("completed proposal rejects second execution"),
        DisburseError::NoActiveProposal
    );
    let credited = balance(&repository, 1001) + balance(&repository, 1002) - before;
    assert_eq!(credited, result.total_disbursed);
    assert_eq!(repository.fund(GUILD_ID) + credited, 300);
}

/// Builds a roster snapshot; only the fields the selection helpers read
/// matter.
fn snapshot(discord_id: DiscordId, balance: i64, games: u32) -> PlayerSnapshot {
    PlayerSnapshot {
        discord_id,
        username: format!("player{discord_id}"),
        balance,
        wins: games,
        losses: 0,
        last_match_at: None,
    }
}

#[test]
fn test_distribution_method_from_code_round_trips() {
    for method in DistributionMethod::ALL {
        assert_eq!(DistributionMethod::from_code(method.as_str()), Some(method));
    }
    assert_eq!(DistributionMethod::from_code("unknown"), None);
    assert_eq!(DistributionMethod::from_code("EVEN"), None);
}

#[test]
fn test_select_debtors_orders_most_indebted_first() {
    let roster = [
        snapshot(1, -50, 1),
        snapshot(2, -500, 1),
        snapshot(3, 100, 1),
        snapshot(4, -50, 0),
    ];
    assert_eq!(
        select_debtors(&roster),
        vec![
            Debtor {
                discord_id: 2,
                balance: -500,
            },
            Debtor {
                discord_id: 1,
                balance: -50,
            },
            Debtor {
                discord_id: 4,
                balance: -50,
            },
        ]
    );
}

#[test]
fn test_method_distributions_debtor_methods_target_debtors() {
    let roster = [
        snapshot(1, -100, 5),
        snapshot(2, -300, 5),
        snapshot(3, 900, 5),
    ];
    assert_eq!(
        amounts(&calculate_method_distributions(
            DistributionMethod::Even,
            200,
            &roster,
            None
        )),
        BTreeMap::from([(1, 100), (2, 100)])
    );
    assert_eq!(
        amounts(&calculate_method_distributions(
            DistributionMethod::Proportional,
            100,
            &roster,
            None
        )),
        BTreeMap::from([(1, 25), (2, 75)])
    );
    assert_eq!(
        calculate_method_distributions(DistributionMethod::Neediest, 150, &roster, None),
        vec![Distribution::new(2, 150)]
    );
}

#[test]
fn test_method_distributions_even_remainder_goes_to_the_most_indebted() {
    // Remainder coins follow the (balance asc, discord id asc) debtor order,
    // deliberately superseding the legacy unspecified roster order.
    let roster = [snapshot(1, -5, 1), snapshot(2, -9, 1)];
    assert_eq!(
        calculate_method_distributions(DistributionMethod::Even, 1, &roster, None),
        vec![Distribution::new(2, 1)]
    );
}

#[test]
fn test_method_distributions_stimulus_skips_the_three_richest_players() {
    let roster = [
        snapshot(1, 900, 2),
        snapshot(2, 800, 2),
        snapshot(3, 700, 2),
        snapshot(4, 600, 2),
        snapshot(5, 500, 2),
        // Unplayed accounts never displace a real player from the top three.
        snapshot(6, 5000, 0),
    ];
    assert_eq!(
        amounts(&calculate_method_distributions(
            DistributionMethod::Stimulus,
            100,
            &roster,
            None
        )),
        BTreeMap::from([(4, 50), (5, 50)])
    );
}

#[test]
fn test_method_distributions_social_security_excludes_richest_non_debtors() {
    let roster = [
        snapshot(1, 900, 10),
        snapshot(2, 800, 10),
        snapshot(3, 700, 10),
        snapshot(4, 50, 6),
        snapshot(5, -500, 4),
    ];
    // Debtors are never part of the richest exclusion, so 4 and 5 split the
    // fund 6:4 by games played.
    assert_eq!(
        calculate_method_distributions(DistributionMethod::SocialSecurity, 100, &roster, None),
        vec![Distribution::new(4, 60), Distribution::new(5, 40)]
    );
}

#[test]
fn test_method_distributions_lottery_pays_only_the_drawn_winner() {
    let roster = [snapshot(1, 10, 1), snapshot(2, 20, 1)];
    assert_eq!(
        calculate_method_distributions(DistributionMethod::Lottery, 250, &roster, Some(2)),
        vec![Distribution::new(2, 250)]
    );
    assert!(
        calculate_method_distributions(DistributionMethod::Lottery, 250, &roster, None).is_empty()
    );
}

#[test]
fn test_method_distributions_richest_breaks_ties_on_the_lowest_id() {
    let roster = [snapshot(7, 250, 1), snapshot(3, 250, 1)];
    assert_eq!(
        calculate_method_distributions(DistributionMethod::Richest, 90, &roster, None),
        vec![Distribution::new(3, 90)]
    );
}

#[test]
fn test_method_distributions_non_paying_methods_and_empty_fund() {
    let roster = [snapshot(1, -100, 1)];
    for method in [
        DistributionMethod::Burn,
        DistributionMethod::NextMatchPot,
        DistributionMethod::Cancel,
    ] {
        assert!(calculate_method_distributions(method, 100, &roster, None).is_empty());
    }
    assert!(calculate_method_distributions(DistributionMethod::Even, 0, &roster, None).is_empty());
}

use super::*;

#[test]
fn test_protection_shield_block_conserves_jc() {
    let result = funded_protection(ProtectionSource::Shield, 20, 10);
    assert_eq!(result.actor_delta, -20);
    assert_eq!(result.target_delta, 10);
    assert!(result.net_delta() <= 0);
}

#[test]
fn test_manashop_ward_block_conserves_jc_on_every_call() {
    let mut actor = 200;
    let mut target = 0;
    for _ in 0..5 {
        let result = funded_protection(ProtectionSource::Ward, 20, 10);
        actor += result.actor_delta;
        target += result.target_delta;
        assert!(result.target_delta <= -result.actor_delta);
        assert!(result.net_delta() <= 0);
    }
    assert_eq!((actor, target), (100, 50));
}

#[test]
fn test_aegis_charge_block_conserves_jc() {
    let result = funded_protection(ProtectionSource::AegisCharge, 15, 100);
    assert_eq!(result.target_delta, 15);
    assert_eq!(result.net_delta(), 0);
}

#[test]
fn test_ward_block_is_audited_as_a_ward_block() {
    let ward = funded_protection(ProtectionSource::Ward, 20, 10);
    assert_eq!(ward.audit_action, "sabotage_ward_blocked");
    let shield = funded_protection(ProtectionSource::Shield, 20, 10);
    assert_eq!(shield.audit_action, "sabotage_blocked");
}

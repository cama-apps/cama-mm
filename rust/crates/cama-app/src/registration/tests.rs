use super::*;

#[test]
fn test_steam_id_at_uint32_ceiling_rejected() {
    assert_eq!(
        validate_steam_id(STEAM_ACCOUNT_ID_UPPER_BOUND),
        Err(SteamIdValidationError::OutOfRange)
    );
}

#[test]
fn test_register_with_invalid_steam_id() {
    for invalid in [-1, 0, STEAM_ACCOUNT_ID_UPPER_BOUND] {
        assert_eq!(
            validate_steam_id(invalid),
            Err(SteamIdValidationError::OutOfRange)
        );
    }
    assert_eq!(validate_steam_id(123_456_789), Ok(()));
    assert_eq!(validate_steam_id(2_147_483_648), Ok(()));
}

#[test]
fn test_duplicate_roles_deduplicated() {
    assert_eq!(parse_roles("1111111111"), Ok(vec!["1".to_owned()]));
}

#[test]
fn test_mixed_duplicates_preserve_order() {
    assert_eq!(
        parse_roles("54321123"),
        Ok(["5", "4", "3", "2", "1"].map(str::to_owned).to_vec())
    );
}

#[test]
fn test_comma_separated_with_duplicates() {
    assert_eq!(
        parse_roles("1, 2, 1, 3, 2"),
        Ok(["1", "2", "3"].map(str::to_owned).to_vec())
    );
}

#[test]
fn test_invalid_role_rejected() {
    assert_eq!(parse_roles("126"), Err(RoleInputError::Invalid('6')));
    assert!(
        RoleInputError::Invalid('6')
            .to_string()
            .contains("Invalid role: 6")
    );
}

#[test]
fn test_empty_roles_rejected() {
    assert_eq!(parse_roles(", "), Err(RoleInputError::Empty));
    assert!(
        RoleInputError::Empty
            .to_string()
            .contains("at least one role")
    );
}

//! Discord-independent registration input policy.

use thiserror::Error;

pub const STEAM_ACCOUNT_ID_UPPER_BOUND: i64 = 1_i64 << 32;

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum SteamIdValidationError {
    #[error("Invalid Steam ID. Must be Steam32 (positive, 32-bit).")]
    OutOfRange,
}

pub fn validate_steam_id(steam_id: i64) -> Result<(), SteamIdValidationError> {
    if steam_id <= 0 || steam_id >= STEAM_ACCOUNT_ID_UPPER_BOUND {
        Err(SteamIdValidationError::OutOfRange)
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RoleInputError {
    #[error("Invalid role: {0}. Roles must be 1-5.")]
    Invalid(char),
    #[error("Please provide at least one role.")]
    Empty,
}

pub fn parse_roles(input: &str) -> Result<Vec<String>, RoleInputError> {
    let mut seen = [false; 5];
    let mut roles = Vec::new();
    for role in input
        .chars()
        .filter(|character| *character != ',' && *character != ' ')
    {
        if !(('1'..='5').contains(&role)) {
            return Err(RoleInputError::Invalid(role));
        }
        let index = usize::from(role as u8 - b'1');
        if !seen[index] {
            seen[index] = true;
            roles.push(role.to_string());
        }
    }
    if roles.is_empty() {
        Err(RoleInputError::Empty)
    } else {
        Ok(roles)
    }
}

#[cfg(test)]
#[path = "registration/tests.rs"]
mod tests;

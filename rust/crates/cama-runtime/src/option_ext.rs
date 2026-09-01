//! Typed extraction helpers shared by the slash-command providers.
//!
//! Two families exist deliberately:
//!
//! - The `Option`-returning helpers ([`string_option`], [`integer_option`],
//!   [`boolean_option`]) treat an absent or wrong-typed value as `None`. The
//!   option types are schema-controlled, so a mismatch only occurs when the
//!   command schema and handler drift apart.
//! - The user helpers fail closed: a supplied user whose snowflake exceeds
//!   SQLite's signed INTEGER range is an error, never a silent `None`, per the
//!   guild-isolation contract. `Ok(None)` strictly means the option is absent.
//!
//! The `*_value` helpers apply the same typed conversions to an
//! already-located [`InteractionValue`], for providers with their own lookup
//! rules (for example `/dig`'s recursive subcommand search). They error on a
//! wrong-typed value and treat the transport's `Unknown` decode fallback as
//! absent.

use crate::registration::{InteractionOption, InteractionValue};

/// Find the raw value of a top-level option by name.
pub fn option_value<'a>(
    options: &'a [InteractionOption],
    name: &str,
) -> Option<&'a InteractionValue> {
    options
        .iter()
        .find(|option| option.name == name)
        .map(|option| &option.value)
}

/// Borrow a string option; absent or wrong-typed values are `None`.
pub fn string_option<'a>(options: &'a [InteractionOption], name: &str) -> Option<&'a str> {
    match option_value(options, name) {
        Some(InteractionValue::String(value)) => Some(value.as_str()),
        _ => None,
    }
}

/// Read an integer option; absent or wrong-typed values are `None`.
pub fn integer_option(options: &[InteractionOption], name: &str) -> Option<i64> {
    match option_value(options, name) {
        Some(InteractionValue::Integer(value)) => Some(*value),
        _ => None,
    }
}

/// Read a boolean option; absent or wrong-typed values are `None`.
pub fn boolean_option(options: &[InteractionOption], name: &str) -> Option<bool> {
    match option_value(options, name) {
        Some(InteractionValue::Boolean(value)) => Some(*value),
        _ => None,
    }
}

/// A resolved user option with its SQLite-signed ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserOptionValue {
    /// The snowflake converted to the signed form SQLite stores.
    pub id: i64,
    /// The raw Discord snowflake.
    pub raw_id: u64,
    /// The guild display name Discord resolved for the payload, if any.
    pub display_name: Option<String>,
    /// Whether the payload marked the user as a bot, if known.
    pub is_bot: Option<bool>,
}

impl UserOptionValue {
    /// The resolved display name, falling back to the numeric ID.
    pub fn display_name_or_id(&self) -> String {
        self.display_name
            .clone()
            .unwrap_or_else(|| self.raw_id.to_string())
    }

    /// The resolved display name, falling back to a `<@id>` mention.
    pub fn display_name_or_mention(&self) -> String {
        self.display_name
            .clone()
            .unwrap_or_else(|| format!("<@{}>", self.raw_id))
    }
}

/// Extract a user option, failing closed on out-of-range IDs.
///
/// `Ok(None)` strictly means the option is absent (or arrived as the
/// transport's `Unknown` decode fallback); a supplied user whose ID does not
/// fit SQLite's signed range fails with `"{name} ID exceeds SQLite INTEGER"`
/// instead of masquerading as a missing option, and a wrong-typed value is a
/// schema-mismatch error.
pub fn user_option(
    options: &[InteractionOption],
    name: &str,
) -> Result<Option<UserOptionValue>, String> {
    user_value(option_value(options, name), name)
}

/// Typed view of an already-located user value; see [`user_option`].
pub fn user_value(
    value: Option<&InteractionValue>,
    name: &str,
) -> Result<Option<UserOptionValue>, String> {
    match value {
        None | Some(InteractionValue::Unknown) => Ok(None),
        Some(InteractionValue::User {
            id,
            display_name,
            is_bot,
        }) => Ok(Some(UserOptionValue {
            id: i64::try_from(*id).map_err(|_| format!("{name} ID exceeds SQLite INTEGER"))?,
            raw_id: *id,
            display_name: display_name.clone(),
            is_bot: *is_bot,
        })),
        Some(_) => Err(format!("{name} must be a Discord user")),
    }
}

/// Typed view of an already-located string value; wrong types are errors.
pub fn string_value(
    value: Option<&InteractionValue>,
    name: &str,
) -> Result<Option<String>, String> {
    match value {
        None | Some(InteractionValue::Unknown) => Ok(None),
        Some(InteractionValue::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("{name} must be text")),
    }
}

/// Typed view of an already-located integer value; wrong types are errors.
pub fn integer_value(value: Option<&InteractionValue>, name: &str) -> Result<Option<i64>, String> {
    match value {
        None | Some(InteractionValue::Unknown) => Ok(None),
        Some(InteractionValue::Integer(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("{name} must be an integer")),
    }
}

/// Typed view of an already-located boolean value; wrong types are errors.
pub fn boolean_value(value: Option<&InteractionValue>, name: &str) -> Result<Option<bool>, String> {
    match value {
        None | Some(InteractionValue::Unknown) => Ok(None),
        Some(InteractionValue::Boolean(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("{name} must be true or false")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(name: &str, value: InteractionValue) -> InteractionOption {
        InteractionOption {
            name: name.to_owned(),
            value,
        }
    }

    fn user(name: &str, id: u64, display_name: Option<&str>) -> InteractionOption {
        option(
            name,
            InteractionValue::User {
                id,
                display_name: display_name.map(str::to_owned),
                is_bot: Some(false),
            },
        )
    }

    #[test]
    fn typed_options_read_present_values() {
        let options = vec![
            option("note", InteractionValue::String("hello".to_owned())),
            option("amount", InteractionValue::Integer(42)),
            option("enabled", InteractionValue::Boolean(true)),
        ];
        assert_eq!(string_option(&options, "note"), Some("hello"));
        assert_eq!(integer_option(&options, "amount"), Some(42));
        assert_eq!(boolean_option(&options, "enabled"), Some(true));
    }

    #[test]
    fn typed_options_treat_absent_and_wrong_type_as_none() {
        let options = vec![option("amount", InteractionValue::String("42".to_owned()))];
        assert_eq!(string_option(&options, "missing"), None);
        assert_eq!(integer_option(&options, "amount"), None);
        assert_eq!(boolean_option(&options, "amount"), None);
    }

    #[test]
    fn user_option_reads_present_user() {
        let options = vec![user("target", 5, Some("Miner"))];
        let user = user_option(&options, "target")
            .expect("valid user")
            .expect("present user");
        assert_eq!(user.id, 5);
        assert_eq!(user.raw_id, 5);
        assert_eq!(user.display_name_or_id(), "Miner");
        assert_eq!(user.display_name_or_mention(), "Miner");
    }

    #[test]
    fn user_option_distinguishes_absent_from_out_of_range() {
        assert_eq!(user_option(&[], "target"), Ok(None));
        let out_of_range = vec![user("target", u64::MAX, None)];
        assert_eq!(
            user_option(&out_of_range, "target"),
            Err("target ID exceeds SQLite INTEGER".to_owned())
        );
    }

    #[test]
    fn user_option_rejects_wrong_type_and_ignores_unknown() {
        let wrong = vec![option("target", InteractionValue::Integer(5))];
        assert_eq!(
            user_option(&wrong, "target"),
            Err("target must be a Discord user".to_owned())
        );
        let unknown = vec![option("target", InteractionValue::Unknown)];
        assert_eq!(user_option(&unknown, "target"), Ok(None));
    }

    #[test]
    fn user_display_name_falls_back_to_id_or_mention() {
        let options = vec![user("target", 7, None)];
        let user = user_option(&options, "target")
            .expect("valid user")
            .expect("present user");
        assert_eq!(user.display_name_or_id(), "7");
        assert_eq!(user.display_name_or_mention(), "<@7>");
    }

    #[test]
    fn strict_values_error_on_wrong_type_and_pass_unknown() {
        let value = InteractionValue::Boolean(true);
        assert_eq!(
            string_value(Some(&value), "note"),
            Err("note must be text".to_owned())
        );
        assert_eq!(
            integer_value(Some(&value), "amount"),
            Err("amount must be an integer".to_owned())
        );
        assert_eq!(
            boolean_value(Some(&InteractionValue::Integer(1)), "enabled"),
            Err("enabled must be true or false".to_owned())
        );
        assert_eq!(
            string_value(Some(&InteractionValue::Unknown), "note"),
            Ok(None)
        );
        assert_eq!(integer_value(None, "amount"), Ok(None));
        assert_eq!(
            boolean_value(Some(&InteractionValue::Boolean(false)), "enabled"),
            Ok(Some(false))
        );
        assert_eq!(
            integer_value(Some(&InteractionValue::Integer(9)), "amount"),
            Ok(Some(9))
        );
        assert_eq!(
            string_value(Some(&InteractionValue::String("x".to_owned())), "note"),
            Ok(Some("x".to_owned()))
        );
    }
}

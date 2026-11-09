//! Numeric coercion shared by legacy JSON projections.
//!
//! Legacy Python payload readers used `int(value)`: JSON integers, finite
//! floats, booleans, and decimal strings are accepted, with floats truncated
//! toward zero. Rust's `serde_json::Value::as_i64` accepts only an
//! already-integral JSON number, so using it directly would reject historical
//! and external payloads.

use serde_json::Value;

/// What one OpenDota player object's `purchase_log` gives the
/// `wrapped_enrichment_facts` row, read in one pass. Shared by every writer of
/// the facts table that reads raw match JSON.
#[derive(Debug, PartialEq, Eq)]
pub struct WrappedPurchaseFacts {
    /// Divine Rapiers bought; zero without a purchase log.
    pub rapier_count: i64,
    /// A JSON object of tracked-item purchase counts, or `None` (SQL NULL)
    /// when the player has no `purchase_log`.
    pub item_counts: Option<String>,
}

pub fn wrapped_purchase_facts(
    player: Option<&serde_json::Map<String, Value>>,
) -> Result<WrappedPurchaseFacts, serde_json::Error> {
    let Some(purchases) = player
        .and_then(|fields| fields.get("purchase_log"))
        .and_then(Value::as_array)
    else {
        return Ok(WrappedPurchaseFacts {
            rapier_count: 0,
            item_counts: None,
        });
    };
    let counts = cama_domain::wrapped_distinct::tracked_item_counts(
        purchases
            .iter()
            .filter_map(|purchase| purchase.get("key").and_then(Value::as_str)),
    );
    Ok(WrappedPurchaseFacts {
        rapier_count: counts.get("rapier").copied().unwrap_or_default(),
        item_counts: Some(serde_json::to_string(&counts)?),
    })
}

/// Apply Python's useful `int(value)` semantics while staying inside `i64`.
///
/// Values outside SQLite's signed-integer range are rejected instead of being
/// silently saturated by Rust's float-to-integer cast.
pub fn coerce_i64_like_python(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_u64().and_then(|value| i64::try_from(value).ok()))
            .or_else(|| number.as_f64().and_then(checked_truncate_f64)),
        Value::String(value) => parse_python_decimal_i64(value),
        Value::Bool(value) => Some(i64::from(*value)),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

/// Read an integer-shaped JSON scalar without truncating a fractional value.
///
/// This matches Python equality between an integer Steam ID and a JSON number:
/// `12345.0 == 12345`, while `12345.9 != 12345`.
pub fn coerce_integral_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_u64().and_then(|value| i64::try_from(value).ok()))
            .or_else(|| {
                number
                    .as_f64()
                    .filter(|value| value.fract() == 0.0)
                    .and_then(checked_truncate_f64)
            }),
        Value::Bool(value) => Some(i64::from(*value)),
        Value::Null | Value::String(_) | Value::Array(_) | Value::Object(_) => None,
    }
}

/// Read a JSON number as `f64` without accepting strings or booleans.
pub fn number_f64(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
}

fn checked_truncate_f64(value: f64) -> Option<i64> {
    // `i64::MAX as f64` rounds up to 2^63, so the upper bound is exclusive.
    const I64_EXCLUSIVE_UPPER_BOUND: f64 = 9_223_372_036_854_775_808.0;

    (value.is_finite() && value >= i64::MIN as f64 && value < I64_EXCLUSIVE_UPPER_BOUND)
        .then(|| value.trunc() as i64)
}

fn parse_python_decimal_i64(value: &str) -> Option<i64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let bytes = value.as_bytes();
    let digit_start = usize::from(matches!(bytes.first(), Some(b'+') | Some(b'-')));
    if digit_start == bytes.len() {
        return None;
    }

    for (index, byte) in bytes.iter().copied().enumerate().skip(digit_start) {
        if byte.is_ascii_digit() {
            continue;
        }
        if byte != b'_'
            || index == digit_start
            || index + 1 == bytes.len()
            || !bytes[index - 1].is_ascii_digit()
            || !bytes[index + 1].is_ascii_digit()
        {
            return None;
        }
    }

    value.replace('_', "").parse().ok()
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn python_i64_coercion_accepts_supported_json_scalars() {
        assert_eq!(coerce_i64_like_python(&json!(74)), Some(74));
        assert_eq!(coerce_i64_like_python(&json!(74.9)), Some(74));
        assert_eq!(coerce_i64_like_python(&json!(-74.9)), Some(-74));
        assert_eq!(coerce_i64_like_python(&json!("  +7_400  ")), Some(7_400));
        assert_eq!(coerce_i64_like_python(&json!(true)), Some(1));
    }

    #[test]
    fn python_i64_coercion_rejects_invalid_and_out_of_range_values() {
        for invalid in [json!("74.9"), json!("7__4"), json!("_74"), Value::Null] {
            assert_eq!(coerce_i64_like_python(&invalid), None);
        }
        let unsigned =
            serde_json::from_str::<Value>(r#"9223372036854775808"#).expect("unsigned JSON integer");
        assert_eq!(coerce_i64_like_python(&unsigned), None);
        assert_eq!(checked_truncate_f64(9_223_372_036_854_775_808.0), None);
        assert_eq!(checked_truncate_f64(f64::INFINITY), None);
    }

    #[test]
    fn purchase_facts_come_from_one_read_of_the_log() {
        let player = json!({"purchase_log": [
            {"key": "rapier"}, {"key": "magic_wand"}, {"key": "rapier"}, {"key": "tango"}
        ]});
        assert_eq!(
            wrapped_purchase_facts(player.as_object()).unwrap(),
            WrappedPurchaseFacts {
                rapier_count: 2,
                item_counts: Some(r#"{"magic_wand":1,"rapier":2}"#.to_owned()),
            }
        );
        let empty = json!({"purchase_log": []});
        assert_eq!(
            wrapped_purchase_facts(empty.as_object()).unwrap(),
            WrappedPurchaseFacts {
                rapier_count: 0,
                item_counts: Some("{}".to_owned()),
            }
        );
        for unknown in [None, json!({}).as_object()] {
            assert_eq!(
                wrapped_purchase_facts(unknown).unwrap(),
                WrappedPurchaseFacts {
                    rapier_count: 0,
                    item_counts: None,
                }
            );
        }
    }

    #[test]
    fn number_projection_preserves_integer_and_fractional_numbers_only() {
        assert_eq!(number_f64(Some(&json!(50))), Some(50.0));
        assert_eq!(number_f64(Some(&json!(50.25))), Some(50.25));
        assert_eq!(number_f64(Some(&json!("50.25"))), None);
        assert_eq!(number_f64(Some(&json!(true))), None);
    }

    #[test]
    fn integral_projection_accepts_only_exact_numeric_integers() {
        assert_eq!(coerce_integral_i64(&json!(12_345)), Some(12_345));
        assert_eq!(coerce_integral_i64(&json!(12_345.0)), Some(12_345));
        assert_eq!(coerce_integral_i64(&json!(12_345.9)), None);
        assert_eq!(coerce_integral_i64(&json!("12345")), None);
    }
}

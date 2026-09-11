//! OID string parser with `const` support.

use crate::{encoder::Encoder, Arc, Error, ObjectIdentifier, Result};

/// Const-friendly OID string parser.
///
/// Parses an OID from the dotted string representation.
#[derive(Debug)]
pub(crate) struct Parser {
    /// Current arc in progress
    current_arc: Arc,

    /// BER/DER encoder
    encoder: Encoder,
}

impl Parser {
    /// Parse an OID from a dot-delimited string e.g. `1.2.840.113549.1.1.1`
    pub(crate) const fn parse(s: &str) -> Result<Self> {
        let bytes = s.as_bytes();

        if bytes.is_empty() {
            return Err(Error::Empty);
        }

        match bytes[0] {
            b'0'..=b'9' => Self {
                current_arc: 0,
                encoder: Encoder::new(),
            }
            .parse_bytes(bytes),
            actual => Err(Error::DigitExpected { actual }),
        }
    }

    /// Finish parsing, returning the result
    pub(crate) const fn finish(self) -> Result<ObjectIdentifier> {
        self.encoder.finish()
    }

    /// Parse without recursion or wrapping an oversized arc into another OID.
    const fn parse_bytes(mut self, bytes: &[u8]) -> Result<Self> {
        let mut index = 0;
        while index < bytes.len() {
            match bytes[index] {
                byte @ b'0'..=b'9' => {
                    let digit = (byte - b'0') as Arc;
                    self.current_arc = match self.current_arc.checked_mul(10) {
                        Some(value) => match value.checked_add(digit) {
                            Some(value) => value,
                            None => return Err(Error::ArcTooBig),
                        },
                        None => return Err(Error::ArcTooBig),
                    };
                }
                b'.' => {
                    if index == 0 || bytes[index - 1] == b'.' {
                        return Err(Error::DigitExpected { actual: b'.' });
                    }
                    if index + 1 == bytes.len() {
                        return Err(Error::TrailingDot);
                    }
                    self.encoder = match self.encoder.arc(self.current_arc) {
                        Ok(encoder) => encoder,
                        Err(err) => return Err(err),
                    };
                    self.current_arc = 0;
                }
                actual => return Err(Error::DigitExpected { actual }),
            }
            index += 1;
        }
        self.encoder = match self.encoder.arc(self.current_arc) {
            Ok(encoder) => encoder,
            Err(err) => return Err(err),
        };
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::Parser;
    use crate::Error;

    #[test]
    fn parse() {
        let oid = Parser::parse("1.23.456").unwrap().finish().unwrap();
        assert_eq!(oid, "1.23.456".parse().unwrap());
    }

    #[test]
    fn reject_empty_string() {
        assert_eq!(Parser::parse("").err().unwrap(), Error::Empty);
    }

    #[test]
    fn reject_non_digits() {
        assert_eq!(
            Parser::parse("X").err().unwrap(),
            Error::DigitExpected { actual: b'X' }
        );

        assert_eq!(
            Parser::parse("1.2.X").err().unwrap(),
            Error::DigitExpected { actual: b'X' }
        );
    }

    #[test]
    fn reject_trailing_dot() {
        assert_eq!(Parser::parse("1.23.").err().unwrap(), Error::TrailingDot);
    }
}

#[cfg(test)]
mod cama_security_tests {
    use super::*;
    #[test]
    fn overflowing_decimal_arc_is_rejected() {
        assert_eq!(
            Parser::parse("1.2.4294967296").unwrap_err(),
            Error::ArcTooBig
        );
        assert!(Parser::parse("1.2.4294967295").is_ok());
    }
    #[test]
    fn consecutive_separators_cannot_alias_a_zero_arc() {
        for input in ["1..2", "1.2..3"] {
            assert_eq!(
                Parser::parse(input).unwrap_err(),
                Error::DigitExpected { actual: b'.' }
            );
        }
        assert!(Parser::parse("1.0.2").is_ok());
    }

    #[test]
    fn long_decimal_arc_does_not_recurse() {
        let input = std::format!("1.2.{}", "0".repeat(100_000));
        assert_eq!(
            Parser::parse(&input).unwrap().finish().unwrap(),
            "1.2.0".parse().unwrap()
        );
    }
}

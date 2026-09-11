//! Arcs are integer values which exist within an OID's hierarchy.

use crate::{Error, ObjectIdentifier, Result};

/// Type alias used to represent an "arc" (i.e. integer identifier value).
///
/// X.660 does not define a maximum size of an arc.
///
/// The current representation is `u32`, which has been selected as being
/// sufficient to cover the current PKCS/PKIX use cases this library has been
/// used in conjunction with.
///
/// Future versions may potentially make it larger if a sufficiently important
/// use case is discovered.
pub type Arc = u32;

/// Maximum value of the first arc in an OID.
pub(crate) const ARC_MAX_FIRST: Arc = 2;

/// Maximum value of the second arc in an OID.
pub(crate) const ARC_MAX_SECOND: Arc = 39;

/// [`Iterator`] over [`Arc`] values (a.k.a. nodes) in an [`ObjectIdentifier`].
///
/// This iterates over all arcs in an OID, including the root.
pub struct Arcs<'a> {
    /// OID we're iterating over
    oid: &'a ObjectIdentifier,

    /// Current position within the serialized DER bytes of this OID
    cursor: Option<usize>,
}

impl<'a> Arcs<'a> {
    /// Create a new iterator over the arcs of this OID
    pub(crate) fn new(oid: &'a ObjectIdentifier) -> Self {
        Self { oid, cursor: None }
    }

    /// Try to parse the next arc in this OID.
    ///
    /// This method is fallible so it can be used as a first pass to determine
    /// that the arcs in the OID are well-formed.
    pub(crate) fn try_next(&mut self) -> Result<Option<Arc>> {
        match self.cursor {
            // Indicates we're on the root OID
            None => {
                let root = RootArcs::try_from(self.oid.as_bytes()[0])?;
                self.cursor = Some(0);
                Ok(Some(root.first_arc()))
            }
            Some(0) => {
                let root = RootArcs::try_from(self.oid.as_bytes()[0])?;
                self.cursor = Some(1);
                Ok(Some(root.second_arc()))
            }
            Some(offset) => {
                let mut result: Arc = 0;
                let mut arc_bytes = 0;

                loop {
                    let len = checked_add!(offset, arc_bytes);

                    match self.oid.as_bytes().get(len).cloned() {
                        // Check the accumulated value before each base-128 step.
                        #[allow(clippy::integer_arithmetic)]
                        Some(byte) => {
                            arc_bytes = checked_add!(arc_bytes, 1);

                            if arc_bytes == 1 && byte == 0x80 {
                                return Err(Error::Base128);
                            }
                            result = match result.checked_mul(128 as Arc) {
                                Some(value) => match value.checked_add((byte & 0x7f) as Arc) {
                                    Some(value) => value,
                                    None => return Err(Error::ArcTooBig),
                                },
                                None => return Err(Error::ArcTooBig),
                            };

                            if byte & 0b10000000 == 0 {
                                self.cursor = Some(checked_add!(offset, arc_bytes));
                                return Ok(Some(result));
                            }
                        }
                        None => {
                            if arc_bytes == 0 {
                                return Ok(None);
                            } else {
                                return Err(Error::Base128);
                            }
                        }
                    }
                }
            }
        }
    }
}

impl<'a> Iterator for Arcs<'a> {
    type Item = Arc;

    fn next(&mut self) -> Option<Arc> {
        // ObjectIdentifier constructors should ensure the OID is well-formed
        self.try_next().expect("OID malformed")
    }
}

/// Byte containing the first and second arcs of an OID.
///
/// This is represented this way in order to reduce the overall size of the
/// [`ObjectIdentifier`] struct.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct RootArcs(u8);

impl RootArcs {
    /// Create [`RootArcs`] from the first and second arc values represented
    /// as `Arc` integers.
    pub(crate) const fn new(first_arc: Arc, second_arc: Arc) -> Result<Self> {
        if first_arc > ARC_MAX_FIRST {
            return Err(Error::ArcInvalid { arc: first_arc });
        }

        if second_arc > ARC_MAX_SECOND {
            return Err(Error::ArcInvalid { arc: second_arc });
        }

        // The checks above ensure this operation will not overflow
        #[allow(clippy::integer_arithmetic)]
        let byte = (first_arc * (ARC_MAX_SECOND + 1)) as u8 + second_arc as u8;

        Ok(Self(byte))
    }

    /// Get the value of the first arc
    #[allow(clippy::integer_arithmetic)]
    pub(crate) const fn first_arc(self) -> Arc {
        self.0 as Arc / (ARC_MAX_SECOND + 1)
    }

    /// Get the value of the second arc
    #[allow(clippy::integer_arithmetic)]
    pub(crate) const fn second_arc(self) -> Arc {
        self.0 as Arc % (ARC_MAX_SECOND + 1)
    }
}

impl TryFrom<u8> for RootArcs {
    type Error = Error;

    // Ensured not to overflow by constructor invariants
    #[allow(clippy::integer_arithmetic)]
    fn try_from(octet: u8) -> Result<Self> {
        let first = octet as Arc / (ARC_MAX_SECOND + 1);
        let second = octet as Arc % (ARC_MAX_SECOND + 1);
        let result = Self::new(first, second)?;
        debug_assert_eq!(octet, result.0);
        Ok(result)
    }
}

impl From<RootArcs> for u8 {
    fn from(root_arcs: RootArcs) -> u8 {
        root_arcs.0
    }
}

#[cfg(test)]
mod cama_der_bounds_tests {
    use crate::{Error, ObjectIdentifier};

    #[test]
    fn rejects_binary_arc_overflow_instead_of_aliasing_zero() {
        let encoded = [42, 0x90, 0x80, 0x80, 0x80, 0];
        assert_eq!(
            ObjectIdentifier::from_bytes(&encoded).unwrap_err(),
            Error::ArcTooBig
        );
    }

    #[test]
    fn largest_binary_arc_round_trips_and_iterates() {
        let encoded = [42, 0x8f, 0xff, 0xff, 0xff, 0x7f];
        let oid = ObjectIdentifier::from_bytes(&encoded).unwrap();
        assert_eq!(oid, ObjectIdentifier::new("1.2.4294967295").unwrap());
        let mut arcs = oid.arcs();
        assert_eq!(arcs.next(), Some(1));
        assert_eq!(arcs.next(), Some(2));
        assert_eq!(arcs.next(), Some(u32::MAX));
        assert_eq!(arcs.next(), None);
    }

    #[test]
    fn rejects_nonminimal_base128_and_truncated_arcs() {
        for encoded in [&[42, 0x80, 0][..], &[42, 0x80, 1], &[42, 0x81]] {
            assert_eq!(
                ObjectIdentifier::from_bytes(encoded).unwrap_err(),
                Error::Base128
            );
        }
    }
}

//! The one shared constant-time byte-slice equality helper.
//!
//! Adopted per design D4: a single vetted primitive ([`subtle::ConstantTimeEq`]) backs both the
//! box's edge service-credential check (`runlet`) and the daemon's static-token check (`fabricd`),
//! replacing two duplicated hand-rolled copies. A length difference short-circuits (a secret's
//! length is not itself secret); equal-length inputs are compared without a data-dependent branch.

use subtle::ConstantTimeEq as _;

/// Returns `true` iff `lhs` and `rhs` are byte-identical, compared in constant time for equal-length
/// inputs. Differing lengths return `false` immediately (length is not the secret).
#[must_use]
pub fn ct_eq(lhs: &[u8], rhs: &[u8]) -> bool {
    if lhs.len() != rhs.len() {
        return false;
    }
    lhs.ct_eq(rhs).into()
}

#[cfg(test)]
mod tests {
    //! Equal/unequal/length-mismatch/empty cases for the shared constant-time compare.

    use super::ct_eq;

    /// Identical bytes match; a single differing byte at equal length does not.
    #[test]
    fn matches_only_identical_bytes() {
        assert!(ct_eq(b"s3cret-token", b"s3cret-token"), "identical matches");
        assert!(
            !ct_eq(b"s3cret-token", b"s3cret-tokeX"),
            "same length, one byte off"
        );
        assert!(
            !ct_eq(b"short", b"longer-token"),
            "different length differs"
        );
        assert!(ct_eq(b"", b""), "empty equals empty");
    }
}

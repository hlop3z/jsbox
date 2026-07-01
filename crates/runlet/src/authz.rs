//! Coarse member-capability authorization (section 5 of the multitenant-trust change).
//!
//! A config-driven `capability → required entitlement` map (`trusted.capability_entitlements`) gates
//! which capability a member may invoke, keyed off the caller's trusted `x-user-roles` /
//! `x-user-entitlements`. This is deliberately coarse (set-membership, not fine-grained
//! role→resource policy — that is a v2 concern, revisit Cedar): "may this member use `db` at all".
//! A capability kind absent from the map is ungated. Runs before the capability does.

use std::collections::HashMap;

use crate::identity::TrustedIdentity;

/// A member lacked the entitlement required to invoke a capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapabilityDenied {
    /// The capability kind that was gated (`"db"`, `"mail"`, …).
    pub(crate) capability: String,
    /// The entitlement the caller would need to hold.
    pub(crate) required: String,
}

/// Authorizes each requested capability against the caller's trusted grants. For every requested
/// kind that the map gates, the caller must hold the required entitlement (as a role or an
/// entitlement); the first violation is returned. Ungated kinds always pass.
///
/// # Errors
///
/// Returns the first [`CapabilityDenied`] for a gated capability the caller may not use.
pub(crate) fn authorize_capabilities(
    map: &HashMap<String, String>,
    requested: &[&str],
    identity: &TrustedIdentity,
) -> Result<(), CapabilityDenied> {
    if map.is_empty() {
        return Ok(());
    }
    for kind in requested {
        if let Some(required) = map.get(*kind)
            && !identity.has_grant(required)
        {
            return Err(CapabilityDenied {
                capability: (*kind).to_owned(),
                required: required.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Permit (holds the entitlement / ungated kind) and deny (gated kind, entitlement absent).

    use super::authorize_capabilities;
    use crate::identity::TrustedIdentity;
    use std::collections::HashMap;

    /// A `capability → entitlement` gate map.
    fn gate(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(cap, ent)| ((*cap).to_owned(), (*ent).to_owned()))
            .collect()
    }

    /// An identity holding the given entitlements (no roles).
    fn holder(entitlements: &[&str]) -> TrustedIdentity {
        TrustedIdentity {
            entitlements: entitlements.iter().map(|item| (*item).to_owned()).collect(),
            ..TrustedIdentity::default()
        }
    }

    /// A member holding the required entitlement is permitted; an ungated kind always passes.
    #[test]
    fn permits_when_entitled_or_ungated() {
        let map = gate(&[("db", "db.write")]);
        let id = holder(&["db.write"]);
        assert!(
            authorize_capabilities(&map, &["db"], &id).is_ok(),
            "holds db.write"
        );
        assert!(
            authorize_capabilities(&map, &["mail"], &id).is_ok(),
            "mail is ungated"
        );
    }

    /// A member lacking the required entitlement is denied on the gated capability.
    #[test]
    fn denies_when_missing_entitlement() {
        let map = gate(&[("db", "db.write")]);
        let id = holder(&["mail.send"]);
        match authorize_capabilities(&map, &["db"], &id) {
            Ok(()) => unreachable!("missing db.write must deny"),
            Err(denied) => {
                assert_eq!(denied.capability, "db");
                assert_eq!(denied.required, "db.write");
            }
        }
    }

    /// A role (not just an entitlement) also satisfies the gate.
    #[test]
    fn role_satisfies_gate() {
        let map = gate(&[("db", "admin")]);
        let id = TrustedIdentity {
            roles: vec!["admin".to_owned()],
            ..TrustedIdentity::default()
        };
        assert!(
            authorize_capabilities(&map, &["db"], &id).is_ok(),
            "role grants"
        );
    }

    /// An empty gate map permits everything (member gating disabled).
    #[test]
    fn empty_map_permits_all() {
        let map = HashMap::new();
        let id = TrustedIdentity::default();
        assert!(authorize_capabilities(&map, &["db", "mail"], &id).is_ok());
    }
}

//! Whether an external identity provider may create an account here.
//!
//! An SSO callback and an LDAP first login both end in the same place: an
//! `INSERT` into `users`. Until this module existed there was nothing between
//! "the provider says this identity is valid" and that insert, so a configured
//! `github.com` or `google.com` provider meant *the entire internet* could hold
//! an account on the instance — and `[auth].registration = "closed"`
//! ([`crate::user::registration`]) did not close it, because it deliberately
//! governs self-service sign-up only.
//!
//! The missing piece was never a check; it was somewhere for the operator to
//! write the answer down. `sso_providers.auto_provision` and
//! `sso_providers.allowed_email_domains` are that place, and this module is the
//! one reader of them, so both doors decide identically:
//!
//! * **Provisioning is not signing in.** The policy is consulted on the branch
//!   that would create an account and nowhere else. An account that already
//!   exists — linked to this provider, or (for LDAP) bound to this directory —
//!   keeps signing in after the switch goes off. Turning `auto_provision` off
//!   must lock out strangers, not the people already using the instance. An SSO
//!   identity is never matched to an existing account by its email
//!   (card_4753cfe7b985): the account links the provider from inside itself.
//! * **A refusal is an answer, not a failure.** Both call sites turn
//!   [`ProvisioningRefusal`] into a `403` that says which of the two rules
//!   refused, because "ask an administrator" is only actionable if the person
//!   reading it knows whether their domain or the whole provider is the
//!   problem.
//!
//! The upgrade defaults live one layer down (`auto_provision` is `NOT NULL
//! DEFAULT TRUE`, so an existing instance behaves exactly as it did) and the
//! new-provider default one layer up (the admin API starts a provider at
//! `false`, making "everyone" a thing an operator types rather than a thing
//! they inherit).

use rg_db::entities::sso_provider;

/// Why a first-login provisioning was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisioningRefusal {
    /// The provider is configured never to create accounts.
    AutoProvisionDisabled,
    /// The provider creates accounts, but not for this address's domain.
    EmailDomainNotAllowed,
    /// The provider creates accounts for some domains only, and did not vouch
    /// that the address it asserted belongs to the person signing in — so its
    /// domain proves nothing.
    EmailNotVerified,
}

impl ProvisioningRefusal {
    /// What the person signing in is told. Says which rule refused: the two
    /// have different remedies, and only one of them is worth mailing an
    /// administrator about.
    pub fn message(self) -> &'static str {
        match self {
            Self::AutoProvisionDisabled => {
                "this provider does not create new accounts on this instance; \
                 ask an administrator to create one for you"
            }
            Self::EmailDomainNotAllowed => {
                "this provider does not create accounts for your email domain; \
                 ask an administrator to create one for you"
            }
            Self::EmailNotVerified => {
                "this provider creates accounts only for confirmed addresses, and it \
                 did not confirm yours; confirm the address with the provider, or ask \
                 an administrator to create an account for you"
            }
        }
    }

    /// Stable label for metrics and operator logs.
    pub fn reason(self) -> &'static str {
        match self {
            Self::AutoProvisionDisabled => "auto_provision_disabled",
            Self::EmailDomainNotAllowed => "email_domain_not_allowed",
            Self::EmailNotVerified => "email_not_verified",
        }
    }
}

impl std::fmt::Display for ProvisioningRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for ProvisioningRefusal {}

/// Whether the provider vouched that the address it asserted is the signing-in
/// person's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressAssurance {
    /// The provider confirmed the address (`email_verified: true`, GitHub's
    /// `/user/emails`, GitLab's confirmed primary), or the address comes from a
    /// directory the operator runs (LDAP).
    Verified,
    /// The provider said nothing about the address, or said it is unconfirmed.
    Unverified,
}

impl AddressAssurance {
    /// Read an SSO profile's three-valued `email_verified`. Only an explicit
    /// `true` is a confirmation: `None` is a provider that did not say.
    pub fn from_claim(email_verified: Option<bool>) -> Self {
        if email_verified == Some(true) {
            Self::Verified
        } else {
            Self::Unverified
        }
    }
}

/// May `provider` create an account for the address it just asserted?
///
/// Called on the creation branch only — see the module note on why an existing
/// account never reaches this function.
///
/// An allowlist is a statement about *people* ("accounts only for staff"), and
/// the domain is evidence of that only when the provider vouched for the
/// address. GitLab and many OIDC providers let a user type any address into
/// their profile, so an unconfirmed `someone@corp.example` would otherwise buy
/// a stranger an account here — and the address with it (card_7099e8a305bc).
/// Without an allowlist the domain decides nothing, so assurance is not asked.
pub fn authorize(
    provider: &sso_provider::Model,
    email: &str,
    assurance: AddressAssurance,
) -> Result<(), ProvisioningRefusal> {
    if !provider.auto_provision {
        return Err(ProvisioningRefusal::AutoProvisionDisabled);
    }
    let Some(allowlist) = provider.allowed_email_domains.as_deref() else {
        return Ok(());
    };
    if !allows_domain(allowlist, email) {
        return Err(ProvisioningRefusal::EmailDomainNotAllowed);
    }
    match assurance {
        AddressAssurance::Verified => Ok(()),
        AddressAssurance::Unverified => Err(ProvisioningRefusal::EmailNotVerified),
    }
}

/// Does a stored allowlist admit this address?
///
/// Exact domain match, case-insensitive: `example.com` does not admit
/// `evil-example.com` and does not admit `mail.example.com` either. Subdomain
/// matching is the kind of convenience that turns an allowlist into a suffix
/// check, and a suffix check is how `notexample.com` gets in.
///
/// A list that parses to nothing (`",,"`, whitespace) admits nobody. The
/// operator wrote *something* in the box; reading it as "no restriction" would
/// turn a typo into an open door, which is the exact failure this column
/// exists to prevent.
fn allows_domain(allowlist: &str, email: &str) -> bool {
    let Some(domain) = email.rsplit_once('@').map(|(_, domain)| domain) else {
        return false;
    };
    if domain.is_empty() {
        return false;
    }
    parse_domains(allowlist).any(|allowed| allowed.eq_ignore_ascii_case(domain))
}

/// Split a stored / submitted allowlist into its entries, tolerating the
/// spellings an admin form produces: spaces around commas, a leading `@`, and
/// mixed case.
fn parse_domains(raw: &str) -> impl Iterator<Item = &str> {
    raw.split(',')
        .map(|entry| entry.trim().trim_start_matches('@'))
        .filter(|entry| !entry.is_empty())
}

/// Canonicalise an allowlist submitted by an admin, or say what is wrong with it.
///
/// Returns `None` for "no restriction" so an emptied form field clears the
/// column instead of storing a blank string that [`allows_domain`] would then
/// read as "nobody". Validation is deliberately here rather than in the HTTP
/// layer: the matcher and the thing that decides what may be stored have to
/// agree, and one function is how they stay agreed.
pub fn normalize_email_domains(raw: &str) -> Result<Option<String>, String> {
    let mut domains: Vec<String> = Vec::new();
    for entry in parse_domains(raw) {
        if entry.contains(char::is_whitespace) || entry.contains('@') || entry.contains('/') {
            return Err(format!("`{entry}` is not a valid email domain"));
        }
        if !entry.contains('.') {
            return Err(format!(
                "`{entry}` is not a valid email domain (a domain needs a dot)"
            ));
        }
        let entry = entry.to_ascii_lowercase();
        if !domains.contains(&entry) {
            domains.push(entry);
        }
    }
    if domains.is_empty() {
        return Ok(None);
    }
    Ok(Some(domains.join(",")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERIFIED: AddressAssurance = AddressAssurance::Verified;

    fn provider(auto_provision: bool, allowed: Option<&str>) -> sso_provider::Model {
        let now = chrono::Utc::now();
        sso_provider::Model {
            id: 1,
            name: "Public IdP".into(),
            slug: "github".into(),
            provider_type: "oauth2".into(),
            client_id: None,
            client_secret_enc: None,
            discovery_url: None,
            scopes: None,
            ldap_host: None,
            ldap_port: None,
            ldap_bind_dn: None,
            ldap_bind_password_enc: None,
            ldap_base_dn: None,
            ldap_user_filter: None,
            enabled: true,
            auto_provision,
            allowed_email_domains: allowed.map(str::to_string),
            icon_url: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn a_provider_without_auto_provision_creates_nobody() {
        assert_eq!(
            authorize(&provider(false, None), "stranger@example.com", VERIFIED),
            Err(ProvisioningRefusal::AutoProvisionDisabled)
        );
        // The allowlist does not rescue a provider that provisions nobody.
        assert_eq!(
            authorize(
                &provider(false, Some("example.com")),
                "alice@example.com",
                VERIFIED
            ),
            Err(ProvisioningRefusal::AutoProvisionDisabled)
        );
    }

    #[test]
    fn no_allowlist_means_no_domain_restriction() {
        assert_eq!(
            authorize(&provider(true, None), "anyone@anywhere.io", VERIFIED),
            Ok(())
        );
    }

    #[test]
    fn the_allowlist_matches_the_exact_domain_only() {
        let corp = provider(true, Some("example.com,partner.org"));
        assert_eq!(authorize(&corp, "alice@example.com", VERIFIED), Ok(()));
        assert_eq!(authorize(&corp, "bob@EXAMPLE.COM", VERIFIED), Ok(()));
        assert_eq!(authorize(&corp, "carol@partner.org", VERIFIED), Ok(()));

        // A suffix check would let all three of these in.
        for outsider in [
            "mallory@evil-example.com",
            "mallory@mail.example.com",
            "mallory@example.com.evil.net",
        ] {
            assert_eq!(
                authorize(&corp, outsider, VERIFIED),
                Err(ProvisioningRefusal::EmailDomainNotAllowed),
                "{outsider} slipped past the allowlist"
            );
        }
    }

    /// card_7099e8a305bc: the domain is evidence about the person only when
    /// the provider vouched for the address. An unconfirmed address inside the
    /// allowlist is refused with its own reason; one outside it keeps the
    /// domain reason, so the refusal names the rule that actually failed.
    #[test]
    fn an_allowlist_admits_only_an_address_the_provider_vouched_for() {
        let corp = provider(true, Some("example.com"));
        assert_eq!(
            authorize(&corp, "someone@example.com", AddressAssurance::Unverified),
            Err(ProvisioningRefusal::EmailNotVerified)
        );
        assert_eq!(
            authorize(&corp, "someone@outsider.io", AddressAssurance::Unverified),
            Err(ProvisioningRefusal::EmailDomainNotAllowed)
        );
        assert_eq!(authorize(&corp, "someone@example.com", VERIFIED), Ok(()));
        // Without an allowlist the domain decides nothing, so neither does
        // whether anybody confirmed it.
        assert_eq!(
            authorize(
                &provider(true, None),
                "someone@example.com",
                AddressAssurance::Unverified
            ),
            Ok(())
        );
        assert_eq!(
            AddressAssurance::from_claim(None),
            AddressAssurance::Unverified
        );
        assert_eq!(
            AddressAssurance::from_claim(Some(false)),
            AddressAssurance::Unverified
        );
        assert_eq!(
            AddressAssurance::from_claim(Some(true)),
            AddressAssurance::Verified
        );
    }

    #[test]
    fn an_address_without_a_domain_never_matches_an_allowlist() {
        let corp = provider(true, Some("example.com"));
        for malformed in ["not-an-address", "trailing@", ""] {
            assert_eq!(
                authorize(&corp, malformed, VERIFIED),
                Err(ProvisioningRefusal::EmailDomainNotAllowed)
            );
        }
    }

    /// A list the operator wrote that contains no usable entry admits nobody:
    /// reading it as "unrestricted" would make a typo open the instance.
    #[test]
    fn an_unparseable_allowlist_admits_nobody() {
        assert_eq!(
            authorize(&provider(true, Some(" , ")), "alice@example.com", VERIFIED),
            Err(ProvisioningRefusal::EmailDomainNotAllowed)
        );
    }

    #[test]
    fn normalisation_canonicalises_what_a_form_produces() {
        assert_eq!(
            normalize_email_domains(" @Example.COM , partner.org ,example.com").unwrap(),
            Some("example.com,partner.org".to_string())
        );
        assert_eq!(normalize_email_domains("   ").unwrap(), None);
        assert_eq!(normalize_email_domains("").unwrap(), None);
    }

    #[test]
    fn normalisation_rejects_what_is_not_a_domain() {
        for bad in [
            "alice@example.com",
            "exa mple.com",
            "example.com/path",
            "localhost",
        ] {
            assert!(
                normalize_email_domains(bad).is_err(),
                "`{bad}` was accepted as an email domain"
            );
        }
    }

    /// The canonical form written by an admin is the form the matcher reads.
    #[test]
    fn a_normalised_list_matches_the_address_it_was_written_for() {
        let stored = normalize_email_domains("@Corp.Example ").unwrap();
        assert_eq!(
            authorize(
                &provider(true, stored.as_deref()),
                "alice@corp.example",
                VERIFIED
            ),
            Ok(())
        );
    }
}

//! Vendor account-ban detection (two tiers).
//!
//! - **High-confidence** ([`is_firecrawl_banned`]): Firecrawl's exact live ban
//!   copy → the key row is hard-DELETEd (irreversible, signature proven).
//! - **Exact-tavily** ([`is_tavily_banned`]): Tavily's exact deactivation body
//!   → the key is SUSPENDED (active=0).
//! - **Likely** ([`is_likely_banned`]): generic ACCOUNT-STATE PHRASES on any
//!   other vendor → the key is DISABLED (active=0) instead — instantly out of
//!   rotation, PERMANENT: an operator re-enable is the only way back.
//!
//! All three dispositions are operator-re-enable-only since schema 18: the
//! re-enable cron's `KEY_REENABLE_AFTER_HOURS` sweep skips
//! `disabled_reason = 'vendor_suspended'`, so a ban never "heals" on a timer
//! and re-serves `401`/`403` forever. Because the likely tier is PERMANENT,
//! it must fire on a phrase about the ACCOUNT, never a bare word: proxy `403`
//! middleware pages, quota copy, and vendor prose all contain "suspended" or
//! "revoked" without the account being banned, and a false positive here can
//! no longer be undone by a cron tick.

/// Live Firecrawl ban body (credit-usage / search / extract), captured 2026-07-30.
#[cfg(test)]
pub const FIRECRAWL_BAN_BODY_FIXTURE: &str = r#"{"success":false,"error":"Unauthorized: This account has been banned. Contact support@firecrawl.com if you believe this is a mistake."}"#;
/// Live Tavily deactivation body (captured 2026-08-27 from the maxim rotating
/// log — this is the real, current Tavily ban copy).
#[cfg(test)]
pub const TAVILY_BAN_BODY_FIXTURE: &str = r#"{"detail":{"error":"The account associated with this API key has been deactivated. If you wish to reactivate your subscription, please contact our team at support@tavily.com."}}"#;

/// Firecrawl's OWN tier, which is the ONLY path that hard-DELETEs the key row
/// (`finish_banned` -> `revoke_key_row`, irreversible). It therefore must not
/// be LOOSER than the likely tier: the short marker `"has been banned"` alone
/// matched any `403` whose copy applied the phrase to something other than the
/// account ("your IP has been banned", "this request has been banned"), and a
/// CDN/WAF page in front of Firecrawl would then destroy a healthy key row
/// permanently. Requiring the account subject keeps the live fixture
/// (`FIRECRAWL_BAN_BODY_FIXTURE`: "This account has been banned") and drops the
/// loose form.
const BAN_MARKERS: &[&str] = &["account has been banned", "this account has been banned"];

/// Generic account-state detection, tokenized: a STATE token must sit within
/// [`STATE_GAP`] tokens of a SUBJECT token, joined only by [`COPULA_FILLER`].
/// Deliberately NOT matched for firecrawl (its exact tier runs instead), and
/// NOT bare words: matching the bare state words fired on proxy `403`
/// middleware pages and quota copy, permanently disabling healthy accounts.
///
/// Two failure modes drove this shape, and both are why it is neither a
/// substring scan nor a fixed phrase list:
///
/// - A substring scan matches the subject inside other words (`key` inside
///   "monkey"/"keyboard") and the state inside other phrases, and a
///   character-distance window has to be tuned per fixture: too tight and
///   real vendor copy ("the account associated with this API key has been
///   deactivated") falls through to `AuthFailure` → fail@3 with a NULL
///   `disabled_reason` → the re-enable cron resurrects a dead account to 401
///   forever; too wide and "your account is active, this request was banned"
///   permanently disables a healthy key. Tokenizing makes the unit of
///   distance a WORD, and the closed filler set decides what may sit between
///   the two words — so `account [has] [been] [permanently] suspended`
///   matches and `account … active … banned` does not.
/// - Copula forms (`has been` / `is` / `was`) are enumeration-free: they are
///   just filler tokens inside the gap, never a row in a phrase table.
///
/// Both orders are checked: "the account … has been deactivated" and
/// "access revoked for this account" state an account fact, and a
/// forward-only rule would let the second fall through to `AuthFailure` and
/// start the cron-revival loop above.
const LIKELY_BAN_SUBJECTS: &[&str] = &["account", "user", "key"];
const LIKELY_BAN_STATES: &[&str] = &["banned", "suspended", "deactivated", "revoked"];
/// The only words allowed BETWEEN a subject and a state. A closed set is the
/// whole point: without it, any N words would bridge "your account is active"
/// to "this request was banned". Copulas and adverbs carry account-state
/// ("has been permanently deactivated"); the determiner/preposition run
/// carries the LIVE Tavily construction ("the account associated with this
/// API key has been deactivated") without a table entry for it.
///
/// `not` is deliberately ABSENT. A bridgeable `not` turns "your account has
/// NOT been deactivated" and "this key was not revoked" into a ban, and this
/// tier's disable is permanent (operator re-enable only) — a negation that
/// reads as a ban to a proximity matcher is the worst false positive this
/// rule could have, and it is real vendor copy ("not suspended — check your
/// request format"). Every genuine positive needs only `has been` / `is` /
/// `now` / `was` or bare adjacency, none of which need a negation.
///
// Grouped compactly on purpose: the three tiers (copulas, status adverbs,
// determiner/preposition run) are the whole point of the rule, and a
// one-string-per-line reflow would bury that in 30 lines of noise.
#[rustfmt::skip]
const COPULA_FILLER: &[&str] = &[
    // copulas / adverbs that carry account-state
    "has", "have", "had", "been", "being", "is", "are", "was", "were", "be",
    "currently", "permanently", "temporarily", "already", "now",
    // the determiner/preposition run inside the live account-state clause
    "associated", "with", "this", "that", "which", "the", "api",
    "a", "an", "as", "due", "to", "for", "of", "and", "please",
];
/// How many filler tokens may sit between the subject and the state. Must
/// span the longest real bridge: in the live Tavily body the run between
/// `account` and `deactivated` is `associated with this api key has been` —
/// 7 tokens. 8 clears that with one to spare.
///
/// The generous ceiling is safe ONLY because the gap is filler-closed: a
/// bridge of 8 all-function-words never occurs in prose that merely mentions
/// both a subject and a state in different sentences. Real copy breaks it —
/// "account associated with this REQUEST has been FLAGGED" stops at `request`,
/// "your account has been permanently RATE limited" stops at `rate` — so
/// widening the window cannot be traded for a false positive the way a
/// character-distance window can.
const STATE_GAP: usize = 8;

/// Negation words. A negation is a STOP, never filler: listing one in
/// [`COPULA_FILLER`] would let it BRIDGE subject and state, so "this account
/// has no revoked keys" would read as a ban. Keeping it out of the filler set
/// is not sufficient on its own, because a negation placed BEFORE the
/// subject ("no account was suspended") is never visited by either walk — so
/// each subject is additionally checked for a negation governing it.
const NEGATIONS: &[&str] = &["no", "not", "never", "none", "neither", "nor"];

/// True when a negation governs the subject at `index`, scanning back over
/// filler only. That scan must be able to cross filler: "no api key has been
/// revoked" puts `api` between the negation and the subject, so stopping at
/// the first non-filler would miss the denial.
fn is_negated(tokens: &[String], index: usize, is_filler: impl Fn(&str) -> bool) -> bool {
    for back in 1..=index {
        let token = &tokens[index - back];
        if NEGATIONS.contains(&token.as_str()) {
            return true;
        }
        if !is_filler(token) {
            return false;
        }
    }
    false
}

/// Split on non-alphanumerics into lowercased tokens. Tokenizing (rather
/// than `str::find`) is what keeps `key` from matching inside "monkey" and
/// `banned` from matching inside a longer word.
fn tokenize(body: &str) -> Vec<&str> {
    body.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect()
}

/// True when some subject token and some state token are within
/// [`STATE_GAP`] tokens of each other with only [`COPULA_FILLER`] between,
/// in either order, and the subject is not governed by a negation.
fn has_account_state_phrase(body: &str) -> bool {
    let tokens: Vec<String> = tokenize(&body.to_ascii_lowercase())
        .into_iter()
        .map(str::to_string)
        .collect();
    let is_subject = |t: &str| LIKELY_BAN_SUBJECTS.contains(&t);
    let is_state = |t: &str| LIKELY_BAN_STATES.contains(&t);
    let is_filler = |t: &str| COPULA_FILLER.contains(&t);
    for (i, token) in tokens.iter().enumerate() {
        if !is_subject(token) {
            continue;
        }
        // A negated subject never bans, in either walk direction: "no account
        // was suspended" denies the state, it does not assert it.
        if is_negated(&tokens, i, is_filler) {
            continue;
        }
        // Forward: subject … filler… state.
        for step in 1..=STATE_GAP {
            let Some(next) = tokens.get(i + step) else {
                break;
            };
            if is_state(next) {
                return true;
            }
            if !is_filler(next) {
                break;
            }
        }
        // Reversed: state … filler… subject.
        for step in 1..=STATE_GAP {
            if i < step {
                break;
            }
            let prev = &tokens[i - step];
            if is_state(prev) {
                return true;
            }
            if !is_filler(prev) {
                break;
            }
        }
    }
    false
}

fn status_gate(status: u16) -> bool {
    status == 401 || status == 403
}

/// True when HTTP status is 401/403 and body matches Firecrawl ban copy.
pub fn is_firecrawl_banned(status: u16, body: &str) -> bool {
    if !status_gate(status) {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    BAN_MARKERS.iter().any(|m| lower.contains(m))
}

/// True when HTTP status is 401/403 and the body states an ACCOUNT-STATE: a
/// STATE token within [`STATE_GAP`] filler tokens of a SUBJECT token, in
/// either order (see [`has_account_state_phrase`]). The state token is
/// MANDATORY — there is no table of prefixes that can match without one.
/// Softer signal than [`is_firecrawl_banned`] — callers must pair it with a
/// disable (never a delete), and must understand the disable is PERMANENT
/// (operator re-enable only; the re-enable cron skips `vendor_suspended`).
pub fn is_likely_banned(status: u16, body: &str) -> bool {
    if !status_gate(status) {
        return false;
    }
    has_account_state_phrase(&body.to_ascii_lowercase())
}

/// True when HTTP status is 401/403 and body matches Tavily's exact
/// deactivation copy (captured from live traffic, 2026-08-27).
pub fn is_tavily_banned(status: u16, body: &str) -> bool {
    if !status_gate(status) {
        return false;
    }
    body.to_ascii_lowercase()
        .contains("the account associated with this api key has been deactivated")
}

/// Provider-dispatched ban check: firecrawl and tavily use their proven
/// exact signatures (hard-delete vs suspend respectively); every other
/// vendor uses the likely-tier matcher (suspend).
pub fn is_account_banned(provider: &str, status: u16, body: &str) -> bool {
    match provider {
        "firecrawl" => is_firecrawl_banned(status, body),
        "tavily" => is_tavily_banned(status, body) || is_likely_banned(status, body),
        _ => is_likely_banned(status, body),
    }
}

#[cfg(test)]
mod banned_tests {
    use super::*;

    #[test]
    fn fixture_403_is_banned() {
        assert!(is_firecrawl_banned(403, FIRECRAWL_BAN_BODY_FIXTURE));
    }

    #[test]
    fn fixture_401_is_banned() {
        assert!(is_firecrawl_banned(401, FIRECRAWL_BAN_BODY_FIXTURE));
    }

    #[test]
    fn case_insensitive() {
        assert!(is_firecrawl_banned(
            403,
            r#"{"error":"ACCOUNT HAS BEEN BANNED by ops"}"#
        ));
    }

    /// The delete tier (`finish_banned` -> hard row DELETE) is irreversible,
    /// so it must be at least as strict as the permanent-disable likely tier.
    /// A short `"has been banned"` marker used to match `403` copy that
    /// applied the phrase to something OTHER than the account — "your IP has
    /// been banned" from a CDN/WAF sits in front of Firecrawl, and the key row
    /// was destroyed with no way back. Only the account-subject form bans.
    #[test]
    fn firecrawl_tier_ignores_the_subjectless_marker() {
        for body in [
            "sorry, has been banned permanently",
            "your IP has been banned",
            "This request has been banned by our WAF",
            "this URL has been banned",
        ] {
            assert!(
                !is_firecrawl_banned(403, body),
                "subject-less ban copy must not hard-delete a key row: {body}"
            );
        }
    }

    #[test]
    fn plain_403_unauthorized_not_banned() {
        assert!(!is_firecrawl_banned(
            403,
            r#"{"success":false,"error":"Unauthorized"}"#
        ));
    }

    #[test]
    fn status_402_not_banned_even_with_marker() {
        assert!(!is_firecrawl_banned(402, FIRECRAWL_BAN_BODY_FIXTURE));
    }

    #[test]
    fn status_429_500_not_banned() {
        assert!(!is_firecrawl_banned(429, FIRECRAWL_BAN_BODY_FIXTURE));
        assert!(!is_firecrawl_banned(500, FIRECRAWL_BAN_BODY_FIXTURE));
    }

    #[test]
    fn tavily_exact_fixture_is_banned() {
        assert!(is_tavily_banned(401, TAVILY_BAN_BODY_FIXTURE));
        assert!(is_tavily_banned(403, TAVILY_BAN_BODY_FIXTURE));
        assert!(!is_tavily_banned(429, TAVILY_BAN_BODY_FIXTURE));
    }

    #[test]
    fn tavily_exact_requires_its_own_wording() {
        assert!(!is_tavily_banned(
            401,
            r#"{"detail":{"error":"account deactivated"}}"#
        ));
        assert!(!is_tavily_banned(401, r#"{"error":"Unauthorized"}"#));
    }

    #[test]
    fn tavily_dispatches_exact_or_likely() {
        assert!(is_account_banned("tavily", 401, TAVILY_BAN_BODY_FIXTURE));
        assert!(is_account_banned(
            "tavily",
            403,
            r#"{"error":"key revoked"}"#
        ));
        assert!(!is_account_banned(
            "tavily",
            403,
            r#"{"error":"Unauthorized"}"#
        ));
    }

    #[test]
    fn likely_tier_matches_account_phrases() {
        assert!(is_likely_banned(403, r#"{"error":"account suspended"}"#));
        assert!(is_likely_banned(401, "account deactivated by admin"));
        assert!(is_likely_banned(403, "API key revoked"));
        assert!(is_likely_banned(
            403,
            "The account associated with this API key has been suspended"
        ));
    }

    /// Narrowing the matcher to phrases must not drop a GENUINE detection.
    /// Every one of these bodies is real vendor account-state copy that the
    /// old bare-word matcher caught; if a copula form goes missing,
    /// `verdict_for` falls through to `AuthFailure`, the key takes
    /// `finish_failure` (fail@3 with a NULL `disabled_reason`), and the
    /// re-enable cron resurrects a deactivated account to 401 forever. This
    /// is the old-vs-new equivalence check: all true, all still true.
    #[test]
    fn phrase_gate_keeps_every_real_account_state_body() {
        for body in [
            "Your account has been deactivated",
            "account has been deactivated",
            "This account has been deactivated",
            "account was deactivated",
            "account is deactivated",
            "account deactivated",
            "The account associated with this API key has been deactivated.",
            // The longest live bridge: the run between the nearest subject
            // anchor and the state is `associated with this api key has been`
            // (7 tokens), so STATE_GAP must clear that.
            "The account associated with this API key has been permanently deactivated",
            // Status adverbs are bridgeable filler in the same family as
            // `currently` / `temporarily`; without them these fall through to
            // AuthFailure and the cron resurrects a genuinely banned account.
            "account is now suspended",
            "the account has now been deactivated",
            "the user was already revoked by an admin",
            "account has been banned",
            "This account has been banned",
            "account was banned",
            "account is banned",
            "account banned",
            "account has been suspended",
            "account is suspended",
            "account was suspended",
            "account suspended",
            "account has been revoked",
            "account was revoked",
            // Reversed order: the state precedes the subject. Forward-only
            // would let these fall through to AuthFailure and resurrect a
            // genuinely revoked/suspended account on the cron.
            "Access revoked for this account",
            "Key has been suspended for security review",
            "user deactivated by an administrator",
            "user has been deactivated",
            "user deactivated",
            "API key revoked",
            "key revoked",
            "key has been revoked",
            "key has been suspended",
        ] {
            assert!(
                is_likely_banned(403, body),
                "real account-state copy must still ban: {body}"
            );
            // The dispatcher routes the non-firecrawl vendors here too.
            for provider in ["tavily", "exa", "xai"] {
                assert!(
                    is_account_banned(provider, 403, body),
                    "{provider} must dispatch this body to the likely tier: {body}"
                );
            }
        }
    }

    /// The false positive this tier can no longer walk back. A bare marker
    /// word in a `403` body is NOT a ban: proxy `403` middleware pages and
    /// quota copy say "suspended"/"revoked" about something other than the
    /// account, and since schema 18 the likely-tier disable is permanent
    /// (the re-enable cron skips `vendor_suspended`) — a false positive now
    /// takes a healthy account out of rotation until an operator intervenes.
    #[test]
    fn likely_tier_ignores_bare_words_and_unrelated_copy() {
        for body in [
            r#"{"error":"plan suspended"}"#,
            r#"{"error":"Unauthorized"}"#,
            "The origin provider is suspended by our egress proxy",
            "your subscription will be revoked soon",
            "the target host is banned in this region",
            "Request rejected: request has been suspended by the edge",
            "This page is suspended",
            "request revoked by the gateway",
            "Your plan is suspended",
        ] {
            assert!(
                !is_likely_banned(403, body),
                "bare wording must not disable a key permanently: {body}"
            );
        }
        // Status gate unchanged: a rate limit is never a ban even with a phrase.
        assert!(!is_likely_banned(429, "account suspended"));
        assert!(!is_likely_banned(402, "account deactivated"));
    }

    /// What decides between "wide proximity" and "tokenized proximity": both
    /// a subject token AND a state token are present, and the body is STILL
    /// not an account-state claim — the words belong to different sentences.
    /// A character-distance window cannot separate these from "the account
    /// associated with this API key has been deactivated"; requiring the gap
    /// to be closed FILLER can. Each would permanently disable a healthy key
    /// under a wide window, which is the failure this rule prevents.
    #[test]
    fn likely_tier_ignores_both_words_in_unrelated_clauses() {
        for body in [
            "The api key is valid; this request has been banned",
            "Your account is active. This request was banned by our content filter.",
            "The account is in good standing; the request was suspended by our WAF",
            "Your plan is suspended. Your account remains active.",
            // No subject token at all: `key` must not be found inside another
            // word, and `banned` inside a longer one.
            "keyboard input banned by our WAF",
            "bananas are not revoked by anyone",
        ] {
            assert!(
                !is_likely_banned(403, body),
                "two words in unrelated clauses must not disable a key: {body}"
            );
        }
    }

    /// A NEGATED account state is not an account state. "not" is deliberately
    /// not bridgeable filler, so each of these breaks the walk and never
    /// reaches a ban — and this tier's disable is permanent, so a negation
    /// that reads as a ban would take a healthy key out of rotation until an
    /// operator intervened. This is real vendor prose.
    #[test]
    fn likely_tier_ignores_negated_account_states() {
        for body in [
            "your account has not been deactivated",
            "this key was not revoked",
            "your account has not been suspended — check your request format",
            "the user is not banned; rotate the api key instead",
            // `no` sits BEFORE the subject, so the walk never starts on it and
            // it cannot break a bridge from inside the gap. It is listed as
            // filler for exactly that reason: "no <subject> <state>" is a
            // denial, not an account state.
            "no account was suspended",
            "no user was banned",
            "no api key has been revoked",
            // The IN-GAP counterpart: `no` sits BETWEEN subject and state, so
            // this row is what pins it staying out of `COPULA_FILLER`. Filler
            // bridges a gap, so re-adding it would make this body a ban while
            // the three rows above still passed — which is why both the
            // pre-subject and the in-gap direction need a row.
            "this account has no revoked keys",
        ] {
            assert!(
                !is_likely_banned(403, body),
                "a negated state must not permanently disable a key: {body}"
            );
        }
    }

    /// A STATE token is MANDATORY for this tier — the disable is permanent, so
    /// a table of subject-side prefixes (which would match ANY continuation)
    /// is not an acceptable mechanism. Each body below is a subject-side
    /// prefix followed by a continuation that is NOT a ban: a payment-method
    /// failure, a content flag, a quota page, and a key-rotation notice. All
    /// four would have disabled a healthy key permanently under such a table.
    #[test]
    fn likely_tier_requires_an_actual_state_token() {
        for body in [
            "the account associated with the payment method failed verification",
            "the account associated with this request has been flagged",
            "your account has been permanently rate limited",
            "the key associated with this api key has been rotated",
        ] {
            assert!(
                !is_likely_banned(403, body),
                "a subject-side prefix with no state token must not disable a key: {body}"
            );
        }
    }

    #[test]
    fn dispatcher_routes_by_provider() {
        // firecrawl → exact tier only
        assert!(is_account_banned(
            "firecrawl",
            403,
            FIRECRAWL_BAN_BODY_FIXTURE
        ));
        assert!(!is_account_banned(
            "firecrawl",
            403,
            r#"{"error":"key revoked"}"#
        ));
        // tavily → exact + likely (both caught); exa/xai → likely tier only.
        assert!(is_account_banned(
            "tavily",
            403,
            r#"{"error":"account suspended"}"#
        ));
        assert!(is_account_banned(
            "exa",
            401,
            r#"{"detail":"user deactivated"}"#
        ));
        assert!(!is_account_banned(
            "tavily",
            403,
            r#"{"error":"Unauthorized"}"#
        ));
    }
}

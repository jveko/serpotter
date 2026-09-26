//! Pure credit-usage parsers (Tavily / Firecrawl) + shared snapshot type.
//!
//! Network fetch lives on [`crate::TavilyClient`] / [`crate::FirecrawlClient`].

use crate::ProviderError;

/// Remaining and plan limit from a vendor usage endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditSnapshot {
    pub remaining: i64,
    pub limit: i64,
}

/// A vendor usage payload we cannot read is a LOCAL refusal
/// ([`ProviderError::Unsupported`], never an `Upstream` status): fabricating
/// `remaining = 0` from a 200 with an unrecognized body would park a funded
/// key at the bottom of the pool forever, and `0` is indistinguishable from
/// "the account is broke". Callers must skip the write on this error.
fn unrecognized_usage(provider: &str) -> ProviderError {
    ProviderError::Unsupported {
        provider: provider.to_string(),
        action: "credit_usage",
        detail: "usage response carried no recognized credit fields; refusing to \
                 fabricate a snapshot"
            .into(),
    }
}

/// Numeric value at a JSON pointer, or `None` when absent/null/non-numeric.
fn num(v: &serde_json::Value, ptr: &str) -> Option<f64> {
    v.pointer(ptr).and_then(|x| x.as_f64())
}

/// Sum of the present members of an account group (missing counts as 0), or
/// `None` when NO member is a recognized number — the "this body is not a
/// usage payload" signal.
fn account_sum(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x + y),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    }
}

/// Pure: parse Tavily `GET /usage` JSON → remaining/limit.
///
/// mysearch parity: prefer account plan_limit+paygo_limit (and plan_usage+paygo_usage);
/// fall back to per-key limit/usage when account totals are zero/missing.
///
/// Recognition rule (pinned by the tests below): a body yields a snapshot
/// only if it carries a RECOGNIZED field — `key.limit`, or an account
/// `plan_limit`/`paygo_limit`, or a `remaining`-bearing field. Account limits
/// that sum to `0` are a present-but-unlimited marker, NOT a source, and a
/// `key.usage` with no `key.limit` is not one either. Anything else — an
/// error object, a renamed field, a future schema, `{account:{…0}, key:{usage:5}}`
/// — is `Err`, leaving the stored snapshot untouched. Fabricating a limit of 0
/// from such a body would park a funded key at the bottom of the pool forever,
/// and `0` is indistinguishable from "the account is broke".
pub fn parse_tavily_usage(v: &serde_json::Value) -> Result<CreditSnapshot, ProviderError> {
    let account_limit = account_sum(
        num(v, "/account/plan_limit"),
        num(v, "/account/paygo_limit"),
    );
    let key_limit = num(v, "/key/limit");
    // Account totals win when they are non-zero; the per-key numbers are the
    // documented fallback (free/unlimited account rows report zeros).
    let account_wins = account_limit.is_some_and(|l| l > 0.0);
    let limit = if account_wins {
        account_limit
    } else {
        key_limit
    };
    let Some(limit) = limit else {
        return Err(unrecognized_usage("tavily"));
    };
    let used = if account_wins {
        num(v, "/account/plan_usage").unwrap_or(0.0) + num(v, "/account/paygo_usage").unwrap_or(0.0)
    } else {
        num(v, "/key/usage").unwrap_or(0.0)
    };
    Ok(CreditSnapshot {
        remaining: (limit - used).max(0.0) as i64,
        limit: limit as i64,
    })
}

/// Pure: parse Firecrawl `GET /v2/team/credit-usage` JSON.
///
/// `remainingCredits` is MANDATORY and `planCredits` is optional — the
/// asymmetry with [`parse_tavily_usage`] is deliberate and follows from how
/// each value is obtained. Tavily's remaining is DERIVED (`limit - used`), so
/// a present-but-zero `key.limit` is itself a vendor statement worth acting on
/// and a zero there is real data. Firecrawl's remaining is a VERBATIM field: if
/// the payload omits `remainingCredits` there is no number to read, and
/// `unwrap_or(0.0)` would report a funded team as exhausted — the vendor going
/// quiet read as "0 credits left". `planCredits` only feeds the pool's score
/// (see `KEY_CREDIT_SCORE_SCALE`), so its absence costs nothing.
pub fn parse_firecrawl_usage(v: &serde_json::Value) -> Result<CreditSnapshot, ProviderError> {
    let Some(remaining) = num(v, "/data/remainingCredits") else {
        return Err(unrecognized_usage("firecrawl"));
    };
    Ok(CreditSnapshot {
        remaining: remaining as i64,
        limit: num(v, "/data/planCredits").unwrap_or(0.0) as i64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tavily_account_totals() {
        let v = serde_json::json!({
            "account": {
                "plan_limit": 1000,
                "plan_usage": 100,
                "paygo_limit": 0,
                "paygo_usage": 0
            },
            "key": { "usage": 5, "limit": 50 }
        });
        let s = parse_tavily_usage(&v).unwrap();
        assert_eq!(s.limit, 1000);
        assert_eq!(s.remaining, 900);
    }

    #[test]
    fn parse_tavily_account_plan_plus_paygo() {
        let v = serde_json::json!({
            "account": {
                "plan_limit": 500,
                "plan_usage": 50,
                "paygo_limit": 200,
                "paygo_usage": 25
            },
            "key": { "usage": 1, "limit": 10 }
        });
        let s = parse_tavily_usage(&v).unwrap();
        assert_eq!(s.limit, 700);
        assert_eq!(s.remaining, 625);
    }

    #[test]
    fn parse_tavily_key_fallback_when_account_zero() {
        let v = serde_json::json!({
            "account": {
                "plan_limit": 0,
                "plan_usage": 0,
                "paygo_limit": 0,
                "paygo_usage": 0
            },
            "key": { "usage": 5, "limit": 50 }
        });
        let s = parse_tavily_usage(&v).unwrap();
        assert_eq!(s.limit, 50);
        assert_eq!(s.remaining, 45);
    }

    #[test]
    fn parse_tavily_key_fallback_when_account_missing() {
        let v = serde_json::json!({
            "key": { "usage": 10, "limit": 100 }
        });
        let s = parse_tavily_usage(&v).unwrap();
        assert_eq!(s.limit, 100);
        assert_eq!(s.remaining, 90);
    }

    #[test]
    fn parse_tavily_remaining_clamped_at_zero() {
        let v = serde_json::json!({
            "account": {
                "plan_limit": 10,
                "plan_usage": 20,
                "paygo_limit": 0,
                "paygo_usage": 0
            }
        });
        let s = parse_tavily_usage(&v).unwrap();
        assert_eq!(s.limit, 10);
        assert_eq!(s.remaining, 0);
    }

    /// The 200-with-a-body-we-do-not-recognize case. Fabricating
    /// `remaining = 0` here parks a funded key at the bottom of the pool
    /// forever and is indistinguishable from "out of credits".
    #[test]
    fn parse_tavily_unrecognized_body_is_err_not_zero() {
        for v in [
            serde_json::json!({}),
            serde_json::json!({ "error": "unauthorized" }),
            serde_json::json!({ "account": { "planLimit": 1000 } }),
            serde_json::json!({ "key": { "usage": 5 } }),
            // Account limits present but summing to zero are an
            // "unlimited" marker, not a limit source; with no `key.limit`
            // there is nothing to compute a remaining from, so this must NOT
            // reach Ok(0) and bury a funded key at the pool floor.
            serde_json::json!({ "account": { "plan_limit": 0, "paygo_limit": 0 }, "key": { "usage": 5 } }),
        ] {
            let err = parse_tavily_usage(&v)
                .expect_err("unrecognized usage body must not yield a snapshot");
            assert!(
                matches!(
                    err,
                    ProviderError::Unsupported {
                        action: "credit_usage",
                        ..
                    }
                ),
                "must be a local refusal, not an upstream status: {err:?}"
            );
        }
    }

    /// A present-but-zero key limit IS a recognized payload (free tier), so
    /// it must not be confused with "unrecognized" and skipped.
    #[test]
    fn parse_tavily_zero_key_limit_still_parses() {
        let v = serde_json::json!({ "key": { "usage": 0, "limit": 0 } });
        let s = parse_tavily_usage(&v).expect("key.limit 0 is recognized");
        assert_eq!(s.limit, 0);
        assert_eq!(s.remaining, 0);
    }

    #[test]
    fn parse_firecrawl_remaining() {
        let v = serde_json::json!({
            "data": { "remainingCredits": 42, "planCredits": 100 }
        });
        let s = parse_firecrawl_usage(&v).unwrap();
        assert_eq!(s.remaining, 42);
        assert_eq!(s.limit, 100);
    }

    #[test]
    fn parse_firecrawl_missing_data_is_err_not_zero() {
        for v in [
            serde_json::json!({}),
            serde_json::json!({ "success": false, "error": "not found" }),
            serde_json::json!({ "data": { "totalCredits": 100 } }),
            // The limit is present and healthy, but `remainingCredits` is
            // absent: there is no number to read, and defaulting it to 0
            // would tell the pool a 500-credit team is exhausted.
            serde_json::json!({ "data": { "planCredits": 500 } }),
        ] {
            let err = parse_firecrawl_usage(&v)
                .expect_err("unrecognized usage body must not yield a snapshot");
            assert!(
                matches!(
                    err,
                    ProviderError::Unsupported {
                        action: "credit_usage",
                        ..
                    }
                ),
                "must be a local refusal, not an upstream status: {err:?}"
            );
        }
    }

    /// One recognized field is enough: a genuinely exhausted account reports
    /// `remainingCredits: 0`, which must still overwrite the stored snapshot.
    #[test]
    fn parse_firecrawl_exhausted_zero_is_written() {
        let v = serde_json::json!({ "data": { "remainingCredits": 0 } });
        let s = parse_firecrawl_usage(&v).expect("zero remaining is real data");
        assert_eq!(s.remaining, 0);
        assert_eq!(s.limit, 0);
    }
}

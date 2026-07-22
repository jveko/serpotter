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

/// Pure: parse Tavily `GET /usage` JSON → remaining/limit.
///
/// mysearch parity: prefer account plan_limit+paygo_limit (and plan_usage+paygo_usage);
/// fall back to per-key limit/usage when account totals are zero/missing.
pub fn parse_tavily_usage(v: &serde_json::Value) -> Result<CreditSnapshot, ProviderError> {
    let account = v.get("account");
    let plan_limit = account
        .and_then(|a| a.get("plan_limit"))
        .and_then(|x| x.as_f64())
        .unwrap_or(0.0);
    let paygo_limit = account
        .and_then(|a| a.get("paygo_limit"))
        .and_then(|x| x.as_f64())
        .unwrap_or(0.0);
    let key_limit = v
        .pointer("/key/limit")
        .and_then(|x| x.as_f64())
        .unwrap_or(0.0);
    let limit = if plan_limit + paygo_limit > 0.0 {
        plan_limit + paygo_limit
    } else {
        key_limit
    };
    let plan_used = account
        .and_then(|a| a.get("plan_usage"))
        .and_then(|x| x.as_f64())
        .unwrap_or(0.0);
    let paygo_used = account
        .and_then(|a| a.get("paygo_usage"))
        .and_then(|x| x.as_f64())
        .unwrap_or(0.0);
    let key_used = v
        .pointer("/key/usage")
        .and_then(|x| x.as_f64())
        .unwrap_or(0.0);
    let used = if plan_limit + paygo_limit > 0.0 {
        plan_used + paygo_used
    } else {
        key_used
    };
    Ok(CreditSnapshot {
        remaining: (limit - used).max(0.0) as i64,
        limit: limit as i64,
    })
}

/// Pure: parse Firecrawl `GET /v2/team/credit-usage` JSON.
pub fn parse_firecrawl_usage(v: &serde_json::Value) -> Result<CreditSnapshot, ProviderError> {
    let remaining = v
        .pointer("/data/remainingCredits")
        .and_then(|x| x.as_f64())
        .unwrap_or(0.0) as i64;
    let limit = v
        .pointer("/data/planCredits")
        .and_then(|x| x.as_f64())
        .unwrap_or(0.0) as i64;
    Ok(CreditSnapshot { remaining, limit })
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

    #[test]
    fn parse_tavily_empty_object_zeros() {
        let v = serde_json::json!({});
        let s = parse_tavily_usage(&v).unwrap();
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
    fn parse_firecrawl_missing_data_zeros() {
        let v = serde_json::json!({});
        let s = parse_firecrawl_usage(&v).unwrap();
        assert_eq!(s.remaining, 0);
        assert_eq!(s.limit, 0);
    }
}

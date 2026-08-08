//! Soft-merge leg error helpers for hybrid/blend.

use crate::error::SearchExecError;

/// Priority for blend all-empty: a, then b, then Verify's c, else synthetic Search.
pub fn first_blend_err(
    a: Option<SearchExecError>,
    b: Option<SearchExecError>,
    c: Option<SearchExecError>,
) -> SearchExecError {
    a.or(b)
        .or(c)
        .unwrap_or(SearchExecError::Search("blend empty".into()))
}

/// Soft-merge signal when multi-leg keeps items but a leg failed (mirrors research social_error).
/// Labels are stable wire strings (`web`/`x` for hybrid; `primary`/`secondary`/`exa` for blend).
pub fn multi_leg_errors<'a, I>(legs: I) -> Option<Vec<String>>
where
    I: IntoIterator<Item = (&'static str, Option<&'a SearchExecError>)>,
{
    let mut out = Vec::new();
    for (label, err) in legs {
        if let Some(e) = err {
            out.push(format!("{label}: {e}"));
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod blend_err_tests {
    use super::{first_blend_err, multi_leg_errors};
    use crate::SearchExecError;

    #[test]
    fn first_blend_prefers_a_then_b_then_c() {
        let a = SearchExecError::KeyBusy("a".into());
        let b = SearchExecError::NoHealthyKey("b".into());
        let c = SearchExecError::Provider("c".into());
        match first_blend_err(Some(a), Some(b), Some(c)) {
            SearchExecError::KeyBusy(m) => assert_eq!(m, "a"),
            other => panic!("expected KeyBusy from a, got {other:?}"),
        }
        match first_blend_err(
            None,
            Some(SearchExecError::KeyBusy("b".into())),
            Some(SearchExecError::Provider("c".into())),
        ) {
            SearchExecError::KeyBusy(m) => assert_eq!(m, "b"),
            other => panic!("expected KeyBusy from b, got {other:?}"),
        }
        match first_blend_err(None, None, Some(SearchExecError::NoHealthyNode("c".into()))) {
            SearchExecError::NoHealthyNode(m) => assert_eq!(m, "c"),
            other => panic!("expected NoHealthyNode from c, got {other:?}"),
        }
        match first_blend_err(None, None, None) {
            SearchExecError::Search(m) => assert_eq!(m, "blend empty"),
            other => panic!("expected synthetic blend empty, got {other:?}"),
        }
    }

    #[test]
    fn multi_leg_errors_blend_labels() {
        let p = SearchExecError::Provider("tavily down".into());
        let e = SearchExecError::KeyBusy("exa busy".into());
        let out = multi_leg_errors([
            ("primary", Some(&p)),
            ("secondary", None),
            ("exa", Some(&e)),
        ])
        .unwrap();
        assert_eq!(
            out,
            vec![
                "primary: tavily down".to_string(),
                "exa: exa busy".to_string()
            ]
        );
        assert!(
            multi_leg_errors([("primary", None), ("secondary", None), ("exa", None),]).is_none()
        );
    }
}

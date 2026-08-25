//! Research orchestration: web search + scrape + optional social leg.

use futures_util::StreamExt as _;
use serpotter_core::{SearchQuery, Sources};
use serpotter_providers::{ProviderError, ProviderSearchParams, SVC_TAVILY, SVC_XAI};

use crate::dto::{Citation, Evidence, ResearchRequest, ResearchResponse, ScrapedPage, Synthesis};
use crate::error::{ExtractError, ResearchError};
use crate::hold::KeyHold;
use crate::meta::{ExecMeta, ProductOutcome, ProgressEvent};
use crate::search::search_inner;
use crate::ProductCtx;

use super::extract_url::{extract_url, map_provider_error, structured_provider_err};
use super::helpers::{map_social_leg, scraped_page_from_extract, select_scrape_targets};

/// Bound on concurrent scrape requests in [`research_inner`]: matches the key
/// pool's `KEY_MAX_INFLIGHT` default (3), so a scrape fan-out never requests
/// more concurrent key leases than the pool grants by default — removing the
/// head-of-line thrash `join_all` produced for `scrape_top_n > 3`. The stream
/// is buffered (not unordered), so result rank order — citations — is kept.
/// `buffered(SCRAPE_CONCURRENCY)` preserves input order; the pool still gates
/// beyond this cap.
const SCRAPE_CONCURRENCY: usize = 3;

pub async fn research_inner(
    ctx: &ProductCtx,
    body: ResearchRequest,
) -> Result<ProductOutcome<ResearchResponse>, ProductOutcome<ResearchError>> {
    // B19: deep research is a different product loop (search → scrape → xAI
    // synthesis → optional refine), bounded by the same request deadline.
    if body.deep {
        return deep_research_inner(ctx, body).await;
    }

    // B1: exact-query TTL cache for STANDARD research only — deep research is
    // never cached (wall-clock loops, cost variance). Fail-open.
    let canonical = crate::cache::canonical_research(&body);
    if let Some(json) =
        crate::cache::cache_get(ctx, crate::cache::SERVICE_RESEARCH, &canonical).await
    {
        if let Ok(resp) = serde_json::from_str::<crate::dto::ResearchResponse>(&json) {
            let mut meta = ExecMeta::default();
            meta.strategy = Some("cache".into());
            meta.mark_cache_hit();
            return Ok(ProductOutcome { result: resp, meta });
        }
    }

    // B17: research_backend=tavily selects the single Tavily `/research` job
    // (synchronously polled); `deep` is a serpotter-loop flag and is ignored
    // for the tavily backend.
    if body.research_backend.as_deref() == Some("tavily") {
        return tavily_research_inner(ctx, body, &canonical).await;
    }

    let max_results = body.web_max_results.unwrap_or(5).clamp(1, 20);
    // Default scrape_top_n=2 (REST + MCP); callers may set 0–10.
    let extract_n = body.scrape_top_n.unwrap_or(2).clamp(0, 10) as usize;
    // Web leg must NOT carry X handles — Gate 3 would steal routing to xAI.
    let q = SearchQuery {
        query: body.query.clone(),
        max_results: Some(max_results),
        include_content: body.include_content.or(Some(false)),
        include_domains: body.include_domains.clone(),
        exclude_domains: body.exclude_domains.clone(),
        from_date: body.from_date.clone(),
        to_date: body.to_date.clone(),
        time_range: body.time_range.clone(),
        country: body.country.clone(),
        ..Default::default()
    };
    let search_out = match search_inner(ctx, q).await {
        Ok(o) => o,
        Err(o) => {
            return Err(ProductOutcome {
                result: ResearchError::Search(o.result),
                meta: o.meta,
            });
        }
    };
    let mut meta = search_out.meta;
    let search = search_out.result;

    // Phase honesty: the web phase is emitted AFTER the search leg returns so
    // `done` reflects the real item count against `total = web_max_results`.
    ctx.emit(&ProgressEvent::Phase {
        name: "web".into(),
        done: search.items.len() as u32,
        total: max_results,
    });

    let mut citations = Vec::new();
    for item in &search.items {
        if !item.url.is_empty() {
            citations.push(Citation {
                title: item.title.clone(),
                url: item.url.clone(),
            });
        }
    }

    // Concurrent scrapes preserve input rank order: `buffered(SCRAPE_CONCURRENCY)`
    // bounds in-flight scrapes to the key pool's default `KEY_MAX_INFLIGHT` —
    // no head-of-line thrash when scrape_top_n > 3 (the pool still gates
    // beyond this cap).
    // Social does not depend on scrape results — overlap wall-clock with scrapes.
    let include_scrape_content = body.include_content.unwrap_or(false);
    let scrape_targets = select_scrape_targets(&search.items, extract_n);

    let social_enabled = ctx.db.get_social_enabled().await.unwrap_or(true);
    let social_n = body.social_max_results.unwrap_or(0);
    let run_social = social_n > 0 && social_enabled;

    let scrape_total = scrape_targets.len() as u32;
    let scrape_fut = async {
        // D4/F15: every extract attempt is recorded in the per-leg ExecMeta
        // (with_key_proxy note_attempt, success AND failure), so the wire merge
        // below can read "attempted" providers straight from the folded meta —
        // no separate success-only provider bookkeeping (which previously made
        // the wire Evidence disagree with request_log).
        let pairs = futures_util::stream::iter(scrape_targets.into_iter().enumerate())
            .map(|(i, (url, title))| async move {
                ctx.emit(&ProgressEvent::Phase {
                    name: "scrape".into(),
                    done: i as u32 + 1,
                    total: scrape_total,
                });
                match extract_url(ctx, &url, None).await {
                    Ok(o) => {
                        let e = o.result;
                        let page = scraped_page_from_extract(
                            e.title,
                            e.url,
                            e.content,
                            include_scrape_content,
                        );
                        (page, o.meta)
                    }
                    Err(o) => (
                        ScrapedPage {
                            title: Some(title),
                            url,
                            content: None,
                            excerpt: None,
                            error: Some(o.result.to_string()),
                        },
                        o.meta,
                    ),
                }
            })
            .buffered(SCRAPE_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        let mut pages = Vec::with_capacity(pairs.len());
        let mut scrape_meta = ExecMeta::default();
        for (page, m) in pairs {
            scrape_meta.absorb(m);
            pages.push(page);
        }
        (pages, scrape_meta)
    };

    let social_fut = async {
        if !run_social {
            (
                map_social_leg(body.social_max_results, social_enabled, None),
                None,
                ExecMeta::default(),
                (None, None, None, None),
            )
        } else {
            // social leg runs last: emit its phase only when it actually starts
            // (done=1/total=1 — a single social pass, not a position marker).
            ctx.emit(&ProgressEvent::Phase {
                name: "social".into(),
                done: 1,
                total: 1,
            });
            let n = social_n.clamp(1, 10);
            // Social leg: handles + dates + relative time (not web domain filters).
            let social_q = SearchQuery {
                query: body.query.clone(),
                max_results: Some(n),
                provider: Some(SVC_XAI.into()),
                sources: Some(Sources::One("x".into())),
                include_content: Some(false),
                allowed_x_handles: body.allowed_x_handles.clone(),
                excluded_x_handles: body.excluded_x_handles.clone(),
                from_date: body.from_date.clone(),
                to_date: body.to_date.clone(),
                time_range: body.time_range.clone(),
                ..Default::default()
            };
            let allowed_handles = social_q
                .allowed_x_handles
                .as_ref()
                .map(|v| v.as_list())
                .filter(|v| !v.is_empty());
            let excluded_handles = social_q
                .excluded_x_handles
                .as_ref()
                .map(|v| v.as_list())
                .filter(|v| !v.is_empty());
            let x_sources = ["x".to_string()];
            let x_source_slice: &[String] = &x_sources;
            // Copy the query slices before the loop so the async block
            // captures Copy values (an owned SearchQuery would move on retry 1).
            let social_query = social_q.query.trim();
            let social_from = social_q.from_date.as_deref();
            let social_to = social_q.to_date.as_deref();
            let social_range = social_q.time_range.as_deref();
            let allowed = allowed_handles.as_deref();
            let excluded = excluded_handles.as_deref();
            // The ladder owns Attempt emission, the provider_attempt span, the
            // http client, hold finishing and meta.note_attempt. xAI always
            // dials direct (`direct=true`): the outbound pool is never touched.
            // Transport/429/401/ban retry the same account up to 3 attempts
            // (HEAD parity: this leg ran through run_provider's retry loop).
            let mut social_meta = ExecMeta::default();
            let mut social_err: Option<String> = None;
            let mut provider_result: Result<Vec<serpotter_core::SearchItem>, ()> = Err(());
            let mut social_usage = (None, None, None, None);
            const SOCIAL_ATTEMPTS: u32 = 3;
            'social: for attempt in 1..=SOCIAL_ATTEMPTS {
                let mut fallback_meta = ExecMeta::default();
                let outcome: Result<
                    Result<serpotter_providers::ProviderResult, serpotter_providers::ProviderError>,
                    String,
                > = crate::lease::with_key_proxy(
                    ctx,
                    SVC_XAI,
                    true,
                    attempt,
                    SOCIAL_ATTEMPTS,
                    &mut fallback_meta,
                    |e| e.to_string(), // acquire-side failure → social_error text
                    |e| crate::lease::verdict_for(SVC_XAI, e),
                    |api_key, _proxy_url, _http, _key_hold, _proxy_hold| async move {
                        let params = ProviderSearchParams {
                            query: social_query,
                            max_results: n,
                            api_key: &api_key,
                            include_content: false,
                            include_answer: true,
                            include_images: false,
                            include_raw_content: false,
                            chunks_per_source: None,
                            search_depth: None,
                            tavily_topic: None,
                            firecrawl_categories: None,
                            sources: Some(x_source_slice),
                            include_domains: None,
                            exclude_domains: None,
                            allowed_x_handles: allowed,
                            excluded_x_handles: excluded,
                            from_date: social_from,
                            to_date: social_to,
                            time_range: social_range,
                            country: None,
                            exact_match: None,
                        };
                        ctx.providers.search(SVC_XAI, params, None).await
                    },
                )
                .await;
                social_meta.absorb(fallback_meta);
                match outcome {
                    Ok(Ok(r)) => {
                        // B2: capture the successful xAI /responses usage
                        // before moving the items out of the provider result.
                        social_usage = (r.input_tokens, r.output_tokens, r.total_tokens, r.cost);
                        provider_result = Ok(r.items.clone());
                        break 'social;
                    }
                    Ok(Err(e)) => {
                        let mode = crate::lease::verdict_for(SVC_XAI, &e);
                        social_err = Some(map_provider_error(SVC_XAI, &e).to_string());
                        if matches!(
                            mode,
                            crate::lease::ReportMode::Retryable
                                | crate::lease::ReportMode::Banned
                                | crate::lease::ReportMode::AuthFailure
                        ) && attempt < SOCIAL_ATTEMPTS
                        {
                            ctx.emit(&ProgressEvent::Retry {
                                service: SVC_XAI.to_string(),
                                attempt: attempt + 1,
                                reason: social_err.clone().unwrap_or_default(),
                            });
                            continue;
                        }
                        break 'social;
                    }
                    Err(e) => {
                        // Acquire-side failure (no healthy key / busy) — no retry.
                        social_err = Some(e);
                        break 'social;
                    }
                }
            }
            // D4/F15: the social leg records the xai attempt in social_meta on
            // BOTH success and failure (with_key_proxy note_attempt), so the
            // wire merge below inherits "attempted" semantics automatically.
            (
                map_social_leg(Some(n), social_enabled, Some(provider_result)),
                social_err,
                social_meta,
                social_usage,
            )
        }
    };

    let ((scraped_pages, scrape_meta), (social_results, social_error, social_meta, social_usage)) =
        tokio::join!(scrape_fut, social_fut);
    meta.absorb(scrape_meta);
    meta.absorb(social_meta);
    // B2: fold the successful social-leg usage (xAI /responses usage) into
    // meta so request_log gets token/cost for the request.
    meta.set_usage(
        social_usage.0,
        social_usage.1,
        social_usage.2,
        social_usage.3,
    );

    // D4/F15: ONE semantics for both surfaces — first-seen ATTEMPTED providers
    // across ALL legs (web/scrape/social), identical to request_log's
    // providers_consulted (meta.providers_csv()). meta absorbs every leg's
    // note_attempt'ed vendors (failed attempts included), so the wire Evidence
    // and the request_log row for the same request can never disagree.
    let providers_consulted = if meta.providers_consulted.is_empty() {
        // Defensive: a successful web leg always records a real vendor; this
        // fallback keeps the dial label only if meta somehow has none.
        vec![search.provider_used.clone()]
    } else {
        meta.providers_consulted.clone()
    };

    let resp = ResearchResponse {
        query: body.query,
        web_results: search.items,
        social_results,
        social_error,
        scraped_pages: if scraped_pages.is_empty() {
            None
        } else {
            Some(scraped_pages)
        },
        citations: if citations.is_empty() {
            None
        } else {
            Some(citations)
        },
        evidence: Some(Evidence {
            summary: search.answer,
            providers_consulted: Some(providers_consulted),
            web_leg_errors: search.leg_errors,
        }),
        // B32: standard research never runs the synthesis loop.
        synthesis: None,
    };
    // B1: cache only successful standard-research responses (fail-open).
    if let Ok(json) = serde_json::to_string(&resp) {
        crate::cache::cache_put(ctx, crate::cache::SERVICE_RESEARCH, &canonical, &json).await;
    }
    Ok(ProductOutcome { result: resp, meta })
}

// ===========================================================================
// B19: iterative deep research (search → scrape → xAI synthesis → refine)
// ===========================================================================

/// Cap on web results per deep pass (personal-use budget).
const DEEP_MAX_WEB: u32 = 6;
/// Cap on scrape targets per deep pass.
const DEEP_MAX_SCRAPE: usize = 6;
/// Refinement only starts when at least this much budget remains — the loop
/// must NEVER run past `ctx.request_timeout` (the F10 handler deadline is the
/// outer cap; this check keeps phases from starting they cannot finish).
const DEEP_REFINE_MIN_REMAINING: std::time::Duration = std::time::Duration::from_secs(20);
/// Scraping stops when less than this much budget remains.
const DEEP_SCRAPE_MIN_REMAINING: std::time::Duration = std::time::Duration::from_secs(5);

fn time_remaining(deadline: std::time::Instant) -> std::time::Duration {
    deadline.saturating_duration_since(std::time::Instant::now())
}

/// Deep research: up to 2 iterations, each search → bounded scrape → xAI
/// synthesis. The synthesis runs only over actually-scraped content; when it
/// is unavailable (no key / error) the result falls back to a normal research
/// response with NO answer and a leg warning — never a fabricated answer.
async fn deep_research_inner(
    ctx: &ProductCtx,
    body: ResearchRequest,
) -> Result<ProductOutcome<ResearchResponse>, ProductOutcome<ResearchError>> {
    // B1-cache correctness: deep research ALREADY bypasses the research-level
    // cache, but its search legs call search_inner, which has its OWN
    // exact-query cache — and the iteration-2 search is the identical query,
    // so it would always be a cache hit and refinement could NEVER find novel
    // URLs. Run both passes through a cache-disabled ctx (cheap Arc clones).
    let search_ctx = ProductCtx {
        cache_enabled: false,
        ..ctx.clone()
    };
    let deadline = std::time::Instant::now() + ctx.request_timeout;
    let mut meta = ExecMeta::default();
    let query = body.query.clone();
    let web_n = body
        .web_max_results
        .unwrap_or(5)
        .clamp(1, 20)
        .min(DEEP_MAX_WEB);
    let scrape_n = body
        .scrape_top_n
        .unwrap_or(3)
        .clamp(1, DEEP_MAX_SCRAPE as u32) as usize;

    // Web leg must NOT carry X handles — Gate 3 would steal routing to xAI.
    let q = SearchQuery {
        query: body.query.clone(),
        max_results: Some(web_n),
        include_content: Some(false),
        include_domains: body.include_domains.clone(),
        exclude_domains: body.exclude_domains.clone(),
        from_date: body.from_date.clone(),
        to_date: body.to_date.clone(),
        time_range: body.time_range.clone(),
        country: body.country.clone(),
        ..Default::default()
    };

    // ---- Iteration 1: search → scrape → synthesize ----
    ctx.emit(&ProgressEvent::Phase {
        name: "deep-search".into(),
        done: 1,
        total: 3,
    });
    let search1 = match search_inner(&search_ctx, q).await {
        Ok(o) => {
            meta.absorb(o.meta);
            o.result
        }
        Err(o) => {
            meta.absorb(o.meta);
            return Err(ProductOutcome {
                result: ResearchError::Search(o.result),
                meta,
            });
        }
    };
    let mut web_items = search1.items.clone();
    let mut synth_errors: Vec<String> = search1.leg_errors.unwrap_or_default();
    let search_provider = search1.provider_used.clone();

    ctx.emit(&ProgressEvent::Phase {
        name: "deep-scrape".into(),
        done: 2,
        total: 3,
    });
    let mut scraped: Vec<ScrapedPage> = Vec::new();
    let targets = select_scrape_targets(&web_items, scrape_n);
    let scrape_total = targets.len() as u32;
    for (i, (url, title)) in targets.into_iter().enumerate() {
        if time_remaining(deadline) <= DEEP_SCRAPE_MIN_REMAINING {
            break;
        }
        ctx.emit(&ProgressEvent::Phase {
            name: "deep-scrape".into(),
            done: i as u32 + 1,
            total: scrape_total,
        });
        match extract_url(ctx, &url, None).await {
            Ok(o) => {
                meta.absorb(o.meta);
                let e = o.result;
                scraped.push(scraped_page_from_extract(e.title, e.url, e.content, true));
            }
            Err(o) => {
                meta.absorb(o.meta);
                scraped.push(ScrapedPage {
                    title: Some(title),
                    url,
                    content: None,
                    excerpt: None,
                    error: Some(o.result.to_string()),
                });
            }
        }
    }

    // Iteration-2 eligibility is decided BEFORE synthesis 1 runs: the refine
    // search is independent of the first synthesis, so it is pipelined
    // concurrently (tokio::join!) to overlap wall-clock. The
    // DEEP_REFINE_MIN_REMAINING guard is checked here — never start a phase
    // that cannot finish under the request deadline.
    let q2 = if time_remaining(deadline) >= DEEP_REFINE_MIN_REMAINING {
        Some(SearchQuery {
            query: body.query.clone(),
            max_results: Some(web_n),
            include_content: Some(false),
            include_domains: body.include_domains.clone(),
            exclude_domains: body.exclude_domains.clone(),
            from_date: body.from_date.clone(),
            to_date: body.to_date.clone(),
            time_range: body.time_range.clone(),
            country: body.country.clone(),
            ..Default::default()
        })
    } else {
        None
    };

    ctx.emit(&ProgressEvent::Phase {
        name: "deep-synthesize".into(),
        done: 3,
        total: 3,
    });
    // B32: the first synthesis is a structured [`Synthesis`]. Its attempts
    // are recorded into a private accumulator because the pipelined search
    // leg runs concurrently — both futures are joined, then the metas fold.
    let (mut synthesis, search2) = match q2 {
        Some(q2) => {
            let mut synth1_meta = ExecMeta::default();
            let synth_fut = synthesize(
                ctx,
                &query,
                &scraped,
                &mut synth1_meta,
                body.output_schema.as_ref(),
            );
            let search2_fut = search_inner(&search_ctx, q2);
            let (s, s2) = tokio::join!(synth_fut, search2_fut);
            meta.absorb(synth1_meta);
            (s, Some(s2))
        }
        None => (
            if has_usable_content(&scraped) {
                synthesize(
                    ctx,
                    &query,
                    &scraped,
                    &mut meta,
                    body.output_schema.as_ref(),
                )
                .await
            } else {
                None
            },
            None,
        ),
    };
    // ---- Iteration 2: adaptive refinement ----
    // The iteration-2 search leg's meta is absorbed on every path (Ok/Err)
    // so its provider attempts are never lost, even when refinement is
    // skipped. A second search failure is soft: the first pass already
    // produced a grounded result; keep it instead of failing the request.
    if let Some(s2) = search2 {
        match s2 {
            Ok(o) => {
                let s2res = o.result;
                meta.absorb(o.meta);
                for item in &s2res.items {
                    if !web_items.iter().any(|w| w.url == item.url) {
                        web_items.push(item.clone());
                    }
                }
                // Adaptive: when the second pass found NO novel URLs, the
                // scrape+resynthesize would run over the same pages — a
                // guaranteed no-op. Skip it entirely and keep answer 1; no
                // extra phase events (agents see the phases that actually
                // ran).
                let known: Vec<&str> = scraped.iter().map(|p| p.url.as_str()).collect();
                let new_targets: Vec<(String, String)> = s2res
                    .items
                    .iter()
                    .filter(|i| !i.url.is_empty() && !known.contains(&i.url.as_str()))
                    .take(scrape_n)
                    .map(|i| (i.url.clone(), i.title.clone()))
                    .collect();
                if !new_targets.is_empty() {
                    ctx.emit(&ProgressEvent::Phase {
                        name: "deep-refine".into(),
                        done: 1,
                        total: 2,
                    });
                    // Extract NEW URLs only (never re-scrape an already-scraped page).
                    let new_total = new_targets.len() as u32;
                    for (j, (url, title)) in new_targets.into_iter().enumerate() {
                        if time_remaining(deadline) <= DEEP_SCRAPE_MIN_REMAINING {
                            break;
                        }
                        ctx.emit(&ProgressEvent::Phase {
                            name: "deep-scrape".into(),
                            done: j as u32 + 1,
                            total: new_total,
                        });
                        match extract_url(ctx, &url, None).await {
                            Ok(o) => {
                                meta.absorb(o.meta);
                                let e = o.result;
                                scraped.push(scraped_page_from_extract(
                                    e.title, e.url, e.content, true,
                                ));
                            }
                            Err(o) => {
                                meta.absorb(o.meta);
                                scraped.push(ScrapedPage {
                                    title: Some(title),
                                    url,
                                    content: None,
                                    excerpt: None,
                                    error: Some(o.result.to_string()),
                                });
                            }
                        }
                    }
                    ctx.emit(&ProgressEvent::Phase {
                        name: "deep-synthesize".into(),
                        done: 2,
                        total: 2,
                    });
                    // A failed refinement synthesis must NOT discard the first
                    // pass's grounded answer — keep the best available.
                    if let Some(refined) = synthesize(
                        ctx,
                        &query,
                        &scraped,
                        &mut meta,
                        body.output_schema.as_ref(),
                    )
                    .await
                    {
                        synthesis = Some(refined);
                    } else {
                        synth_errors.push("xAI refinement synthesis unavailable".into());
                    }
                }
            }
            Err(o) => {
                meta.absorb(o.meta);
            }
        }
    }

    if synthesis.is_none() {
        synth_errors.push("xAI synthesis unavailable (no grounded answer)".into());
    }

    let citations: Vec<Citation> = web_items
        .iter()
        .filter(|i| !i.url.is_empty())
        .map(|i| Citation {
            title: i.title.clone(),
            url: i.url.clone(),
        })
        .collect();

    let providers_consulted = if meta.providers_consulted.is_empty() {
        vec![search_provider]
    } else {
        meta.providers_consulted.clone()
    };

    Ok(ProductOutcome {
        result: ResearchResponse {
            query,
            web_results: web_items,
            social_results: None,
            social_error: None,
            scraped_pages: if scraped.is_empty() {
                None
            } else {
                Some(scraped)
            },
            citations: if citations.is_empty() {
                None
            } else {
                Some(citations)
            },
            evidence: Some(Evidence {
                // B32 wire compat: evidence.summary carries the plain answer;
                // the structured synthesis rides in the new `synthesis` field.
                summary: synthesis.as_ref().map(|s| s.answer.clone()),
                providers_consulted: Some(providers_consulted),
                web_leg_errors: if synth_errors.is_empty() {
                    None
                } else {
                    Some(synth_errors)
                },
            }),
            synthesis,
        },
        meta,
    })
}

/// True when at least one page carries real scraped content (never synthesize
/// from an all-error/empty page set).
fn has_usable_content(pages: &[ScrapedPage]) -> bool {
    pages
        .iter()
        .any(|p| p.content.as_deref().is_some_and(|c| !c.trim().is_empty()))
}

/// B32 fixed JSON schema for the unstructured (`output_schema: None`) deep
/// synthesis: the answer is always a structured object with an `answer`
/// string, optional `reasoning` string, and optional `citations` integer
/// array (1-based indices into the response citation list).
/// `additionalProperties: false` keeps the model from inventing fields.
fn fixed_synthesis_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "answer": {"type": "string"},
            "reasoning": {"type": "string"},
            "citations": {"type": "array", "items": {"type": "integer"}}
        },
        "required": ["answer"],
        "additionalProperties": false
    })
}

/// Parse the model's JSON text into a [`Synthesis`] against the fixed
/// synthesis schema: `Ok` → structured (answer + optional reasoning/
/// citations, absent stays `None`); any parse failure — including a JSON
/// object without a required `answer` — falls back honestly to
/// `Synthesis { answer: raw_text, None, None }` (never fabricate
/// reasoning/citations the model did not produce).
fn synthesis_from_text(text: String) -> Synthesis {
    let Some(v) = serde_json::from_str::<serde_json::Value>(&text).ok() else {
        return Synthesis {
            answer: text,
            reasoning: None,
            citations: None,
        };
    };
    let Some(answer) = v.get("answer").and_then(|a| a.as_str()).map(str::to_string) else {
        // Required field missing → not a valid synthesis; keep the raw text.
        return Synthesis {
            answer: text,
            reasoning: None,
            citations: None,
        };
    };
    Synthesis {
        answer,
        reasoning: v
            .get("reasoning")
            .and_then(|r| r.as_str())
            .map(str::to_string),
        citations: v
            .get("citations")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_u64().map(|n| n as usize))
                    .collect::<Vec<usize>>()
            })
            .filter(|c: &Vec<usize>| !c.is_empty()),
    }
}

/// B19 synthesis: build system+user prose from the scraped pages with
/// citation markers `[n]` and ask xAI for a concise grounded answer.
/// Uses the xAI key pool like every other provider call. Returns `None`
/// (never a fabricated answer) when synthesis is unavailable: no healthy
/// key, acquire timeout, upstream error, or an empty model response.
///
/// B28/B32: [`XaiClient::complete_structured`] is always used — with the
/// user's `output_schema` when set (the user-owned schema owns the whole
/// answer: the model's raw JSON text becomes `answer`, `reasoning`/`citations`
/// stay `None`), or with the fixed [`fixed_synthesis_schema`] otherwise
/// (parsed into the structured [`Synthesis`] shape; a non-JSON answer falls
/// back to `answer = raw text`).
async fn synthesize(
    ctx: &ProductCtx,
    query: &str,
    pages: &[ScrapedPage],
    meta: &mut ExecMeta,
    output_schema: Option<&serde_json::Value>,
) -> Option<Synthesis> {
    if !has_usable_content(pages) {
        return None;
    }
    let mut src = String::new();
    for (i, p) in pages.iter().enumerate() {
        let idx = i + 1;
        let title = p.title.as_deref().unwrap_or("(untitled)");
        let content = p.content.as_deref().unwrap_or("(no content)");
        let excerpt: String = content.chars().take(1500).collect();
        src.push_str(&format!(
            "[{idx}] {title} — {url}\n{excerpt}\n\n",
            url = p.url
        ));
    }
    let system = "You are a research synthesizer. Answer the user's research question concisely, grounded ONLY in the numbered sources provided. Cite sources with [n] markers matching the numbered source list. If the sources do not contain enough information, say so explicitly instead of guessing or inventing facts.";
    let user = format!(
        "Research question: {query}\n\nSources:\n{src}\n\nProvide a concise grounded answer with [n] citations."
    );

    let lease = match ctx.keys.acquire(SVC_XAI).await {
        Ok(l) => l,
        Err(_) => return None,
    };
    let mut hold = KeyHold::new(std::sync::Arc::clone(&ctx.keys), lease.id);
    // B28: output_schema flips the call onto the user schema; without one the
    // fixed synthesis schema drives the structured answer (B32).
    let call = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let schema = output_schema
            .cloned()
            .unwrap_or_else(fixed_synthesis_schema);
        ctx.providers
            .xai
            .complete_structured(&lease.key, system, &user, None, 1200, &schema)
            .await
            .map(|c| c.text)
    });
    match call.await {
        Ok(Ok(text)) if !text.trim().is_empty() => {
            hold.finish_release().await;
            meta.note_attempt(SVC_XAI, lease.id, None, true);
            Some(match output_schema {
                // User-owned schema: the raw model text IS the answer — never
                // re-parse or wrap it (reasoning/citations stay None).
                Some(_) => Synthesis {
                    answer: text,
                    reasoning: None,
                    citations: None,
                },
                // Fixed synthesis schema: parse the structured shape; honest
                // text fallback when the model did not return JSON.
                None => synthesis_from_text(text),
            })
        }
        _ => {
            hold.finish_release().await;
            meta.note_attempt(SVC_XAI, lease.id, None, false);
            None
        }
    }
}

// ===========================================================================
// B17: Tavily `/research` backend (synchronous bounded poll)
// ===========================================================================

/// Poll interval between Tavily `/research/{id}` status checks.
const TAVILY_RESEARCH_POLL: std::time::Duration = std::time::Duration::from_secs(2);
/// Hard cap on the poll window regardless of the request deadline (the F10
/// handler deadline is the outer cap; Tavily research jobs are multi-minute).
const TAVILY_RESEARCH_POLL_CAP: std::time::Duration = std::time::Duration::from_secs(90);

/// Lease-acquire failure → [`ResearchError`] for the tavily-research
/// backend. Message strings are identical to the pre-ladder manual mapping
/// ("No healthy {s} key" / "All {s} keys busy (acquire timeout)" /
/// "No healthy outbound proxy node (REQUIRE_OUTBOUND_PROXY)").
fn map_tavily_lease_err(e: crate::lease::LeaseError) -> ResearchError {
    ResearchError::Extract(match e {
        crate::lease::LeaseError::NoHealthyKey(s) => ExtractError::NoHealthyKey(s),
        crate::lease::LeaseError::KeyBusy(s) => ExtractError::KeyBusy(s),
        crate::lease::LeaseError::NoHealthyNode(msg) => ExtractError::NoHealthyNode(msg),
        crate::lease::LeaseError::Db(e) => ExtractError::Db(e),
    })
}

/// Map a poll-loop [`ProviderError`] from the tavily-research ladder back to
/// an [`ExtractError`]. Every arm yields a neutral, vendor-text-free message
/// (verbatim bodies are only ever written to the server WARN log); `408`
/// maps to the distinct [`ExtractError::ExtractTimeout`] kind.
fn map_tavily_poll_error(e: ProviderError) -> ExtractError {
    // The poll path builds synthetic `Upstream` bodies (start/status/failed/
    // timeout wording, incl. vendor failure text). Client messages drop them,
    // so this WARN is their only durable record.
    if let ProviderError::Upstream { status, body, .. } = &e {
        tracing::warn!(
            provider = SVC_TAVILY,
            status = *status,
            body = %body,
            reason = "research_poll",
            "tavily research poll error; full detail logged"
        );
    }
    match e {
        ProviderError::Upstream { status: 408, .. } => ExtractError::ExtractTimeout(
            "tavily research job did not reach a terminal state in time".into(),
        ),
        ProviderError::Upstream { status, .. } => {
            ExtractError::Provider(format!("tavily research upstream error (status {status})"))
        }
        // The ladder's client_for failure (bad proxied URL) surfaces here.
        ProviderError::Http(e) => ExtractError::Provider(format!("tavily research client: {e}")),
        other => structured_provider_err("tavily research", other),
    }
}

/// B17: run research on the Tavily `/research` backend — start the async
/// vendor job, poll every 2s until terminal or `min(request_timeout, 90s)`
/// elapses, and map the answer + citations into the standard
/// [`ResearchResponse`] wire (evidence.summary + citations).
///
/// The B16 jobs table is deliberately NOT used (matches B18's in-request poll
/// pattern — the job handle lives only for this request). `canonical` is the
/// already-computed cache key (callers checked the cache before dispatch);
/// success responses are cached here.
///
/// The whole start+sync-poll runs under the dual-pool ladder
/// ([`crate::lease::with_key_proxy`]): the ladder owns Attempt emission, the
/// http client, hold finishing and meta.note_attempt; the closure owns the
/// vendor job and its poll loop, refreshing BOTH leases every ~2s tick so a
/// multi-minute poll never lets `lease_until` expire under an in-flight hold.
async fn tavily_research_inner(
    ctx: &ProductCtx,
    body: ResearchRequest,
    canonical: &str,
) -> Result<ProductOutcome<ResearchResponse>, ProductOutcome<ResearchError>> {
    let query = body.query.clone();
    let citation_format = body.citation_format.clone();
    let canonical = canonical.to_string();
    let mut meta = ExecMeta::default();
    let outcome = crate::lease::with_key_proxy(
        ctx,
        SVC_TAVILY,
        false, // tavily is web-only: the outbound ladder always runs.
        1,
        1,
        &mut meta,
        map_tavily_lease_err,
        // Every poll-loop failure is a plain release/release (never fail@3).
        |_| crate::lease::ReportMode::Failure,
        |api_key, _proxy_url, http, key_refresh, proxy_refresh| async move {
            let start = ctx
                .providers
                .tavily
                .research(
                    &http,
                    &api_key,
                    &query,
                    None,
                    citation_format.as_deref(),
                    None,
                )
                .await
                .map_err(|e| ProviderError::Upstream {
                    provider: SVC_TAVILY.to_string(),
                    status: 0, // synthetic: message fully formatted below
                    body: format!("tavily research start: {e}"),
                })?;
            let poll_budget = ctx.request_timeout.min(TAVILY_RESEARCH_POLL_CAP);
            let deadline = std::time::Instant::now() + poll_budget;
            loop {
                let st = match ctx
                    .providers
                    .tavily
                    .research_status(&http, &api_key, &start.id)
                    .await
                {
                    Ok(st) => st,
                    Err(e) => {
                        return Err(ProviderError::Upstream {
                            provider: SVC_TAVILY.to_string(),
                            status: 0,
                            body: structured_provider_err("tavily research status", e).to_string(),
                        });
                    }
                };
                if st.completed {
                    let citations: Vec<Citation> = st
                        .citations
                        .unwrap_or_default()
                        .into_iter()
                        .map(|c| Citation {
                            title: c.title,
                            url: c.url,
                        })
                        .collect();
                    let resp = ResearchResponse {
                        query,
                        web_results: Vec::new(),
                        social_results: None,
                        social_error: None,
                        scraped_pages: None,
                        citations: if citations.is_empty() {
                            None
                        } else {
                            Some(citations)
                        },
                        evidence: Some(Evidence {
                            summary: st.answer,
                            providers_consulted: Some(vec![SVC_TAVILY.to_string()]),
                            web_leg_errors: None,
                        }),
                        synthesis: None,
                    };
                    // B1: cache only successful responses (fail-open on DB errors).
                    if let Ok(json) = serde_json::to_string(&resp) {
                        crate::cache::cache_put(
                            ctx,
                            crate::cache::SERVICE_RESEARCH,
                            &canonical,
                            &json,
                        )
                        .await;
                    }
                    return Ok(resp);
                }
                if st.failed {
                    return Err(ProviderError::Upstream {
                        provider: SVC_TAVILY.to_string(),
                        status: 0,
                        body: format!(
                            "tavily research failed: {}",
                            st.answer.unwrap_or_else(|| "vendor job failed".into())
                        ),
                    });
                }
                // Still processing: refresh both leases every tick so the
                // multi-minute poll never expires a live hold.
                key_refresh.refresh().await;
                if let Some(ph) = &proxy_refresh {
                    ph.refresh().await;
                }
                if std::time::Instant::now() >= deadline {
                    return Err(ProviderError::Upstream {
                        provider: SVC_TAVILY.to_string(),
                        status: 408, // synthetic: mapped to ExtractTimeout below
                        body: format!(
                            "tavily research did not finish within {}s",
                            poll_budget.as_secs()
                        ),
                    });
                }
                tokio::time::sleep(TAVILY_RESEARCH_POLL).await;
            }
        },
    )
    .await;
    match outcome {
        Ok(Ok(resp)) => Ok(ProductOutcome { result: resp, meta }),
        Ok(Err(e)) => Err(ProductOutcome {
            result: ResearchError::Extract(map_tavily_poll_error(e)),
            meta,
        }),
        Err(e) => Err(ProductOutcome { result: e, meta }),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use serpotter_db::Db;
    use serpotter_keypool::KeyPool;
    use serpotter_outbound::ProxyPool;
    use serpotter_providers::{
        ExaClient, FirecrawlClient, ProviderRegistry, TavilyClient, XaiClient,
    };

    use crate::dto::{ResearchRequest, ResearchResponse, Synthesis};
    use crate::meta::{ProgressEvent, ProgressSink};
    use crate::{research_inner, ProductCtx};

    use super::{synthesis_from_text, SCRAPE_CONCURRENCY};

    /// Collects events in order for assertions.
    #[derive(Clone, Default)]
    struct VecSink(Arc<Mutex<Vec<ProgressEvent>>>);

    impl ProgressSink for VecSink {
        fn emit(&self, event: &ProgressEvent) {
            self.0.lock().unwrap().push(event.clone());
        }
    }

    async fn test_db() -> Db {
        serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate")
    }

    /// C2a ctx: the key pool allows 32 concurrent leases — far above
    /// `SCRAPE_CONCURRENCY` — so the scrape fan-out bound itself (not the
    /// pool's default cap) is what the mock observes. Tavily points at the
    /// research mock; the other providers sit at 127.0.0.1:9 (connection
    /// refused). Only a tavily key is inserted, so each scrape's extract leg
    /// falls through firecrawl (NoHealthyKey, instant) to tavily.
    fn test_ctx(
        db: Db,
        sink: VecSink,
        mock_url: String,
        xai_url: String,
        hold_ttl_secs: i64,
    ) -> ProductCtx {
        let keys = Arc::new(KeyPool::with_config(
            db.clone(),
            32,
            std::time::Duration::from_secs(5),
            hold_ttl_secs,
            100,
        ));
        let outbound = Arc::new(ProxyPool::new(db.clone()));
        let registry = ProviderRegistry::with_clients(
            TavilyClient::new(mock_url.clone()),
            FirecrawlClient::new("http://127.0.0.1:9"),
            ExaClient::new("http://127.0.0.1:9"),
            XaiClient::new(xai_url),
        );
        ProductCtx {
            db,
            keys,
            outbound,
            providers: registry,
            progress: Some(Arc::new(sink)),
            request_timeout: std::time::Duration::from_secs(120),
            cache_enabled: true,
            cache_ttl: std::time::Duration::from_secs(300),
        }
    }

    /// Raw-TCP Tavily mock: `/search` answers `search_body`; `/extract` sleeps
    /// `latency`, then echoes the requested URL in a success result row.
    /// Raw-TCP Tavily mock: each successive `/search` request answers with the
    /// next body from `search_bodies` (the LAST body repeats for any extra
    /// calls — deep research issues two searches); `/extract` sleeps
    /// `latency`, then echoes the requested URL in a success result row.
    /// Returns `(base_url, max-concurrent-/extract counter)`.
    fn spawn_research_mock_seq(
        search_bodies: Vec<String>,
        latency: std::time::Duration,
    ) -> (String, Arc<AtomicUsize>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let addr = listener.local_addr().expect("mock addr");
        let peak = Arc::new(AtomicUsize::new(0));
        let current = Arc::new(AtomicUsize::new(0));
        let peak_shared = Arc::clone(&peak);
        let current_shared = Arc::clone(&current);
        let search_idx = Arc::new(AtomicUsize::new(0));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let search_bodies = search_bodies.clone();
                let peak = Arc::clone(&peak_shared);
                let current = Arc::clone(&current_shared);
                let search_idx = Arc::clone(&search_idx);
                std::thread::spawn(move || {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    // Read headers up to the blank line.
                    loop {
                        match stream.read(&mut chunk) {
                            Ok(0) => return,
                            Ok(n) => {
                                buf.extend_from_slice(&chunk[..n]);
                                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                    break;
                                }
                            }
                            Err(_) => return,
                        }
                    }
                    let head = String::from_utf8_lossy(&buf).into_owned();
                    let header_end = head.find("\r\n\r\n").unwrap_or(buf.len());
                    let content_length: usize = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    // Read the body until content-length is satisfied.
                    let body_end = header_end + 4 + content_length;
                    while buf.len() < body_end {
                        match stream.read(&mut chunk) {
                            Ok(0) => break,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            Err(_) => break,
                        }
                    }
                    let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let body =
                        String::from_utf8_lossy(&buf[header_end + 4..body_end.min(buf.len())])
                            .to_string();
                    let resp = if path == "/search" {
                        let i = search_idx.fetch_add(1, Ordering::SeqCst);
                        let search_body = search_bodies
                            .get(i)
                            .cloned()
                            .unwrap_or_else(|| search_bodies.last().cloned().unwrap_or_default());
                        format!(
                            "HTTP/1.1 200 Mock\r\ncontent-length: {}\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{}",
                            search_body.len(),
                            search_body
                        )
                    } else {
                        // /extract: count in-flight, delay, echo the URL. The
                        // peak counter is never decremented (the current count
                        // would drag the recorded max back down).
                        let in_flight = current.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(in_flight, Ordering::SeqCst);
                        std::thread::sleep(latency);
                        let requested = serde_json::from_str::<serde_json::Value>(&body)
                            .ok()
                            .and_then(|v| v["urls"][0].as_str().map(|s| s.to_string()))
                            .unwrap_or_default();
                        let json = serde_json::json!({
                            "results": [{
                                "url": requested,
                                "raw_content": format!("# body for {requested}"),
                            }]
                        })
                        .to_string();
                        current.fetch_sub(1, Ordering::SeqCst);
                        format!(
                            "HTTP/1.1 200 Mock\r\ncontent-length: {}\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{}",
                            json.len(),
                            json
                        )
                    };
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.flush();
                });
            }
        });
        (format!("http://{addr}"), peak)
    }

    /// Convenience wrapper: every `/search` answers with the same body.
    fn spawn_research_mock(
        search_body: String,
        latency: std::time::Duration,
    ) -> (String, Arc<AtomicUsize>) {
        spawn_research_mock_seq(vec![search_body], latency)
    }

    /// Raw-TCP xAI `/responses` mock: each request consumes the next
    /// `output_text` from `texts` (the LAST repeats) — enough for
    /// `XaiClient::complete_structured`, which parses `output_text` and
    /// tries JSON. Returns `(base_url, request-hit counter)` so tests can
    /// count synthesis calls at the wire level.
    fn spawn_xai_mock_seq(texts: Vec<String>) -> (String, Arc<AtomicUsize>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let addr = listener.local_addr().expect("mock addr");
        let idx = Arc::new(AtomicUsize::new(0));
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_for_thread = Arc::clone(&hits);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let texts = texts.clone();
                let idx = Arc::clone(&idx);
                let hits = Arc::clone(&hits_for_thread);
                std::thread::spawn(move || {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        match stream.read(&mut chunk) {
                            Ok(0) => return,
                            Ok(n) => {
                                buf.extend_from_slice(&chunk[..n]);
                                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                    break;
                                }
                            }
                            Err(_) => return,
                        }
                    }
                    let head = String::from_utf8_lossy(&buf).into_owned();
                    let header_end = head.find("\r\n\r\n").unwrap_or(buf.len());
                    let content_length: usize = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse().ok())
                        })
                        .unwrap_or(0);
                    let body_end = header_end + 4 + content_length;
                    while buf.len() < body_end {
                        match stream.read(&mut chunk) {
                            Ok(0) => break,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            Err(_) => break,
                        }
                    }
                    hits.fetch_add(1, Ordering::SeqCst);
                    let i = idx.fetch_add(1, Ordering::SeqCst);
                    let text = texts
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| texts.last().cloned().unwrap_or_default());
                    let json = serde_json::json!({ "output_text": text }).to_string();
                    let resp = format!(
                        "HTTP/1.1 200 Mock\r\ncontent-length: {}\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{}",
                        json.len(),
                        json
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.flush();
                });
            }
        });
        (format!("http://{addr}"), hits)
    }

    /// Raw-TCP Tavily `/research` job mock: `POST /research` starts a job
    /// (`{"request_id":"job-1"}`); each `GET /research/job-1` consumes the
    /// next status from `seq` (the last repeats): `"pending"` →
    /// `{"status":"pending"}`, `"completed"` → `{"status":"completed",
    /// "content":"the answer","sources":[...]}`, `"failed"` →
    /// `{"status":"failed","content":"boom"}`.
    fn spawn_tavily_research_job_mock(seq: Vec<&'static str>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let addr = listener.local_addr().expect("mock addr");
        let idx = Arc::new(AtomicUsize::new(0));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let seq = seq.clone();
                let idx = Arc::clone(&idx);
                std::thread::spawn(move || {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        match stream.read(&mut chunk) {
                            Ok(0) => return,
                            Ok(n) => {
                                buf.extend_from_slice(&chunk[..n]);
                                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                    break;
                                }
                            }
                            Err(_) => return,
                        }
                    }
                    let head = String::from_utf8_lossy(&buf).into_owned();
                    let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let json = if path == "/research" {
                        serde_json::json!({ "request_id": "job-1", "status": "pending" })
                    } else {
                        let i = idx.fetch_add(1, Ordering::SeqCst);
                        match seq
                            .get(i)
                            .copied()
                            .unwrap_or_else(|| *seq.last().unwrap_or(&"pending"))
                        {
                            "completed" => serde_json::json!({
                                "status": "completed",
                                "content": "the answer",
                                "sources": [{
                                    "title": "S",
                                    "url": "https://s.example/",
                                }],
                            }),
                            "failed" => serde_json::json!({
                                "status": "failed",
                                "content": "boom",
                            }),
                            _ => serde_json::json!({ "status": "pending" }),
                        }
                    };
                    let resp = format!(
                        "HTTP/1.1 200 Mock\r\ncontent-length: {}\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{}",
                        json.to_string().len(),
                        json
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.flush();
                });
            }
        });
        format!("http://{addr}")
    }

    /// Canned Tavily `/search` success body with `n` distinct public URLs
    /// (passes the extract SSRF gate: hostname hosts, no localhost/IPs).
    fn search_body(n: usize) -> String {
        let results: Vec<serde_json::Value> = (1..=n)
            .map(|i| {
                serde_json::json!({
                    "title": format!("Result {i}"),
                    "url": format!("https://r{i}.example/"),
                    "content": format!("snippet {i}"),
                    "score": 0.9,
                })
            })
            .collect();
        serde_json::json!({ "results": results }).to_string()
    }

    /// C2a: the scrape fan-out is bounded by `SCRAPE_CONCURRENCY` (3) even
    /// though the key pool allows 32 concurrent leases, and `buffered` keeps
    /// rank order — scraped pages come back in search-result order.
    #[tokio::test]
    async fn research_scrape_concurrency_bounded_and_rank_order_preserved() {
        let db = test_db().await;
        db.insert_api_key("tavily", "tvly-c2a-scrape")
            .await
            .unwrap();
        let (mock, max_inflight) =
            spawn_research_mock(search_body(10), std::time::Duration::from_millis(100));
        let sink = VecSink::default();
        let ctx = test_ctx(
            db,
            sink.clone(),
            mock,
            "http://127.0.0.1:9".into(),
            serpotter_db::KEY_HOLD_TTL_SECS,
        );
        let body = ResearchRequest {
            query: "hello".into(),
            web_max_results: Some(10),
            scrape_top_n: Some(10),
            social_max_results: Some(0),
            include_content: Some(false),
            ..Default::default()
        };
        let out = research_inner(&ctx, body)
            .await
            .expect("research succeeds against the mock");
        let pages = out.result.scraped_pages.expect("10 scraped pages");
        assert_eq!(pages.len(), 10);
        // Rank order: buffered preserves input order (URLs in result order).
        for (i, page) in pages.iter().enumerate() {
            assert_eq!(page.url, format!("https://r{}.example/", i + 1), "page {i}");
            assert!(
                page.error.is_none(),
                "page {i} extracted ok: {:?}",
                page.error
            );
        }
        // Concurrency bound: the mock never saw more than SCRAPE_CONCURRENCY
        // in-flight /extract requests (the pool allows 32; the bound is the
        // buffered fan-out).
        assert_eq!(
            max_inflight.load(Ordering::SeqCst),
            SCRAPE_CONCURRENCY,
            "scrape fan-out must never exceed the bound"
        );
        // Per-scrape phases emitted ascending 1..=10 (rank order on the wire).
        let events = sink.0.lock().unwrap().clone();
        let scrape_done: Vec<u32> = events
            .iter()
            .filter_map(|e| match e {
                ProgressEvent::Phase { name, done, .. } if name == "scrape" => Some(*done),
                _ => None,
            })
            .collect();
        assert_eq!(scrape_done, (1..=10).collect::<Vec<u32>>());
    }

    /// C2a: the web phase reports the REAL returned item count (`done`) against
    /// `web_max_results` (`total`) and is emitted only AFTER the search leg
    /// ran — never a fake `done: 1` before the search. Multi-item version
    /// (3 items returned vs total 5 requested).
    #[tokio::test]
    async fn research_web_phase_after_search_with_real_counts() {
        let db = test_db().await;
        db.insert_api_key("tavily", "tvly-c2a-webphase")
            .await
            .unwrap();
        let (mock, _) = spawn_research_mock(search_body(3), std::time::Duration::ZERO);
        let sink = VecSink::default();
        let ctx = test_ctx(
            db,
            sink.clone(),
            mock,
            "http://127.0.0.1:9".into(),
            serpotter_db::KEY_HOLD_TTL_SECS,
        );
        let body = ResearchRequest {
            query: "hello".into(),
            web_max_results: Some(5),
            scrape_top_n: Some(0),
            social_max_results: Some(0),
            ..Default::default()
        };
        let _ = research_inner(&ctx, body).await;
        let events = sink.0.lock().unwrap().clone();

        // The FIRST phase event is the web phase with the real counts.
        let web_pos = events
            .iter()
            .position(|e| matches!(e, ProgressEvent::Phase { name, .. } if name == "web"))
            .expect("a web phase is emitted");
        assert_eq!(
            events[web_pos],
            ProgressEvent::Phase {
                name: "web".into(),
                done: 3,
                total: 5,
            }
        );
        // No other phases exist (no scrapes, social skipped)…
        let phase_count = events
            .iter()
            .filter(|e| matches!(e, ProgressEvent::Phase { .. }))
            .count();
        assert_eq!(phase_count, 1, "only the web phase: {events:?}");
        // …and it appears AFTER the search leg actually ran (an Attempt
        // precedes it) — not a pre-search marker.
        let first_attempt = events
            .iter()
            .position(|e| matches!(e, ProgressEvent::Attempt { .. }));
        assert!(
            first_attempt.is_some_and(|a| a < web_pos),
            "web phase must follow the search attempt: {events:?}"
        );
    }

    /// B32: fixed-schema parse — valid JSON yields answer + reasoning +
    /// citations; non-JSON (and JSON missing the required `answer`) falls
    /// back honestly to the raw text with no fabricated fields; an empty
    /// citations array is omitted.
    #[test]
    fn synthesis_fixed_schema_parse_happy_and_fallback() {
        // Valid: answer + reasoning + citations extracted.
        let s = synthesis_from_text(
            r#"{"answer":"42","reasoning":"because","citations":[1,3]}"#.into(),
        );
        assert_eq!(s.answer, "42");
        assert_eq!(s.reasoning.as_deref(), Some("because"));
        assert_eq!(s.citations.as_deref(), Some(&[1usize, 3][..]));
        // Answer-only JSON: optionals stay None.
        let s = synthesis_from_text(r#"{"answer":"only"}"#.into());
        assert_eq!(s.answer, "only");
        assert_eq!(s.reasoning, None);
        assert_eq!(s.citations, None);
        // Non-JSON: honest text fallback (never fabricate).
        let s = synthesis_from_text("plain text answer".into());
        assert_eq!(s.answer, "plain text answer");
        assert_eq!(s.reasoning, None);
        assert_eq!(s.citations, None);
        // JSON without the required `answer`: same honest fallback.
        let s = synthesis_from_text(r#"{"reasoning":"no answer"}"#.into());
        assert_eq!(s.answer, r#"{"reasoning":"no answer"}"#);
        assert_eq!(s.reasoning, None);
        assert_eq!(s.citations, None);
        // Empty citations array is omitted (skip_serializing_if parity).
        let s = synthesis_from_text(r#"{"answer":"a","citations":[]}"#.into());
        assert_eq!(s.citations, None);
    }

    /// B32: ResearchResponse.synthesis is camelCase on the wire, optional
    /// fields skip when None, and the field round-trips from an absent key.
    #[test]
    fn research_response_synthesis_serde_camel_case_and_skipped_when_none() {
        let with = ResearchResponse {
            query: "q".into(),
            web_results: vec![],
            social_results: None,
            social_error: None,
            scraped_pages: None,
            citations: None,
            evidence: None,
            synthesis: Some(Synthesis {
                answer: "a".into(),
                reasoning: Some("r".into()),
                citations: Some(vec![1, 2]),
            }),
        };
        let v = serde_json::to_value(&with).unwrap();
        assert_eq!(v["synthesis"]["answer"], "a");
        assert_eq!(v["synthesis"]["reasoning"], "r");
        assert_eq!(v["synthesis"]["citations"], serde_json::json!([1, 2]));
        // Round-trip: present synthesis survives serialization.
        let back: ResearchResponse = serde_json::from_value(v).unwrap();
        assert_eq!(
            back.synthesis.as_ref().map(|s| s.answer.as_str()),
            Some("a")
        );
        // Absent synthesis is omitted entirely (additive wire field).
        let without = ResearchResponse {
            synthesis: None,
            ..with
        };
        let v = serde_json::to_value(&without).unwrap();
        assert!(
            v.get("synthesis").is_none(),
            "None synthesis must be omitted"
        );
    }

    /// B32 deep loop: when the iteration-2 search finds NO novel URLs the
    /// scrape+resynthesize is skipped entirely — exactly ONE xAI synthesis
    /// attempt, no `deep-refine` phase, and the first answer is kept.
    #[tokio::test]
    async fn deep_research_skips_refine_when_no_novel_urls() {
        let db = test_db().await;
        db.insert_api_key("tavily", "tvly-deep-skip").await.unwrap();
        db.insert_api_key("xai", "xai-deep-skip").await.unwrap();
        // Both search passes return the SAME 2 URLs → no novel targets.
        let (mock, _) = spawn_research_mock(search_body(2), std::time::Duration::ZERO);
        let (xai, xai_hits) = spawn_xai_mock_seq(vec![
            r#"{"answer":"grounded answer","reasoning":"r","citations":[1,2]}"#.into(),
        ]);
        let sink = VecSink::default();
        let ctx = test_ctx(db, sink.clone(), mock, xai, serpotter_db::KEY_HOLD_TTL_SECS);
        let body = ResearchRequest {
            query: "q".into(),
            deep: true,
            web_max_results: Some(2),
            scrape_top_n: Some(2),
            social_max_results: Some(0),
            ..Default::default()
        };
        let out = research_inner(&ctx, body).await.expect("deep research ok");
        let resp = out.result;
        // Structured synthesis present; evidence.summary mirrors the answer.
        let syn = resp.synthesis.clone().expect("B32 synthesis");
        assert_eq!(syn.answer, "grounded answer");
        assert_eq!(syn.reasoning.as_deref(), Some("r"));
        assert_eq!(syn.citations.as_deref(), Some(&[1usize, 2][..]));
        assert_eq!(
            resp.evidence.as_ref().and_then(|e| e.summary.as_deref()),
            Some("grounded answer"),
            "evidence.summary must stay the plain answer (wire compat)"
        );
        let events = sink.0.lock().unwrap().clone();
        // Exactly ONE synthesis call on the wire (refinement skipped).
        assert_eq!(
            xai_hits.load(Ordering::SeqCst),
            1,
            "refinement must be skipped, one synthesis: {events:?}"
        );
        // Only the first deep-synthesize phase; no deep-refine phase.
        let synth_phases: Vec<(u32, u32)> = events
            .iter()
            .filter_map(|e| match e {
                ProgressEvent::Phase { name, done, total } if name == "deep-synthesize" => {
                    Some((*done, *total))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            synth_phases,
            vec![(3, 3)],
            "one synthesis phase: {events:?}"
        );
        let refine_phases = events
            .iter()
            .filter(|e| matches!(e, ProgressEvent::Phase { name, .. } if name == "deep-refine"))
            .count();
        assert_eq!(refine_phases, 0, "no deep-refine phase: {events:?}");
    }

    /// B32 deep loop pipeline: the iteration-2 SEARCH runs concurrently with
    /// the iteration-1 SYNTHESIS; when it finds a NOVEL url the refine phase
    /// proceeds and its synthesis REPLACES the first answer (keep-first only
    /// on success). Phase events stay honest (deep-synthesize 3/3 precedes
    /// deep-refine; the refine synthesis is 2/2 and last).
    #[tokio::test]
    async fn deep_research_pipelines_refine_search_with_first_synthesis() {
        let db = test_db().await;
        db.insert_api_key("tavily", "tvly-deep-pipe").await.unwrap();
        db.insert_api_key("xai", "xai-deep-pipe").await.unwrap();
        // Search 1 = 2 results; search 2 = 3 results (r3 is NOVEL → refine).
        let (mock, _) = spawn_research_mock_seq(
            vec![search_body(2), search_body(3)],
            std::time::Duration::ZERO,
        );
        let (xai, xai_hits) = spawn_xai_mock_seq(vec![
            r#"{"answer":"first answer"}"#.into(),
            r#"{"answer":"refined answer","citations":[3]}"#.into(),
        ]);
        let sink = VecSink::default();
        let ctx = test_ctx(db, sink.clone(), mock, xai, serpotter_db::KEY_HOLD_TTL_SECS);
        let body = ResearchRequest {
            query: "q".into(),
            deep: true,
            web_max_results: Some(2),
            scrape_top_n: Some(2),
            social_max_results: Some(0),
            ..Default::default()
        };
        let out = research_inner(&ctx, body).await.expect("deep research ok");
        let resp = out.result;
        // The refine synthesis replaced the first-pass answer.
        let syn = resp.synthesis.expect("B32 synthesis");
        assert_eq!(syn.answer, "refined answer");
        assert_eq!(syn.citations.as_deref(), Some(&[3usize][..]));
        // Both searches merged into web_results (r3 appended, no dups).
        let urls: Vec<&str> = resp.web_results.iter().map(|i| i.url.as_str()).collect();
        assert_eq!(
            urls,
            vec![
                "https://r1.example/",
                "https://r2.example/",
                "https://r3.example/"
            ]
        );
        // Phase ORDER is honest: search → scrape → synthesize(3/3) → refine
        // → scrape(new) → synthesize(2/2).
        let events = sink.0.lock().unwrap().clone();
        let names: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                ProgressEvent::Phase { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            names,
            vec![
                "deep-search",
                "deep-scrape",
                "deep-scrape",
                "deep-scrape",
                "deep-synthesize",
                "deep-refine",
                "deep-scrape",
                "deep-synthesize",
            ],
            "phase order must stay honest under the pipeline: {names:?}"
        );
        let synth_phases: Vec<(u32, u32)> = events
            .iter()
            .filter_map(|e| match e {
                ProgressEvent::Phase { name, done, total } if name == "deep-synthesize" => {
                    Some((*done, *total))
                }
                _ => None,
            })
            .collect();
        assert_eq!(synth_phases, vec![(3, 3), (2, 2)]);
        // Two synthesis calls on the wire: first + refine.
        assert_eq!(
            xai_hits.load(Ordering::SeqCst),
            2,
            "first + refine synthesis: {events:?}"
        );
    }

    /// B17 ladder: the tavily `/research` poll refreshes the key lease every
    /// ~2s tick, so a long poll never lets `lease_until` expire mid-hold.
    /// With a 2s hold TTL and a ~4s poll, an un-refreshed lease (stamped only
    /// at acquire) would sit in the past; the recorder observes lease_until
    /// advancing after the per-tick refresh.
    #[tokio::test]
    async fn tavily_research_poll_refreshes_key_lease_each_tick() {
        let db = test_db().await;
        let key = db
            .insert_api_key("tavily", "tvly-research-poll")
            .await
            .unwrap();
        // Two pending ticks (~2s each) then completion → ~4s of polling.
        let mock = spawn_tavily_research_job_mock(vec!["pending", "pending", "completed"]);
        let sink = VecSink::default();
        // Hold TTL of 2s — shorter than the poll, so only the per-tick
        // refresh keeps the lease alive (observable via lease_until).
        let ctx = test_ctx(
            db.clone(),
            sink.clone(),
            mock,
            "http://127.0.0.1:9".into(),
            2,
        );
        let body = ResearchRequest {
            query: "q".into(),
            research_backend: Some("tavily".into()),
            ..Default::default()
        };
        let recorder = tokio::spawn({
            let db = db.clone();
            async move {
                let mut records: Vec<String> = Vec::new();
                for _ in 0..60 {
                    if let Ok(Some(admin)) = db.get_api_key_admin(key.id).await {
                        if let Some(lu) = admin.lease_until {
                            if records.last().map(|l| l != &lu).unwrap_or(true) {
                                records.push(lu);
                            }
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                records
            }
        });
        let out = research_inner(&ctx, body)
            .await
            .expect("tavily research ok");
        let records = recorder.await.expect("recorder finished");
        assert_eq!(
            out.result
                .evidence
                .as_ref()
                .and_then(|e| e.summary.as_deref()),
            Some("the answer"),
            "completed poll returns the vendor answer"
        );
        // The acquire stamp plus at least one per-tick refresh → the last
        // recorded lease_until is strictly newer than the first.
        assert!(
            records.len() >= 2,
            "lease_until must be re-stamped during the poll: {records:?}"
        );
        assert!(
            records
                .last()
                .zip(records.first())
                .is_some_and(|(l, f)| l > f),
            "lease_until must advance across poll ticks: {records:?}"
        );
    }
}

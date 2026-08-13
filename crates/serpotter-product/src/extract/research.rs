//! Research orchestration: web search + scrape + optional social leg.

use serpotter_core::{SearchQuery, Sources};
use serpotter_keypool::KeyPoolError;
use serpotter_providers::{ProviderSearchParams, SVC_TAVILY, SVC_XAI};

use crate::dto::{Citation, Evidence, ResearchRequest, ResearchResponse, ScrapedPage};
use crate::error::{ExtractError, ResearchError};
use crate::hold::{KeyHold, ProxyHold};
use crate::meta::{ExecMeta, ProductOutcome, ProgressEvent};
use crate::search::search_inner;
use crate::ProductCtx;

use super::extract_url::{extract_url, map_provider_error, structured_provider_err};
use super::helpers::{map_social_leg, scraped_page_from_extract, select_scrape_targets};

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

    // Concurrent scrapes preserve input rank order via join_all.
    // Cap is extract_n ≤ 10; can thrash KEY_MAX_INFLIGHT when scrape_top_n > 3 — acceptable for personal-use.
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
        let pairs = futures_util::future::join_all(scrape_targets.into_iter().enumerate().map(
            |(i, (url, title))| async move {
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
            },
        ))
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
                    |api_key, _proxy_url, _http| async move {
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
    let search1 = match search_inner(ctx, q).await {
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

    ctx.emit(&ProgressEvent::Phase {
        name: "deep-synthesize".into(),
        done: 3,
        total: 3,
    });
    let mut answer = if has_usable_content(&scraped) {
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
    };
    if answer.is_none() {
        synth_errors.push("xAI synthesis unavailable (no grounded answer)".into());
    }

    // ---- Iteration 2: refinement (only when budget remains AND the first
    // synthesis succeeded — no point refining a broken loop) ----
    if answer.is_some() && time_remaining(deadline) >= DEEP_REFINE_MIN_REMAINING {
        ctx.emit(&ProgressEvent::Phase {
            name: "deep-refine".into(),
            done: 1,
            total: 2,
        });
        let q2 = SearchQuery {
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
        // A second search failure is soft: the first pass already produced a
        // grounded result; keep it instead of failing the whole request.
        if let Ok(o) = search_inner(ctx, q2).await {
            let s2 = o.result;
            meta.absorb(o.meta);
            for item in &s2.items {
                if !web_items.iter().any(|w| w.url == item.url) {
                    web_items.push(item.clone());
                }
            }
            // Extract NEW URLs only (never re-scrape an already-scraped page).
            let known: Vec<&str> = scraped.iter().map(|p| p.url.as_str()).collect();
            let new_targets: Vec<(String, String)> = s2
                .items
                .iter()
                .filter(|i| !i.url.is_empty() && !known.contains(&i.url.as_str()))
                .take(scrape_n)
                .map(|i| (i.url.clone(), i.title.clone()))
                .collect();
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
            ctx.emit(&ProgressEvent::Phase {
                name: "deep-synthesize".into(),
                done: 2,
                total: 2,
            });
            // A failed refinement synthesis must NOT discard the first pass's
            // grounded answer — keep the best available synthesis.
            if let Some(refined) = synthesize(
                ctx,
                &query,
                &scraped,
                &mut meta,
                body.output_schema.as_ref(),
            )
            .await
            {
                answer = Some(refined);
            } else {
                synth_errors.push("xAI refinement synthesis unavailable".into());
            }
        }
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
                summary: answer,
                providers_consulted: Some(providers_consulted),
                web_leg_errors: if synth_errors.is_empty() {
                    None
                } else {
                    Some(synth_errors)
                },
            }),
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

/// B19 synthesis: build system+user prose from the scraped pages with
/// citation markers `[n]` and ask xAI for a concise grounded answer via
/// [`XaiClient::complete`]. Uses the xAI key pool like every other provider
/// call. Returns `None` (never a fabricated answer) when synthesis is
/// unavailable: no healthy key, acquire timeout, upstream error, or an empty
/// model response.
///
/// B28: when `output_schema` is set, [`XaiClient::complete_structured`] is
/// used instead — the JSON-schema instruction is appended to the system
/// prompt and the answer is the model's JSON text (best-effort; a non-JSON
/// answer is still surfaced as text).
async fn synthesize(
    ctx: &ProductCtx,
    query: &str,
    pages: &[ScrapedPage],
    meta: &mut ExecMeta,
    output_schema: Option<&serde_json::Value>,
) -> Option<String> {
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
    // B28: output_schema flips the call onto complete_structured (schema
    // instruction appended to the system prompt; answer parsed as JSON).
    let call = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        match output_schema {
            Some(schema) => ctx
                .providers
                .xai
                .complete_structured(&lease.key, system, &user, None, 1200, schema)
                .await
                .map(|c| c.text),
            None => {
                ctx.providers
                    .xai
                    .complete(&lease.key, system, &user, None, 1200)
                    .await
            }
        }
    });
    match call.await {
        Ok(Ok(text)) if !text.trim().is_empty() => {
            hold.finish_release().await;
            meta.note_attempt(SVC_XAI, lease.id, None, true);
            Some(text)
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

/// B17: run research on the Tavily `/research` backend — start the async
/// vendor job, poll every 2s until terminal or `min(request_timeout, 90s)`
/// elapses, and map the answer + citations into the standard
/// [`ResearchResponse`] wire (evidence.summary + citations).
///
/// The B16 jobs table is deliberately NOT used (matches B18's in-request poll
/// pattern — the job handle lives only for this request). `canonical` is the
/// already-computed cache key (callers checked the cache before dispatch);
/// success responses are cached here.
async fn tavily_research_inner(
    ctx: &ProductCtx,
    body: ResearchRequest,
    canonical: &str,
) -> Result<ProductOutcome<ResearchResponse>, ProductOutcome<ResearchError>> {
    let query = body.query.clone();
    let mut meta = ExecMeta::default();
    ctx.emit(&ProgressEvent::Attempt {
        service: SVC_TAVILY.to_string(),
        attempt: 1,
        max: 1,
    });
    let lease = match ctx.keys.acquire(SVC_TAVILY).await {
        Ok(k) => k,
        Err(KeyPoolError::NoHealthyKey(s)) => {
            return Err(ProductOutcome {
                result: ResearchError::Extract(ExtractError::NoHealthyKey(format!(
                    "No healthy {s} key"
                ))),
                meta,
            });
        }
        Err(KeyPoolError::AcquireTimeout(s)) => {
            return Err(ProductOutcome {
                result: ResearchError::Extract(ExtractError::KeyBusy(format!(
                    "All {s} keys busy (acquire timeout)"
                ))),
                meta,
            });
        }
        Err(KeyPoolError::Db(e)) => {
            return Err(ProductOutcome {
                result: ResearchError::Extract(ExtractError::Db(e)),
                meta,
            });
        }
    };
    let mut key_hold = KeyHold::new(std::sync::Arc::clone(&ctx.keys), lease.id);
    let proxy = match ctx.outbound.acquire().await {
        Ok(None) if ctx.outbound.require_proxy() => {
            key_hold.finish_release().await;
            return Err(ProductOutcome {
                result: ResearchError::Extract(ExtractError::NoHealthyNode(
                    "No healthy outbound proxy node (REQUIRE_OUTBOUND_PROXY)".into(),
                )),
                meta,
            });
        }
        Ok(p) => p,
        Err(serpotter_outbound::ProxyPoolError::Db(e)) => {
            key_hold.finish_release().await;
            return Err(ProductOutcome {
                result: ResearchError::Extract(ExtractError::Db(e)),
                meta,
            });
        }
    };
    let mut proxy_hold = proxy
        .as_ref()
        .map(|p| ProxyHold::new(std::sync::Arc::clone(&ctx.outbound), p.clone()));
    let node_id = proxy_hold.as_ref().map(|h| h.node_id());
    let _key_id = key_hold.key_id();
    let proxy_url = proxy.as_ref().map(|p| p.url.as_str());
    let http = match ctx.providers.client_for(proxy_url) {
        Ok(h) => h,
        Err(e) => {
            key_hold.finish_release().await;
            if let Some(h) = proxy_hold.as_mut() {
                h.finish_release().await;
            }
            meta.note_attempt(SVC_TAVILY, lease.id, node_id, false);
            return Err(ProductOutcome {
                result: ResearchError::Extract(ExtractError::Provider(format!(
                    "tavily research client: {e}"
                ))),
                meta,
            });
        }
    };

    let start = match ctx
        .providers
        .tavily
        .research(
            &http,
            &lease.key,
            &query,
            None,
            body.citation_format.as_deref(),
            None,
        )
        .await
    {
        Ok(job) => job,
        Err(e) => {
            key_hold.finish_release().await;
            if let Some(h) = proxy_hold.as_mut() {
                h.finish_release().await;
            }
            meta.note_attempt(SVC_TAVILY, lease.id, node_id, false);
            return Err(ProductOutcome {
                result: ResearchError::Extract(ExtractError::Provider(format!(
                    "tavily research start: {e}"
                ))),
                meta,
            });
        }
    };

    let poll_budget = ctx.request_timeout.min(TAVILY_RESEARCH_POLL_CAP);
    let deadline = std::time::Instant::now() + poll_budget;
    loop {
        match ctx
            .providers
            .tavily
            .research_status(&http, &lease.key, &start.id)
            .await
        {
            Ok(st) if st.completed => {
                key_hold.finish_success().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_success().await;
                }
                meta.note_attempt(SVC_TAVILY, lease.id, node_id, true);
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
                };
                // B1: cache only successful responses (fail-open on DB errors).
                if let Ok(json) = serde_json::to_string(&resp) {
                    crate::cache::cache_put(ctx, crate::cache::SERVICE_RESEARCH, canonical, &json)
                        .await;
                }
                return Ok(ProductOutcome { result: resp, meta });
            }
            Ok(st) if st.failed => {
                key_hold.finish_release().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_release().await;
                }
                meta.note_attempt(SVC_TAVILY, lease.id, node_id, false);
                return Err(ProductOutcome {
                    result: ResearchError::Extract(ExtractError::Provider(format!(
                        "tavily research failed: {}",
                        st.answer.unwrap_or_else(|| "vendor job failed".into())
                    ))),
                    meta,
                });
            }
            Ok(_) => {
                if std::time::Instant::now() >= deadline {
                    key_hold.finish_release().await;
                    if let Some(h) = proxy_hold.as_mut() {
                        h.finish_release().await;
                    }
                    meta.note_attempt(SVC_TAVILY, lease.id, node_id, false);
                    return Err(ProductOutcome {
                        result: ResearchError::Extract(ExtractError::ExtractTimeout(format!(
                            "tavily research did not finish within {}s",
                            poll_budget.as_secs()
                        ))),
                        meta,
                    });
                }
                tokio::time::sleep(TAVILY_RESEARCH_POLL).await;
            }
            Err(e) => {
                key_hold.finish_release().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_release().await;
                }
                meta.note_attempt(SVC_TAVILY, lease.id, node_id, false);
                return Err(ProductOutcome {
                    result: ResearchError::Extract(structured_provider_err(
                        "tavily research status",
                        e,
                    )),
                    meta,
                });
            }
        }
    }
}

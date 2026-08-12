//! Research orchestration: web search + scrape + optional social leg.

use serpotter_core::{route_search, RouteInput, SearchQuery, Sources};
use serpotter_providers::SVC_XAI;

use crate::dto::{Citation, Evidence, ResearchRequest, ResearchResponse, ScrapedPage};
use crate::error::ResearchError;
use crate::hold::KeyHold;
use crate::meta::{ExecMeta, ProductOutcome, ProgressEvent};
use crate::search::{run_provider, search_inner};
use crate::ProductCtx;

use super::extract_url::extract_url;
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
    ctx.emit(&ProgressEvent::Phase {
        name: "web".into(),
        done: 1,
        total: 3,
    });
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
        // (run_provider note_attempt, success AND failure), so the wire merge
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
            ctx.emit(&ProgressEvent::Phase {
                name: "social".into(),
                done: 3,
                total: 3,
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
            let decision = route_search(RouteInput { query: &social_q });
            let x_sources = ["x".to_string()];
            // D4/F15: the social leg records the xai attempt in social_meta on
            // BOTH success and failure (run_provider note_attempt), so the wire
            // merge below inherits "attempted" semantics automatically.
            let (provider_result, social_err, social_meta, social_usage) = match run_provider(
                ctx,
                SVC_XAI,
                &social_q,
                &decision,
                n,
                false,
                &[],
                &[],
                Some(x_sources.as_slice()),
            )
            .await
            {
                Ok(o) => {
                    // B2: capture the successful xAI /responses usage before
                    // moving the items out of the provider result.
                    let usage = (
                        o.result.input_tokens,
                        o.result.output_tokens,
                        o.result.total_tokens,
                        o.result.cost,
                    );
                    (Ok(o.result.items), None, o.meta, usage)
                }
                Err(o) => (
                    Err(()),
                    Some(o.result.to_string()),
                    o.meta,
                    (None, None, None, None),
                ),
            };
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
        synthesize(ctx, &query, &scraped, &mut meta).await
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
            if let Some(refined) = synthesize(ctx, &query, &scraped, &mut meta).await {
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
async fn synthesize(
    ctx: &ProductCtx,
    query: &str,
    pages: &[ScrapedPage],
    meta: &mut ExecMeta,
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
    let call = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        ctx.providers
            .xai
            .complete(&lease.key, system, &user, None, 1200),
    );
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

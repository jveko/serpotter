use super::*;
use serpotter_db::connect_and_migrate;
use std::sync::Arc;
use std::time::Duration;

#[test]
fn proxy_url_http_with_auth() {
    assert_eq!(
        proxy_url_from_node("http", "proxy.example", 8080, Some("u"), Some("p")),
        "http://u:p@proxy.example:8080"
    );
}

#[test]
fn proxy_url_https_and_socks5() {
    assert_eq!(
        proxy_url_from_node("https", "h.example", 443, None, None),
        "https://h.example:443"
    );
    assert_eq!(
        proxy_url_from_node("socks5", "s.example", 1080, Some("u"), Some("p")),
        "socks5://u:p@s.example:1080"
    );
}

#[test]
fn proxy_url_user_only_and_encoding() {
    assert_eq!(
        proxy_url_from_node("http", "h", 1, Some("a@b"), None),
        "http://a%40b@h:1"
    );
}

#[tokio::test]
async fn empty_nodes_returns_none_direct() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let pool = ProxyPool::new(db);
    assert!(pool.acquire().await.unwrap().is_none());
    assert!(!pool.require_proxy());
}

#[tokio::test]
async fn require_proxy_flag_preserved_on_empty_nodes() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let pool = ProxyPool::with_options(db, true);
    assert!(pool.require_proxy());
    assert!(pool.acquire().await.unwrap().is_none());
}

#[tokio::test]
async fn release_decrements_inflight() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let n = db
        .insert_node("rel.example", 8080, None, None, "http")
        .await
        .unwrap();
    let pool = ProxyPool::new(db.clone());

    let lease = pool.acquire().await.unwrap().unwrap();
    assert_eq!(lease.node_id, n.id);
    assert_eq!(
        db.list_nodes().await.unwrap()[0].inflight,
        1,
        "acquire bumps inflight"
    );

    pool.release(&lease).await.unwrap();
    assert_eq!(
        db.list_nodes().await.unwrap()[0].inflight,
        0,
        "release must decrement"
    );
}

/// C3a: refresh keeps a held node's lease alive without disturbing its
/// health — acquire → refresh → the lease is still stamped and inflight
/// unchanged; success finishes normally after the refresh.
#[tokio::test]
async fn refresh_keeps_held_node_lease_alive() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let n = db
        .insert_node("refresh.example", 8080, None, None, "http")
        .await
        .unwrap();
    let pool = ProxyPool::new(db.clone());

    let lease = pool.acquire().await.unwrap().unwrap();
    assert_eq!(lease.node_id, n.id);
    let row = db.list_nodes().await.unwrap().into_iter().next().unwrap();
    assert_eq!(row.inflight, 1);
    assert!(row.lease_until.is_some(), "acquire stamps lease_until");

    pool.refresh(&lease).await.unwrap();

    let row = db.list_nodes().await.unwrap().into_iter().next().unwrap();
    assert_eq!(row.inflight, 1, "refresh never releases the hold");
    assert!(row.lease_until.is_some(), "refresh keeps the lease stamped");
    assert_eq!(row.consecutive_fails, 0, "refresh never blames health");

    // The hold is still live: success finishes normally.
    pool.report_success(&lease).await.unwrap();
    let row = db.list_nodes().await.unwrap().into_iter().next().unwrap();
    assert_eq!(row.inflight, 0);
    assert_eq!(row.consecutive_fails, 0);
}

/// C3a: refreshing a released or absent node is a no-op success — never an
/// error or panic — and a released lease is never re-stamped.
#[tokio::test]
async fn refresh_absent_or_released_is_noop() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let n = db
        .insert_node("refresh-noop.example", 8080, None, None, "http")
        .await
        .unwrap();
    let pool = ProxyPool::new(db.clone());

    // Absent node: Ok, no panic.
    let phantom = ProxyLease {
        node_id: 9_999_999,
        url: "http://phantom.example:1".into(),
    };
    pool.refresh(&phantom).await.unwrap();

    // Acquire then release: refresh after release is Ok and must NOT leave a
    // stale lease behind (release cleared it; the inflight guard keeps it).
    let lease = pool.acquire().await.unwrap().unwrap();
    assert_eq!(lease.node_id, n.id);
    pool.release(&lease).await.unwrap();
    pool.refresh(&lease).await.unwrap();
    let row = db.list_nodes().await.unwrap().into_iter().next().unwrap();
    assert_eq!(row.inflight, 0);
    assert_eq!(
        row.lease_until, None,
        "released node must not be re-stamped"
    );
}

#[tokio::test]
async fn report_failure_disables_at_three() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let n = db
        .insert_node("fail.example", 8080, None, None, "http")
        .await
        .unwrap();
    let pool = ProxyPool::new(db.clone());

    for i in 1..=3 {
        let lease = pool.acquire().await.unwrap().expect("node still enabled");
        assert_eq!(lease.node_id, n.id);
        pool.report_failure(&lease, None).await.unwrap();
        let row = db.list_nodes().await.unwrap().into_iter().next().unwrap();
        assert_eq!(row.consecutive_fails, i);
        if i < 3 {
            assert_eq!(row.enabled, 1);
        } else {
            assert_eq!(row.enabled, 0);
            assert_eq!(row.inflight, 0);
        }
    }

    assert!(
        pool.acquire().await.unwrap().is_none(),
        "disabled node → direct"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_acquire_least_inflight_distinct() {
    // File DB allows multi-connection; :memory: pool is max_connections=1.
    let path =
        std::env::temp_dir().join(format!("serpotter-outbound-pool-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite:{}?mode=rwc", path.display());
    let db = connect_and_migrate(&url).await.unwrap();
    let a = db
        .insert_node("a.example", 8001, None, None, "http")
        .await
        .unwrap();
    let b = db
        .insert_node("b.example", 8002, None, None, "http")
        .await
        .unwrap();
    let pool = Arc::new(ProxyPool::new(db.clone()));

    let p1 = Arc::clone(&pool);
    let p2 = Arc::clone(&pool);
    let (r1, r2) = tokio::join!(p1.acquire(), p2.acquire());
    let l1 = r1.unwrap().expect("lease1");
    let l2 = r2.unwrap().expect("lease2");

    let ids: std::collections::HashSet<i64> = [l1.node_id, l2.node_id].into_iter().collect();
    assert_eq!(
        ids.len(),
        2,
        "two concurrent acquires on tied inflight must pick distinct nodes"
    );
    assert!(ids.contains(&a.id) && ids.contains(&b.id));

    pool.release(&l1).await.unwrap();
    pool.release(&l2).await.unwrap();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn acquire_builds_url_from_row_protocol() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_node("proxy.example", 8080, Some("u"), Some("p"), "socks5")
        .await
        .unwrap();
    let pool = ProxyPool::new(db);
    let lease = pool.acquire().await.unwrap().unwrap();
    assert_eq!(lease.url, "socks5://u:p@proxy.example:8080");
    pool.report_success(&lease).await.unwrap();
}

// --- FU21: invalid NODE_HOLD_TTL_SECS warns (never silent fallback) ----------

/// Serializes process-env mutation so parallel tests never race set/remove.
static ENV_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// Test-only capture sink for WARN+ events (Arc-owned buffer, no leak).
#[derive(Clone, Default)]
struct CaptureSink(Arc<parking_lot::Mutex<Vec<u8>>>);

impl std::io::Write for CaptureSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture_warns(f: impl FnOnce()) -> String {
    let sink = CaptureSink::default();
    let writer = sink.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false) // CI runners emit ANSI escapes; assertions need plain text
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    let guard = sink.0.lock();
    String::from_utf8_lossy(&guard).into_owned()
}

#[test]
fn invalid_node_hold_ttl_warns_and_defaults() {
    let _guard = ENV_LOCK.lock();
    std::env::set_var("NODE_HOLD_TTL_SECS", "not-a-number");
    let text = capture_warns(|| {
        assert_eq!(
            env_i64_or("NODE_HOLD_TTL_SECS", serpotter_db::NODE_HOLD_TTL_SECS),
            serpotter_db::NODE_HOLD_TTL_SECS,
            "unparseable value must fall back to the default"
        );
    });
    std::env::remove_var("NODE_HOLD_TTL_SECS");
    assert!(
        text.contains("NODE_HOLD_TTL_SECS"),
        "warn must name the var: {text}"
    );
    assert!(
        text.contains("not-a-number"),
        "warn must carry the raw offending value: {text}"
    );
}

#[test]
fn node_hold_ttl_parseable_value_wins_without_warning() {
    let _guard = ENV_LOCK.lock();
    std::env::set_var("NODE_HOLD_TTL_SECS", "7");
    let text = capture_warns(|| {
        assert_eq!(env_i64_or("NODE_HOLD_TTL_SECS", 90), 7);
    });
    std::env::remove_var("NODE_HOLD_TTL_SECS");
    assert!(
        text.is_empty(),
        "no warn expected for a parseable value: {text}"
    );
}

// --- B12 node connectivity probe -------------------------------------------------

fn refused_node_row() -> serpotter_db::NodeRow {
    serpotter_db::NodeRow {
        id: 1,
        host: "127.0.0.1".into(),
        port: 9,
        protocol: "http".into(),
        username: None,
        password: None,
        enabled: 1,
        inflight: 0,
        consecutive_fails: 0,
        last_error: None,
        lease_until: None,
        disabled_at: None,
    }
}

/// A node at 127.0.0.1:9 refuses instantly — the probe must fail with an
/// honest transport-class error (no network, no CI flake). Also proves the
/// proxy URL was built from the row correctly (a malformed URL would surface
/// as "invalid proxy URL" instead of a connection failure).
#[tokio::test]
async fn test_node_refused_node_reports_connection_failure() {
    let err = test_node(&refused_node_row(), Duration::from_secs(10))
        .await
        .expect_err("127.0.0.1:9 must refuse the probe");
    assert!(
        err.to_ascii_lowercase().contains("connection failed"),
        "transport class must be reported: {err}"
    );
    assert!(
        err.to_ascii_lowercase().contains("refused"),
        "the underlying refusal must be visible: {err}"
    );
}

/// Credentialed nodes build a userinfo-bearing proxy URL — covered end to end
/// via `proxy_url_from_node` (already unit-tested); this guards that the probe
/// URL source stays the same builder the pool uses.
#[test]
fn probe_uses_pool_proxy_url_builder() {
    let row = serpotter_db::NodeRow {
        id: 2,
        host: "proxy.example".into(),
        port: 8080,
        protocol: "socks5".into(),
        username: Some("u".into()),
        password: Some("p".into()),
        enabled: 1,
        inflight: 0,
        consecutive_fails: 0,
        last_error: None,
        lease_until: None,
        disabled_at: None,
    };
    assert_eq!(
        proxy_url_from_node(
            &row.protocol,
            &row.host,
            row.port as u16,
            row.username.as_deref(),
            row.password.as_deref(),
        ),
        "socks5://u:p@proxy.example:8080",
        "probe must dial the same URL the pool leases"
    );
}

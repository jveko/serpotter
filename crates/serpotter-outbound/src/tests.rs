use super::*;
use serpotter_db::connect_and_migrate;
use std::sync::Arc;

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

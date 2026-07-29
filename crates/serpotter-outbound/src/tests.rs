use super::*;
use serpotter_db::connect_and_migrate;
use std::sync::Arc;

#[test]
fn proxy_url_with_auth() {
    assert_eq!(
        proxy_url_from_node("proxy.example", 8080, Some("u"), Some("p")),
        "http://u:p@proxy.example:8080"
    );
}

#[tokio::test]
async fn fixed_mode_ignores_nodes() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let node = db
        .insert_node("node.example", 9000, None, None, "http")
        .await
        .unwrap();
    let pool = ProxyPool::from_env_and_db(Some("http://fixed.proxy:3128".into()), db.clone());

    let lease = pool.acquire().await.unwrap().expect("fixed always Some");
    assert_eq!(lease.node_id, None);
    assert_eq!(lease.url, "http://fixed.proxy:3128");

    // Second acquire still fixed URL — never the node.
    let again = pool.acquire().await.unwrap().unwrap();
    assert_eq!(again.node_id, None);
    assert_eq!(again.url, "http://fixed.proxy:3128");

    let row = db.list_nodes().await.unwrap().into_iter().next().unwrap();
    assert_eq!(row.id, node.id);
    assert_eq!(row.inflight, 0, "fixed must not bump node inflight");
}

#[tokio::test]
async fn empty_nodes_returns_none_direct() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let pool = ProxyPool::from_env_and_db(None, db);
    assert!(pool.acquire().await.unwrap().is_none());
    assert!(!pool.require_proxy());
}

#[tokio::test]
async fn require_proxy_flag_preserved_on_empty_nodes() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let pool = ProxyPool::with_options(None, db, true);
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
    let pool = ProxyPool::from_env_and_db(None, db.clone());

    let lease = pool.acquire().await.unwrap().unwrap();
    assert_eq!(lease.node_id, Some(n.id));
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
    let pool = ProxyPool::from_env_and_db(None, db.clone());

    for i in 1..=3 {
        let lease = pool.acquire().await.unwrap().expect("node still enabled");
        assert_eq!(lease.node_id, Some(n.id));
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

#[tokio::test]
async fn fixed_report_is_noop_on_nodes() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let n = db
        .insert_node("noop.example", 8080, None, None, "http")
        .await
        .unwrap();
    // Seed inflight so we can detect accidental release/report.
    db.bump_node_inflight(n.id, 2).await.unwrap();
    let pool = ProxyPool::from_env_and_db(Some("http://fixed:1".into()), db.clone());

    let lease = pool.acquire().await.unwrap().unwrap();
    assert_eq!(lease.node_id, None);

    pool.report_success(&lease).await.unwrap();
    pool.report_failure(&lease, None).await.unwrap();
    pool.release(&lease).await.unwrap();

    let row = db.list_nodes().await.unwrap().into_iter().next().unwrap();
    assert_eq!(row.id, n.id);
    assert_eq!(row.inflight, 2, "fixed reports must not touch nodes SQL");
    assert_eq!(row.consecutive_fails, 0);
    assert_eq!(row.enabled, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_acquire_least_inflight_distinct() {
    // File DB allows multi-connection; :memory: pool is max_connections=1.
    let path = std::env::temp_dir().join(format!(
        "serpotter-outbound-pool-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite:{}?mode=rwc", path.display());
    let db = connect_and_migrate(&url).await.unwrap();
    let a = db.insert_node("a.example", 8001, None, None, "http").await.unwrap();
    let b = db.insert_node("b.example", 8002, None, None, "http").await.unwrap();
    let pool = Arc::new(ProxyPool::from_env_and_db(None, db.clone()));

    let p1 = Arc::clone(&pool);
    let p2 = Arc::clone(&pool);
    let (r1, r2) = tokio::join!(p1.acquire(), p2.acquire());
    let l1 = r1.unwrap().expect("lease1");
    let l2 = r2.unwrap().expect("lease2");

    let ids: std::collections::HashSet<i64> = [l1.node_id.unwrap(), l2.node_id.unwrap()]
        .into_iter()
        .collect();
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
async fn nodes_mode_builds_url_from_row() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_node("proxy.example", 8080, Some("u"), Some("p"), "http")
        .await
        .unwrap();
    let pool = ProxyPool::from_env_and_db(None, db);
    let lease = pool.acquire().await.unwrap().unwrap();
    assert_eq!(lease.url, "http://u:p@proxy.example:8080");
    pool.report_success(&lease).await.unwrap();
}

#[tokio::test]
async fn whitespace_env_is_not_fixed() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_node("ws.example", 1, None, None, "http").await.unwrap();
    let pool = ProxyPool::from_env_and_db(Some("   ".into()), db);
    let lease = pool.acquire().await.unwrap().unwrap();
    assert!(lease.node_id.is_some(), "blank env must fall through to nodes");
}

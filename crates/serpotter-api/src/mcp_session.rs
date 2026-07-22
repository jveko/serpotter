//! Process-local MCP session registry (Streamable HTTP subset).
//! Single-VPS only — not shared across processes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

pub const MCP_SESSION_TTL_SECS: u64 = 3600;
pub const MCP_SESSION_HEADER: &str = "mcp-session-id";

#[derive(Clone)]
pub struct McpSessionStore {
    inner: Arc<Mutex<HashMap<String, Instant>>>,
    ttl: Duration,
}

impl Default for McpSessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl McpSessionStore {
    pub fn new() -> Self {
        Self::with_ttl(Duration::from_secs(MCP_SESSION_TTL_SECS))
    }

    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            ttl,
        }
    }

    pub fn mint_id() -> String {
        let mut buf = [0u8; 16];
        getrandom::fill(&mut buf).expect("getrandom");
        let mut s = String::with_capacity(32);
        for b in buf {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    pub fn create(&self) -> String {
        let id = Self::mint_id();
        self.inner.lock().insert(id.clone(), Instant::now());
        id
    }

    pub fn contains_live(&self, id: &str) -> bool {
        self.touch(id)
    }

    pub fn touch(&self, id: &str) -> bool {
        let mut g = self.inner.lock();
        match g.get(id) {
            Some(t) if t.elapsed() <= self.ttl => {
                g.insert(id.to_string(), Instant::now());
                true
            }
            Some(_) => {
                g.remove(id);
                false
            }
            None => false,
        }
    }

    pub fn remove(&self, id: &str) -> bool {
        self.inner.lock().remove(id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn mint_id_is_32_hex() {
        let id = McpSessionStore::mint_id();
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn create_touch_remove() {
        let store = McpSessionStore::new();
        let id = store.create();
        assert!(store.contains_live(&id));
        assert!(store.touch(&id));
        assert!(store.remove(&id));
        assert!(!store.contains_live(&id));
        assert!(!store.touch(&id));
    }

    #[test]
    fn expired_session_not_live() {
        let store = McpSessionStore::with_ttl(Duration::from_millis(1));
        let id = store.create();
        std::thread::sleep(Duration::from_millis(5));
        assert!(!store.contains_live(&id));
    }
}

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct GeoEntry {
    pub name: String,
    pub country: String,
    pub timezone: String,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug)]
pub struct GeoCache {
    ttl: Duration,
    cap: usize,
    inner: Mutex<HashMap<String, (GeoEntry, Instant)>>,
}

impl GeoCache {
    pub fn new(ttl: Duration, cap: usize) -> Self {
        Self {
            ttl,
            cap,
            inner: Mutex::new(HashMap::new()),
        }
    }

    pub fn normalize(query: &str) -> String {
        query.trim().to_ascii_lowercase()
    }

    pub fn get(&self, query: &str) -> Option<GeoEntry> {
        let key = Self::normalize(query);
        let mut map = self.inner.lock().expect("cache poisoned");
        let entry = map.get(&key)?.clone();
        if entry.1.elapsed() > self.ttl {
            map.remove(&key);
            return None;
        }
        Some(entry.0)
    }

    pub fn clear(&self) {
        let mut map = self.inner.lock().expect("cache poisoned");
        map.clear();
    }

    pub fn put(&self, query: &str, value: GeoEntry) {
        let key = Self::normalize(query);
        let mut map = self.inner.lock().expect("cache poisoned");
        if map.len() >= self.cap && !map.contains_key(&key) {
            // Evict oldest entry by insertion timestamp.
            if let Some(oldest_key) = map
                .iter()
                .min_by_key(|(_, (_, ts))| *ts)
                .map(|(k, _)| k.clone())
            {
                map.remove(&oldest_key);
            }
        }
        map.insert(key, (value, Instant::now()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    fn sample(name: &str) -> GeoEntry {
        GeoEntry {
            name: name.into(),
            country: "ES".into(),
            timezone: "Europe/Madrid".into(),
            lat: 40.0,
            lon: -3.0,
        }
    }

    #[test]
    fn put_and_get() {
        let c = GeoCache::new(Duration::from_secs(60), 10);
        c.put("Madrid", sample("Madrid"));
        let got = c.get("  madrid  ").expect("hit");
        assert_eq!(got.name, "Madrid");
    }

    #[test]
    fn expiry_returns_none() {
        let c = GeoCache::new(Duration::from_millis(10), 10);
        c.put("Madrid", sample("Madrid"));
        sleep(Duration::from_millis(20));
        assert!(c.get("Madrid").is_none());
    }

    #[test]
    fn eviction_when_full() {
        let c = GeoCache::new(Duration::from_secs(60), 2);
        c.put("A", sample("A"));
        sleep(Duration::from_millis(2));
        c.put("B", sample("B"));
        sleep(Duration::from_millis(2));
        c.put("C", sample("C"));
        assert!(c.get("A").is_none(), "oldest should be evicted");
        assert!(c.get("B").is_some());
        assert!(c.get("C").is_some());
    }
}

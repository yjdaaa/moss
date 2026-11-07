//! Bounded in-memory 5-minute cache for weather/news; cache keys are opaque digests.
use super::http::FetchError;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(300);
const MAX_ENTRIES: usize = 32;

#[derive(Default)]
pub struct PluginCache {
    values: Mutex<HashMap<String, (Instant, Value)>>,
    flights: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}
impl PluginCache {
    pub fn fetch(
        &self,
        key: &str,
        fetch: impl FnOnce() -> Result<Value, FetchError>,
    ) -> Result<Value, FetchError> {
        if let Some(hit) = self.get(key) {
            return Ok(hit);
        }
        let guard = {
            let mut flights = self.flights.lock().map_err(|_| FetchError::Network)?;
            if flights.len() > MAX_ENTRIES * 2 {
                flights.retain(|_, lock| Arc::strong_count(lock) > 1);
            }
            flights
                .entry(key.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _one = guard.lock().map_err(|_| FetchError::Network)?;
        if let Some(hit) = self.get(key) {
            return Ok(hit);
        }
        let value = fetch()?;
        let mut entries = self.values.lock().map_err(|_| FetchError::Network)?;
        if entries.len() >= MAX_ENTRIES {
            if let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, v)| v.0)
                .map(|(key, _)| key.clone())
            {
                entries.remove(&oldest);
            }
        }
        entries.insert(key.to_string(), (Instant::now(), value.clone()));
        Ok(value)
    }
    fn get(&self, key: &str) -> Option<Value> {
        let mut data = self.values.lock().ok()?;
        match data.get(key) {
            Some((when, value)) if when.elapsed() < TTL => Some(value.clone()),
            _ => {
                data.remove(key);
                None
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[test]
    fn cache_miss_hit_error_not_cached_and_changed_key() {
        let cache = PluginCache::default();
        let n = AtomicUsize::new(0);
        let load = || {
            n.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::json!({"x":1}))
        };
        cache.fetch("revision1", &load).unwrap();
        cache.fetch("revision1", &load).unwrap();
        assert_eq!(n.load(Ordering::SeqCst), 1);
        cache.fetch("revision2", &load).unwrap();
        assert_eq!(n.load(Ordering::SeqCst), 2);
        assert!(
            cache
                .fetch("error", || Err(FetchError::RateLimited))
                .is_err()
        );
        assert_eq!(
            cache.fetch("error", || Ok(serde_json::json!({}))).unwrap(),
            serde_json::json!({})
        );
    }
    #[test]
    fn concurrent_same_key_requests_single_flight() {
        let cache = Arc::new(PluginCache::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let jobs: Vec<_> = (0..8)
            .map(|_| {
                let cache = cache.clone();
                let calls = calls.clone();
                std::thread::spawn(move || {
                    cache
                        .fetch("same", || {
                            calls.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(15));
                            Ok(serde_json::json!({"ok":true}))
                        })
                        .unwrap()
                })
            })
            .collect();
        for job in jobs {
            assert_eq!(job.join().unwrap()["ok"], true);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn expired_values_reload_and_capacity_evicts_oldest() {
        let cache = PluginCache::default();
        cache
            .fetch("expired", || Ok(serde_json::json!({"old":true})))
            .unwrap();
        cache.values.lock().unwrap().get_mut("expired").unwrap().0 = Instant::now() - TTL;
        assert_eq!(
            cache
                .fetch("expired", || Ok(serde_json::json!({"fresh":true})))
                .unwrap(),
            serde_json::json!({"fresh":true})
        );
        cache.values.lock().unwrap().get_mut("expired").unwrap().0 =
            Instant::now() - Duration::from_secs(1);
        for n in 0..MAX_ENTRIES {
            cache
                .fetch(&format!("other-{n}"), || Ok(serde_json::json!({})))
                .unwrap();
        }
        assert_eq!(cache.values.lock().unwrap().len(), MAX_ENTRIES);
        assert!(cache.get("expired").is_none());
    }
}

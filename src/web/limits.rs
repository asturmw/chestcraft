//! Concurrent download limit per IP (anonymous visitors).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct DownloadLimiter {
    inner: Arc<Mutex<HashMap<IpAddr, u32>>>,
}

/// Holds one download slot until dropped (it travels with the response body).
pub struct DownloadGuard {
    inner: Arc<Mutex<HashMap<IpAddr, u32>>>,
    ip: IpAddr,
}

impl DownloadLimiter {
    pub fn try_acquire(&self, ip: IpAddr, max: u32) -> Option<DownloadGuard> {
        let mut map = self.inner.lock().unwrap();
        let n = map.entry(ip).or_insert(0);
        if *n >= max {
            return None;
        }
        *n += 1;
        Some(DownloadGuard {
            inner: self.inner.clone(),
            ip,
        })
    }

    /// Downloads this address runs now.
    pub fn active(&self, ip: IpAddr) -> u32 {
        self.inner.lock().unwrap().get(&ip).copied().unwrap_or(0)
    }
}

impl Drop for DownloadGuard {
    fn drop(&mut self) {
        let mut map = self.inner.lock().unwrap();
        if let Some(n) = map.get_mut(&self.ip) {
            *n -= 1;
            if *n == 0 {
                map.remove(&self.ip);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_released_on_drop() {
        let l = DownloadLimiter::default();
        let ip: IpAddr = "203.0.113.1".parse().unwrap();
        let guards: Vec<_> = (0..4).map(|_| l.try_acquire(ip, 4).unwrap()).collect();
        assert!(l.try_acquire(ip, 4).is_none());
        assert!(l.try_acquire("203.0.113.2".parse().unwrap(), 4).is_some());
        drop(guards);
        assert_eq!(l.active(ip), 0);
        assert!(l.try_acquire(ip, 4).is_some());
    }
}

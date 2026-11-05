//! Slows down password guessing.
//!
//! Failures are counted per (IP, account) and per IP across accounts. After the free tries,
//! every further failure makes that IP wait, doubling from a minute up to an hour (a minute
//! on the LAN). An account is never locked for everyone, so an attacker can't lock the owner
//! out. Many failures on one account from many IPs within an hour raise a
//! credential-stuffing signal. Counters live in memory and reset on restart.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;

pub const FREE_TRIES: u32 = 5;
pub const FREE_TRIES_PER_IP: u32 = 20;
pub const BASE_WAIT: i64 = 60;
pub const MAX_WAIT_REMOTE: i64 = 3600;
pub const MAX_WAIT_LOCAL: i64 = 60;
pub const STUFFING_FAILURES: usize = 20;
pub const STUFFING_IPS: usize = 5;
const WINDOW: i64 = 3600;
const FORGET_AFTER: i64 = 86_400;

#[derive(Debug, Clone, Copy, Default)]
struct Entry {
    failures: u32,
    until: i64,
    last: i64,
}

impl Entry {
    fn fail(&mut self, now: i64, free: u32, max_wait: i64) {
        self.failures += 1;
        self.last = now;
        if self.failures >= free {
            let doublings = (self.failures - free).min(12);
            let wait = (BASE_WAIT << doublings).min(max_wait);
            self.until = now + wait;
        }
    }
}

#[derive(Default)]
struct State {
    pairs: HashMap<(IpAddr, String), Entry>,
    ips: HashMap<IpAddr, Entry>,
    /// Recent failures per account: (time, ip).
    accounts: HashMap<String, Vec<(i64, IpAddr)>>,
    /// Last stuffing signal per account, so it fires once per window.
    signaled: HashMap<String, i64>,
    last_sweep: i64,
}

#[derive(Default)]
pub struct Backoff {
    state: Mutex<State>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failure {
    /// Seconds before the next try is accepted (0 = right away).
    pub wait: i64,
    /// Credential-stuffing pattern on this account.
    pub stuffing: bool,
}

impl Backoff {
    /// Seconds this IP has to wait before trying `account` again.
    pub fn wait(&self, ip: IpAddr, account: &str, now: i64) -> i64 {
        let st = self.state.lock().unwrap();
        let pair = st
            .pairs
            .get(&(ip, account.to_string()))
            .map_or(0, |e| e.until - now);
        let per_ip = st.ips.get(&ip).map_or(0, |e| e.until - now);
        pair.max(per_ip).max(0)
    }

    pub fn fail(&self, ip: IpAddr, account: &str, local: bool, now: i64) -> Failure {
        let max_wait = if local {
            MAX_WAIT_LOCAL
        } else {
            MAX_WAIT_REMOTE
        };
        let mut st = self.state.lock().unwrap();
        if now - st.last_sweep > 600 {
            st.pairs.retain(|_, e| now - e.last < FORGET_AFTER);
            st.ips.retain(|_, e| now - e.last < FORGET_AFTER);
            st.accounts
                .retain(|_, v| v.iter().any(|(t, _)| now - t < WINDOW));
            st.signaled.retain(|_, t| now - *t < WINDOW);
            st.last_sweep = now;
        }
        let pair = st.pairs.entry((ip, account.to_string())).or_default();
        pair.fail(now, FREE_TRIES, max_wait);
        let pair_until = pair.until;
        let per_ip = st.ips.entry(ip).or_default();
        per_ip.fail(now, FREE_TRIES_PER_IP, max_wait);
        let ip_until = per_ip.until;

        let recent = st.accounts.entry(account.to_string()).or_default();
        recent.retain(|(t, _)| now - t < WINDOW);
        recent.push((now, ip));
        let mut distinct: Vec<IpAddr> = recent.iter().map(|(_, ip)| *ip).collect();
        distinct.sort_unstable();
        distinct.dedup();
        let pattern = recent.len() >= STUFFING_FAILURES && distinct.len() >= STUFFING_IPS;
        let stuffing = pattern && st.signaled.get(account).is_none_or(|t| now - t >= WINDOW);
        if stuffing {
            st.signaled.insert(account.to_string(), now);
        }
        Failure {
            wait: (pair_until.max(ip_until) - now).max(0),
            stuffing,
        }
    }

    /// A correct password clears that IP's counter for the account.
    pub fn succeed(&self, ip: IpAddr, account: &str) {
        self.state
            .lock()
            .unwrap()
            .pairs
            .remove(&(ip, account.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(n: u8) -> IpAddr {
        IpAddr::from([203, 0, 113, n])
    }

    #[test]
    fn waits_start_after_the_free_tries_and_double() {
        let b = Backoff::default();
        for i in 1..FREE_TRIES {
            assert_eq!(b.fail(ip(1), "tibor", false, 1000).wait, 0, "try {i}");
        }
        assert_eq!(b.fail(ip(1), "tibor", false, 1000).wait, 60);
        assert_eq!(b.wait(ip(1), "tibor", 1030), 30);
        assert_eq!(b.fail(ip(1), "tibor", false, 1100).wait, 120);
        assert_eq!(b.fail(ip(1), "tibor", false, 1300).wait, 240);
        // Another IP isn't affected: the account is never locked for everyone.
        assert_eq!(b.wait(ip(2), "tibor", 1300), 0);
        b.succeed(ip(1), "tibor");
        assert_eq!(b.wait(ip(1), "tibor", 1300), 0);
    }

    #[test]
    fn waits_are_capped() {
        let b = Backoff::default();
        let mut last = Failure {
            wait: 0,
            stuffing: false,
        };
        for _ in 0..30 {
            last = b.fail(ip(1), "a", false, 1000);
        }
        assert_eq!(last.wait, MAX_WAIT_REMOTE);
        let lan = Backoff::default();
        for _ in 0..30 {
            last = lan.fail(IpAddr::from([192, 168, 1, 5]), "a", true, 1000);
        }
        assert_eq!(last.wait, MAX_WAIT_LOCAL);
    }

    #[test]
    fn one_ip_trying_many_accounts_is_slowed_too() {
        let b = Backoff::default();
        for n in 0..FREE_TRIES_PER_IP {
            b.fail(ip(9), &format!("user{n}"), false, 1000);
        }
        assert!(b.wait(ip(9), "fresh-account", 1000) > 0);
    }

    #[test]
    fn credential_stuffing_is_signaled_once_per_hour() {
        let b = Backoff::default();
        let mut signals = 0;
        for n in 0..40u32 {
            if b.fail(ip((n % 8) as u8), "tibor", false, 1000 + i64::from(n))
                .stuffing
            {
                signals += 1;
            }
        }
        assert_eq!(signals, 1);
        // Few IPs: no signal even with many failures.
        let b = Backoff::default();
        assert!((0..40).all(|n| !b.fail(ip((n % 2) as u8), "x", false, 1000).stuffing));
    }
}

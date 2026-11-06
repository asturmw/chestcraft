//! Daily maintenance at `trash.purge_at` (server time zone): purge trash items older than
//! their volume's `trash_days`, clean up uploads (and temporary WebDAV entries) abandoned
//! for `uploads.abandoned_days`, and drop guest sessions (share visitors, public uploaders)
//! that ran out.

use super::Shared;
use crate::config::TimeOfDay;
use crate::fs::{transfer, trash};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use std::time::Duration;

/// Seconds from `now` until the next `at` in `tz` (strictly in the future).
pub fn secs_until(at: TimeOfDay, tz: &TimeZone, now: Timestamp) -> u64 {
    let mut date = now.to_zoned(tz.clone()).date();
    for _ in 0..3 {
        let next = date
            .at(at.hour as i8, at.minute as i8, 0, 0)
            .to_zoned(tz.clone());
        if let Ok(t) = next
            && t.timestamp() > now
        {
            return (t.timestamp().as_second() - now.as_second()).max(1) as u64;
        }
        date = date.tomorrow().unwrap_or(date);
    }
    86_400
}

pub fn spawn(st: Shared) {
    let hourly = st.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            hourly.thumb_limiter.retain_recent();
            hourly.thumb_limiter.shrink_to_fit();
        }
    });
    tokio::spawn(async move {
        loop {
            let wait = secs_until(
                st.cfg.trash.purge_at,
                &st.cfg.server.timezone,
                Timestamp::now(),
            );
            tokio::time::sleep(Duration::from_secs(wait)).await;
            run(st.clone()).await;
        }
    });
}

/// One maintenance pass.
pub async fn run(st: Shared) {
    let done = tokio::task::spawn_blocking(move || {
        let now = Timestamp::now().as_second();
        let (purged, failed) = trash::purge_due(&st.db, &st.volumes, &st.cfg, now);
        let max_age = i64::from(st.cfg.uploads.abandoned_days) * 86_400;
        let (uploads, batches) = st.uploads.sweep(&st.volumes, max_age, now);
        let temps = transfer::sweep_temps(&st.db, &st.volumes, Some(max_age), now);
        let guests = crate::shares::sweep_guests(&st.db, now);
        tracing::info!(
            purged,
            failed,
            abandoned_uploads = uploads,
            abandoned_folders = batches,
            abandoned_temps = temps,
            expired_guest_sessions = guests,
            "daily cleanup done"
        );
    })
    .await;
    if done.is_err() {
        tracing::error!("daily cleanup crashed");
    }
}

/// At startup: no request is running yet, so every temporary entry of a WebDAV upload, copy
/// or move is left over from a crash.
pub async fn sweep_leftovers(st: Shared) {
    let swept = tokio::task::spawn_blocking(move || {
        transfer::sweep_temps(&st.db, &st.volumes, None, Timestamp::now().as_second())
    })
    .await;
    match swept {
        Ok(0) => {}
        Ok(n) => tracing::info!(removed = n, "removed temporary entries left by a crash"),
        Err(_) => tracing::error!("sweeping temporary entries crashed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_run_in_berlin_time() {
        let tz = TimeZone::get("Europe/Berlin").unwrap();
        let four = TimeOfDay { hour: 4, minute: 0 };
        // 2026-11-06 03:00 CET = 02:00 UTC → one hour.
        let now: Timestamp = "2026-11-06T02:00:00Z".parse().unwrap();
        assert_eq!(secs_until(four, &tz, now), 3600);
        // Exactly at 04:00 → tomorrow.
        let now: Timestamp = "2026-11-06T03:00:00Z".parse().unwrap();
        assert_eq!(secs_until(four, &tz, now), 86_400);
        // Across the switch to summer time (2027-03-28, 02:00 → 03:00): 23 hours.
        let now: Timestamp = "2027-03-27T03:00:00Z".parse().unwrap();
        assert_eq!(secs_until(four, &tz, now), 23 * 3600);
    }
}

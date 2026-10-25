//! Rules of the download queue: which download runs next, what happens
//! after a failed attempt, and how the queue continues after a restart. The
//! app runs the workers; the queue itself is kept in the library.

use crate::error::Error;
use crate::nexus::{Download, DownloadStatus};

/// Downloads running at the same time.
pub const WORKERS: usize = 3;

/// Seconds to wait before attempt `n` (1-based) is retried; `None` when the
/// download has failed often enough.
pub fn retry_delay(attempts: u32) -> Option<i64> {
    match attempts {
        1 => Some(5),
        2 => Some(30),
        3 => Some(120),
        _ => None,
    }
}

/// Whether a failure may go away by trying again.
pub fn is_retryable(error: &Error) -> bool {
    matches!(error, Error::Network(_))
}

/// The next download to start: the oldest waiting one whose retry time has
/// come.
pub fn next_ready(downloads: &[Download], now: i64) -> Option<usize> {
    downloads
        .iter()
        .enumerate()
        .filter(|(_, d)| d.status == DownloadStatus::Queued)
        .filter(|(_, d)| d.retry_at.is_none_or(|at| at <= now))
        .min_by_key(|(_, d)| d.created_at)
        .map(|(i, _)| i)
}

/// Records a failed attempt: waits and tries again for network problems,
/// otherwise (or after too many attempts) the download fails.
pub fn record_failure(download: &mut Download, error: &Error, now: i64) {
    download.attempts += 1;
    match retry_delay(download.attempts).filter(|_| is_retryable(error)) {
        Some(delay) => {
            download.status = DownloadStatus::Queued;
            download.retry_at = Some(now + delay);
        }
        None => {
            download.retry_at = None;
            download.status = DownloadStatus::Failed {
                message: error.to_string(),
            };
        }
    }
}

/// Whether the download needs an `nxm://` key that has expired.
pub fn link_expired(download: &Download, now: i64) -> bool {
    download.key.is_some() && download.expires.is_some_and(|e| (e as i64) <= now)
}

pub const EXPIRED_MESSAGE: &str = "This download link expired while the app was closed. Click \"Mod Manager Download\" on the mod page again (Premium members can press Retry).";

/// The queue as it continues after a restart: unfinished downloads wait
/// again (a partly written file is continued), except ones whose download
/// link expired. Ones that were installing or waiting for installer choices
/// start over from their kept archive.
pub fn restore(downloads: Vec<Download>, now: i64) -> Vec<Download> {
    downloads
        .into_iter()
        .map(|mut d| {
            match &d.status {
                DownloadStatus::Queued | DownloadStatus::Downloading => {
                    if d.archive_id.is_none() && link_expired(&d, now) {
                        d.status = DownloadStatus::Failed {
                            message: EXPIRED_MESSAGE.into(),
                        };
                    } else {
                        d.status = DownloadStatus::Queued;
                        d.retry_at = None;
                    }
                }
                DownloadStatus::Installing | DownloadStatus::AwaitingChoices { .. } => {
                    d.status = if d.archive_id.is_some() {
                        DownloadStatus::Queued
                    } else {
                        DownloadStatus::Failed {
                            message:
                                "The app closed while installing this. Start the download again."
                                    .into(),
                        }
                    };
                }
                DownloadStatus::Done { .. } | DownloadStatus::Failed { .. } => {}
            }
            d
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn download(id: &str, status: DownloadStatus, created_at: i64) -> Download {
        Download {
            id: id.into(),
            status,
            created_at,
            ..Default::default()
        }
    }

    #[test]
    fn retries_network_failures_with_backoff() {
        let mut d = download("a", DownloadStatus::Downloading, 0);
        let net = Error::Network("connection reset".into());
        record_failure(&mut d, &net, 100);
        assert_eq!(
            (d.status.clone(), d.retry_at),
            (DownloadStatus::Queued, Some(105))
        );
        assert_eq!(next_ready(std::slice::from_ref(&d), 104), None, "not yet");
        assert_eq!(next_ready(std::slice::from_ref(&d), 105), Some(0));
        record_failure(&mut d, &net, 200);
        record_failure(&mut d, &net, 300);
        assert_eq!(d.retry_at, Some(420));
        record_failure(&mut d, &net, 500);
        assert!(matches!(d.status, DownloadStatus::Failed { .. }), "gave up");

        let mut d = download("b", DownloadStatus::Downloading, 0);
        record_failure(
            &mut d,
            &Error::Nexus("Not found on Nexus Mods.".into()),
            100,
        );
        assert!(
            matches!(d.status, DownloadStatus::Failed { .. }),
            "not worth retrying"
        );
    }

    #[test]
    fn restores_after_a_restart() {
        let mut expired = download("expired", DownloadStatus::Downloading, 1);
        expired.key = Some("k".into());
        expired.expires = Some(50);
        let mut installing = download("installing", DownloadStatus::Installing, 2);
        installing.archive_id = Some(7);
        let restored = restore(
            vec![
                download("queued", DownloadStatus::Queued, 0),
                expired,
                installing,
                download(
                    "lost",
                    DownloadStatus::AwaitingChoices { token: "t".into() },
                    3,
                ),
            ],
            100,
        );
        let states: Vec<(&str, bool)> = restored
            .iter()
            .map(|d| (d.id.as_str(), d.status == DownloadStatus::Queued))
            .collect();
        assert_eq!(
            states,
            [
                ("queued", true),
                ("expired", false),
                ("installing", true),
                ("lost", false)
            ]
        );
        assert_eq!(
            restored[1].status,
            DownloadStatus::Failed {
                message: EXPIRED_MESSAGE.into()
            }
        );
        assert_eq!(next_ready(&restored, 100), Some(0), "oldest first");
    }
}

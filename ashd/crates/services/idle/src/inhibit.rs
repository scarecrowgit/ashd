//! logind `idle` inhibitors (`ListInhibitors`), read on demand.

use std::{sync::Mutex, time::Duration};

use smol::future::or;
use zbus::Connection;

/// "Is something holding an idle inhibitor?" — blocking, called from a worker thread.
pub trait InhibitSource: Send + Sync {
    fn idle_inhibited(&self) -> bool;
}

/// The real thing: the system bus, connected lazily and kept.
#[derive(Default)]
pub(crate) struct Logind {
    conn: Mutex<Option<Connection>>,
}

const TIMEOUT: Duration = Duration::from_secs(2);

type Row = (String, String, String, String, u32, u32);

impl Logind {
    async fn list(&self) -> zbus::Result<Vec<Row>> {
        let cached = self.conn.lock().ok().and_then(|g| g.clone());
        let conn = match cached {
            Some(c) => c,
            None => {
                let c = Connection::system().await?;
                if let Ok(mut g) = self.conn.lock() {
                    *g = Some(c.clone());
                }
                c
            }
        };
        let proxy = zbus::Proxy::new(&conn, "org.freedesktop.login1", "/org/freedesktop/login1", "org.freedesktop.login1.Manager").await?;
        proxy.call("ListInhibitors", &()).await
    }
}

impl InhibitSource for Logind {
    fn idle_inhibited(&self) -> bool {
        let rows = smol::block_on(or(async { Some(self.list().await) }, async {
            smol::Timer::after(TIMEOUT).await;
            None
        }));
        match rows {
            Some(Ok(rows)) => rows_inhibit(&rows),
            Some(Err(e)) => {
                tracing::debug!("idle: ListInhibitors: {e}");
                // Reconnect next time.
                if let Ok(mut g) = self.conn.lock() {
                    *g = None;
                }
                false
            }
            None => {
                tracing::debug!("idle: ListInhibitors timed out");
                false
            }
        }
    }
}

/// Does any `block` lock cover `idle`?
pub(crate) fn rows_inhibit(rows: &[Row]) -> bool {
    rows.iter().any(|(what, _who, _why, mode, _uid, _pid)| mode == "block" && what.split(':').any(|w| w == "idle"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(what: &str, mode: &str) -> Row {
        (what.into(), "someone".into(), "because".into(), mode.into(), 1000, 42)
    }

    #[test]
    fn only_block_locks_that_cover_idle_count() {
        assert!(!rows_inhibit(&[]));
        assert!(!rows_inhibit(&[row("sleep", "block"), row("shutdown:sleep", "delay")]));
        assert!(!rows_inhibit(&[row("idle", "delay")]));
        assert!(rows_inhibit(&[row("sleep", "block"), row("handle-power-key:idle", "block")]));
    }
}

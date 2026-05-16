//! Terminal mode snapshots.
//!
//! Mode polling is intentionally best-effort. Platforms that cannot expose the
//! needed local flags report unsupported instead of synthesizing values.

use crate::error::{PtyxError, PtyxErrorKind};
use crate::session::{Session, SessionInner};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TermMode {
    pub(crate) canonical: Option<bool>,
    pub(crate) echo: Option<bool>,
    pub(crate) signals: Option<bool>,
}

pub(crate) fn get_term_mode(session: &Session) -> Result<TermMode, PtyxError> {
    term_mode_snapshot(&session.inner)
}

pub(crate) fn term_mode_snapshot(inner: &SessionInner) -> Result<TermMode, PtyxError> {
    #[cfg(unix)]
    {
        use nix::sys::termios::LocalFlags;
        let master = inner
            .master
            .lock()
            .map_err(|_| PtyxError::new(PtyxErrorKind::Error, "master lock poisoned"))?;
        let termios = master.get_termios().ok_or_else(|| {
            PtyxError::new(PtyxErrorKind::Unsupported, "terminal mode unavailable")
        })?;
        let canonical = termios.local_flags.contains(LocalFlags::ICANON);
        let echo = termios.local_flags.contains(LocalFlags::ECHO);
        let signals = termios.local_flags.contains(LocalFlags::ISIG);
        Ok(TermMode {
            canonical: Some(canonical),
            echo: Some(echo),
            signals: Some(signals),
        })
    }

    #[cfg(not(unix))]
    {
        let _ = inner;
        Err(PtyxError::new(
            PtyxErrorKind::Unsupported,
            "terminal mode snapshots are unsupported",
        ))
    }
}

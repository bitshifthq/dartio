//! Terminal mode snapshots.
//!
//! Mode polling is intentionally best-effort. Platforms that cannot expose the
//! needed local flags report unsupported instead of synthesizing values.

use crate::abi::{
    ptyx_term_mode_t, PTYX_STATUS_INVALID_ARGUMENT, PTYX_STATUS_OK, PTYX_STATUS_UNSUPPORTED,
};
#[cfg(unix)]
use crate::abi::{
    PTYX_STATUS_ERROR, PTYX_TERM_MODE_CANONICAL_VALID, PTYX_TERM_MODE_ECHO_VALID,
    PTYX_TERM_MODE_SIGNALS_VALID,
};
use crate::error::{ffi_status, PtyxError};
use crate::session::{ptyx_session, session_ref, SessionInner};

pub(crate) fn get_term_mode(session: *mut ptyx_session, out_mode: *mut ptyx_term_mode_t) -> u32 {
    ffi_status(|| {
        let session = session_ref(session)?;
        if out_mode.is_null() {
            return Err(PtyxError::new(
                PTYX_STATUS_INVALID_ARGUMENT,
                "out_mode must not be null",
            ));
        }
        let mode = term_mode_snapshot(&session.inner)?;
        unsafe {
            *out_mode = mode;
        }
        Ok(PTYX_STATUS_OK)
    })
}

pub(crate) fn term_mode_snapshot(inner: &SessionInner) -> Result<ptyx_term_mode_t, PtyxError> {
    #[cfg(unix)]
    {
        use nix::sys::termios::LocalFlags;
        let master = inner
            .master
            .lock()
            .map_err(|_| PtyxError::new(PTYX_STATUS_ERROR, "master lock poisoned"))?;
        let termios = master
            .get_termios()
            .ok_or_else(|| PtyxError::new(PTYX_STATUS_UNSUPPORTED, "terminal mode unavailable"))?;
        let canonical = termios.local_flags.contains(LocalFlags::ICANON);
        let echo = termios.local_flags.contains(LocalFlags::ECHO);
        let signals = termios.local_flags.contains(LocalFlags::ISIG);
        Ok(ptyx_term_mode_t {
            valid_fields: PTYX_TERM_MODE_CANONICAL_VALID
                | PTYX_TERM_MODE_ECHO_VALID
                | PTYX_TERM_MODE_SIGNALS_VALID,
            canonical,
            echo,
            signals,
        })
    }

    #[cfg(not(unix))]
    {
        let _ = inner;
        Err(PtyxError::new(
            PTYX_STATUS_UNSUPPORTED,
            "terminal mode snapshots are unsupported",
        ))
    }
}

pub(crate) fn same_observed_mode(a: &ptyx_term_mode_t, b: &ptyx_term_mode_t) -> bool {
    a.valid_fields == b.valid_fields
        && a.canonical == b.canonical
        && a.echo == b.echo
        && a.signals == b.signals
}

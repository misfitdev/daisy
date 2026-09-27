//! When a connection to the other Mac ends or cannot be made: whether to try
//! again, and how long to wait. No I/O; `service::connect` acts on it.

use std::time::Duration;

use backon::{BackoffBuilder, ExponentialBackoff, ExponentialBuilder};

use crate::session::SessionError;
use crate::share::Silent;

/// A session that lasted this long starts the waits over from the shortest;
/// one that dropped sooner keeps backing off, so a flapping link is not hammered.
pub const STABLE: Duration = Duration::from_secs(30);

/// Waits that double from 1 second to a 10-second cap, each lengthened by up
/// to the same again at random so two Macs do not retry in lockstep: 1–2 s,
/// 2–4 s, 4–8 s, 8–16 s, then 10–20 s. Never gives up on its own.
pub fn waits() -> ExponentialBackoff {
    ExponentialBuilder::new()
        .with_min_delay(Duration::from_secs(1))
        .with_max_delay(Duration::from_secs(10))
        .with_jitter()
        .without_max_times()
        .build()
}

/// A different Mac answered at the address; never connect to it in place of
/// the one that was paired.
#[derive(Debug, thiserror::Error)]
#[error("a different Mac answered at {address}; pair with it deliberately if that is intended")]
pub struct KeyChanged {
    pub address: String,
}

/// The other Mac could not be reached at the network level: the connection
/// was refused, timed out, or the name did not resolve.
#[derive(Debug, thiserror::Error)]
#[error("could not reach {address}")]
pub struct Unreachable {
    pub address: String,
    #[source]
    pub source: std::io::Error,
}

/// Whether an attempt that failed with `error` is worth repeating. Only the
/// connection itself qualifies: unreachable, reset or silent, or a timeout.
/// A local failure, such as the peer file failing to save, stops like any
/// trust, key, permission or setup failure, even though it is also I/O.
pub fn retryable(error: &anyhow::Error) -> bool {
    if error.downcast_ref::<KeyChanged>().is_some() {
        return false;
    }
    error.downcast_ref::<Unreachable>().is_some()
        || error.downcast_ref::<Silent>().is_some()
        || error.downcast_ref::<tokio::time::error::Elapsed>().is_some()
        || matches!(
            error.downcast_ref::<SessionError>(),
            Some(SessionError::Io(_) | SessionError::Closed)
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    #[test]
    fn waits_grow_to_a_cap_and_never_stop() {
        let waits: Vec<_> = waits().take(8).collect();
        assert_eq!(waits.len(), 8);
        for (wait, base) in waits.iter().zip([1, 2, 4, 8, 10, 10, 10, 10]) {
            let base = Duration::from_secs(base);
            assert!(*wait >= base && *wait < base * 2, "{wait:?} for {base:?}");
        }
    }

    #[test]
    fn network_failures_are_retried() {
        let refused = anyhow::Error::from(Unreachable {
            address: "studio.local:24850".into(),
            source: std::io::Error::from(std::io::ErrorKind::ConnectionRefused),
        });
        assert!(retryable(&refused));
        assert!(retryable(&anyhow::Error::from(Silent)));
        assert!(retryable(&anyhow::Error::from(SessionError::Closed)));
        let reset = SessionError::Io(std::io::Error::from(std::io::ErrorKind::ConnectionReset));
        assert!(retryable(&anyhow::Error::from(reset)));
    }

    #[tokio::test(start_paused = true)]
    async fn timeouts_are_retried() {
        let elapsed = tokio::time::timeout(Duration::from_millis(1), std::future::pending::<()>())
            .await
            .unwrap_err();
        assert!(retryable(
            &anyhow::Error::from(elapsed).context("the Noise handshake timed out")
        ));
    }

    #[test]
    fn a_local_file_failure_stops_even_though_it_is_io() {
        let save: anyhow::Result<()> =
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)).context("saving peers.toml");
        assert!(!retryable(&save.unwrap_err()));
        let full: anyhow::Result<()> =
            Err(std::io::Error::from(std::io::ErrorKind::StorageFull)).context("saving peers.toml");
        assert!(!retryable(&full.unwrap_err()));
    }

    #[test]
    fn trust_key_and_setup_failures_stop() {
        let not_paired = anyhow::anyhow!("Studio (ab12) is not paired with this system; pair both systems again");
        assert!(!retryable(&not_paired));
        assert!(!retryable(&anyhow::Error::from(KeyChanged {
            address: "studio.local".into()
        })));
        let tampered = SessionError::Malformed(postcard::Error::DeserializeUnexpectedEnd);
        assert!(!retryable(&anyhow::Error::from(tampered)));
        assert!(!retryable(&anyhow::anyhow!("both systems are set as Host")));
    }

    #[test]
    fn a_key_change_stops_even_with_context_around_it() {
        let changed = anyhow::Error::from(KeyChanged {
            address: "studio.local".into(),
        });
        let wrapped: anyhow::Result<()> = Err(changed).context("connecting");
        assert!(!retryable(&wrapped.unwrap_err()));
    }
}

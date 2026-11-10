//! The part of the AI layer that starts things, and only when asked.
//!
//! Nothing in this crate runs before the first AI request: no thread, no socket, no runtime.
//! The shell calls [`block_on`] from the main thread when a person types an `omsh ai` command
//! or (from S21) a `?` request, and the first call builds a small tokio runtime that lives on
//! the calling thread (`current_thread`: it has no worker threads of its own). A normal
//! session never gets here, which a test checks from outside by looking for sockets and
//! threads (`omsh-tests/tests/ai_lazy.rs`).

use std::future::Future;
use std::sync::{Once, OnceLock};
use std::time::Duration;

use futures::future::{Either, select};
use tokio::runtime::{Builder, Runtime};

use crate::error::{ErrorKind, ProviderError};

static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();
static TLS: Once = Once::new();

/// How often a waiting request looks at the Ctrl+C flag.
const TICK: Duration = Duration::from_millis(50);

/// Chooses the crypto library for HTTPS (`ring`). Done once, before the first HTTP client is
/// built: with no choice made, rustls refuses to build a client.
pub fn prepare_tls() {
    TLS.call_once(|| {
        // Fails only if some other code chose first, which is then fine too.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn runtime() -> Result<&'static Runtime, ProviderError> {
    RUNTIME
        .get_or_init(|| {
            Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|why| ProviderError::new(ErrorKind::Internal, format!("no runtime: {why}")))
}

/// Runs `future` to its end on the calling thread, and gives up with
/// [`ErrorKind::Cancelled`] as soon as `interrupted` says so (it is looked at every 50 ms).
/// Dropping the future closes its connections.
///
/// # Errors
///
/// [`ErrorKind::Cancelled`] when interrupted, [`ErrorKind::Internal`] when the runtime cannot
/// start.
pub fn block_on<F: Future>(
    future: F,
    interrupted: &dyn Fn() -> bool,
) -> Result<F::Output, ProviderError> {
    prepare_tls();
    let runtime = runtime()?;
    runtime.block_on(async {
        let work = std::pin::pin!(future);
        let watch = std::pin::pin!(async {
            loop {
                if interrupted() {
                    break;
                }
                tokio::time::sleep(TICK).await;
            }
        });
        let outcome = match select(work, watch).await {
            Either::Left((output, _)) => Ok(output),
            Either::Right(_) => Err(ProviderError::new(ErrorKind::Cancelled, "interrupted")),
        };
        // The runtime runs only while `block_on` runs. Let the tasks of the HTTP client that
        // are closing their connections finish now, so that no socket outlives the request.
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        outcome
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[test]
    fn a_future_runs_to_its_end() {
        let out = block_on(async { 7 }, &|| false).unwrap();
        assert_eq!(out, 7);
    }

    #[test]
    fn a_timer_works_inside() {
        let out = block_on(
            async {
                tokio::time::sleep(Duration::from_millis(5)).await;
                "done"
            },
            &|| false,
        );
        assert_eq!(out.unwrap(), "done");
    }

    #[test]
    fn ctrl_c_ends_a_wait_that_never_finishes() {
        let flag = AtomicBool::new(false);
        let started = std::time::Instant::now();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(120));
                flag.store(true, Ordering::Relaxed);
            });
            let out = block_on(std::future::pending::<()>(), &|| {
                flag.load(Ordering::Relaxed)
            });
            assert_eq!(out.unwrap_err().kind, ErrorKind::Cancelled);
        });
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_flag_already_set_cancels_at_once() {
        let out = block_on(std::future::pending::<()>(), &|| true);
        assert_eq!(out.unwrap_err().kind, ErrorKind::Cancelled);
    }

    #[test]
    fn the_second_call_reuses_the_runtime() {
        block_on(async {}, &|| false).unwrap();
        block_on(async {}, &|| false).unwrap();
        assert!(RUNTIME.get().is_some());
    }
}

pub mod email_outbox;

use crate::state::SharedState;

pub fn spawn_all(state: SharedState) {
    tokio::spawn(async move {
        loop {
            tracing::info!("starting supervised email outbox worker");
            let worker_state = state.clone();
            match tokio::spawn(email_outbox::run(worker_state)).await {
                Ok(()) => tracing::error!("email outbox worker exited unexpectedly"),
                Err(error) => tracing::error!(
                    panicked = error.is_panic(),
                    error = %error,
                    "email outbox worker task failed"
                ),
            }
            tracing::warn!("restarting email outbox worker in one second");
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });
}

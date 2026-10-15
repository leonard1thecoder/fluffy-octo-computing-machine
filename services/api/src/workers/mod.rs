pub mod email_outbox;

use crate::state::SharedState;

pub fn spawn_all(state: SharedState) {
    tokio::spawn(email_outbox::run(state));
}

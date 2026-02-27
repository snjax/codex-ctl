use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::watch;

use crate::session::state::SessionState;

#[derive(Debug, Clone, Serialize)]
pub struct WaitResult {
    pub state: SessionState,
    pub waited: bool,
    pub waited_sec: f64,
    pub timed_out: bool,
}

/// Wait for the session to reach one of the target states.
/// If timeout is None, waits indefinitely.
/// Dead is always treated as a terminal state.
pub async fn wait_for_state(
    mut state_rx: watch::Receiver<SessionState>,
    target_states: &[SessionState],
    timeout: Option<Duration>,
) -> WaitResult {
    let start = Instant::now();

    loop {
        let current = state_rx.borrow().clone();

        // Target state reached
        if target_states.contains(&current) || current == SessionState::Dead {
            return WaitResult {
                state: current,
                waited: true,
                waited_sec: start.elapsed().as_secs_f64(),
                timed_out: false,
            };
        }

        // Wait for next change or timeout
        let wait_result = match timeout {
            Some(dur) => {
                let remaining = dur.saturating_sub(start.elapsed());
                if remaining.is_zero() {
                    return WaitResult {
                        state: state_rx.borrow().clone(),
                        waited: true,
                        waited_sec: start.elapsed().as_secs_f64(),
                        timed_out: true,
                    };
                }
                tokio::time::timeout(remaining, state_rx.changed()).await
            }
            None => Ok(state_rx.changed().await),
        };

        match wait_result {
            Ok(Ok(())) => continue,
            Ok(Err(_)) => {
                // Channel closed — session dead
                return WaitResult {
                    state: SessionState::Dead,
                    waited: true,
                    waited_sec: start.elapsed().as_secs_f64(),
                    timed_out: false,
                };
            }
            Err(_) => {
                // Timeout
                return WaitResult {
                    state: state_rx.borrow().clone(),
                    waited: true,
                    waited_sec: start.elapsed().as_secs_f64(),
                    timed_out: true,
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_wait_immediate_match() {
        let (tx, rx) = watch::channel(SessionState::Idle);
        let _ = tx; // keep alive
        let result = wait_for_state(
            rx,
            &[SessionState::Idle],
            Some(Duration::from_secs(1)),
        )
        .await;
        assert_eq!(result.state, SessionState::Idle);
        assert!(!result.timed_out);
    }

    #[tokio::test]
    async fn test_wait_timeout() {
        let (tx, rx) = watch::channel(SessionState::Working);
        let _tx = tx; // keep alive
        let result = wait_for_state(
            rx,
            &[SessionState::Idle],
            Some(Duration::from_millis(100)),
        )
        .await;
        assert!(result.timed_out);
        assert_eq!(result.state, SessionState::Working);
    }

    #[tokio::test]
    async fn test_wait_state_change() {
        let (tx, rx) = watch::channel(SessionState::Working);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = tx.send(SessionState::Idle);
        });
        let result = wait_for_state(
            rx,
            &[SessionState::Idle],
            Some(Duration::from_secs(2)),
        )
        .await;
        assert_eq!(result.state, SessionState::Idle);
        assert!(!result.timed_out);
    }

    #[tokio::test]
    async fn test_wait_dead_always_terminal() {
        let (tx, rx) = watch::channel(SessionState::Working);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = tx.send(SessionState::Dead);
        });
        // Waiting for Idle, but Dead should also return
        let result = wait_for_state(
            rx,
            &[SessionState::Idle],
            Some(Duration::from_secs(2)),
        )
        .await;
        assert_eq!(result.state, SessionState::Dead);
        assert!(!result.timed_out);
    }
}

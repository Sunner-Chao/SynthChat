use std::{env, ffi::OsString, future::Future, sync::OnceLock};

use crate::error::AppResult;

static ACP_SESSION_ENV_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub(super) async fn run_with_session_env<F, T>(session_id: &str, future: F) -> AppResult<T>
where
    F: Future<Output = AppResult<T>>,
{
    let _guard = ACP_SESSION_ENV_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await;
    let previous_synthchat = env::var_os("SYNTHCHAT_SESSION_ID");
    let previous_hermes = env::var_os("HERMES_SESSION_ID");
    env::set_var("SYNTHCHAT_SESSION_ID", session_id);
    env::set_var("HERMES_SESSION_ID", session_id);
    let result = future.await;
    restore_env_var("SYNTHCHAT_SESSION_ID", previous_synthchat);
    restore_env_var("HERMES_SESSION_ID", previous_hermes);
    result
}

fn restore_env_var(key: &str, value: Option<OsString>) {
    if let Some(value) = value {
        env::set_var(key, value);
    } else {
        env::remove_var(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn session_env_guard_sets_session_id_and_restores_previous_values() {
        env::set_var("SYNTHCHAT_SESSION_ID", "outer-synthchat");
        env::set_var("HERMES_SESSION_ID", "outer-hermes");

        let seen = run_with_session_env("session-acp-123", async {
            Ok((
                env::var("SYNTHCHAT_SESSION_ID").ok(),
                env::var("HERMES_SESSION_ID").ok(),
            ))
        })
        .await
        .unwrap();

        assert_eq!(seen.0.as_deref(), Some("session-acp-123"));
        assert_eq!(seen.1.as_deref(), Some("session-acp-123"));
        assert_eq!(
            env::var("SYNTHCHAT_SESSION_ID").ok().as_deref(),
            Some("outer-synthchat")
        );
        assert_eq!(
            env::var("HERMES_SESSION_ID").ok().as_deref(),
            Some("outer-hermes")
        );

        env::remove_var("SYNTHCHAT_SESSION_ID");
        env::remove_var("HERMES_SESSION_ID");
    }
}

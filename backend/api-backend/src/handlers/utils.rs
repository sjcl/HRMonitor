use crate::error::AppError;
use common::messages::ConnectionChangeAck;

pub(crate) async fn check_user_exists(db: &sqlx::PgPool, user_id: &str) -> Result<(), AppError> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
        .bind(user_id)
        .fetch_one(db)
        .await?;
    if !exists {
        return Err(AppError::NotFound("User not found".into()));
    }
    Ok(())
}

pub(crate) fn connection_change_applied(ack: &ConnectionChangeAck, user_id: &str) -> bool {
    if !ack.applied {
        tracing::warn!(user_id, error = ?ack.error, "Ingest did not apply");
        return false;
    }

    if ack.stale {
        tracing::info!(
            user_id,
            actual_cv = ?ack.config_version,
            "Ingest ignored stale connection change command"
        );
        return false;
    }

    true
}

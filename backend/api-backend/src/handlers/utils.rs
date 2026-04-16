use crate::auth::AppError;

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

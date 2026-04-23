use axum::extract::FromRequestParts;
use axum::http::request::Parts;

use crate::auth::{AuthenticatedUser, AuthContext, UserIdParam};
use crate::error::AppError;

/// Check whether `auth_user` is allowed to view `target_id`'s heart rate data.
///
/// `heart_rate_visibility` controls access:
/// - `group_default`: follow the group's visibility settings.
/// - `private`: always deny non-self access regardless of group settings.
///
/// Returns `NotFound` if the target user does not exist, `Forbidden` if not
/// allowed.
pub async fn ensure_can_view_user(
    db: &sqlx::PgPool,
    auth_user: &AuthenticatedUser,
    target_id: &str,
) -> Result<(), AppError> {
    if auth_user.id == target_id {
        return Ok(());
    }
    let vis: Option<String> =
        sqlx::query_scalar("SELECT heart_rate_visibility FROM users WHERE id = $1")
            .bind(target_id)
            .fetch_optional(db)
            .await?;
    match vis.as_deref() {
        Some("private") => Err(AppError::Forbidden("Not allowed to view this user".into())),
        Some("group_default") => {
            let shared: bool = sqlx::query_scalar(
                "SELECT EXISTS(
                    SELECT 1 FROM group_members gm1
                    JOIN group_members gm2 ON gm1.group_id = gm2.group_id
                    WHERE gm1.user_id = $1 AND gm1.status = 'active'
                      AND gm2.user_id = $2 AND gm2.status = 'active' AND gm2.sharing = true
                )",
            )
            .bind(&auth_user.id)
            .bind(target_id)
            .fetch_one(db)
            .await?;
            if shared {
                Ok(())
            } else {
                Err(AppError::Forbidden("Not allowed to view this user".into()))
            }
        }
        Some(_) => Err(AppError::Forbidden("Not allowed to view this user".into())),
        None => Err(AppError::NotFound("User not found".into())),
    }
}

/// Returns `(role, sharing)` if `user_id` has an active membership in `group_id`.
pub async fn ensure_active_member(
    db: &sqlx::PgPool,
    group_id: &str,
    user_id: &str,
) -> Result<(String, bool), AppError> {
    let row: Option<(String, bool)> = sqlx::query_as(
        "SELECT role, sharing FROM group_members
         WHERE group_id = $1 AND user_id = $2 AND status = 'active'",
    )
    .bind(group_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?;
    row.ok_or_else(|| AppError::NotFound("Group not found".into()))
}

/// Extractor that combines authentication, user ID resolution (`me` → auth user),
/// and visibility check into a single step. The target user must be viewable by
/// the authenticated user per `ensure_can_view_user`.
pub struct ViewableUserId(pub String);

impl<S: AuthContext> FromRequestParts<S> for ViewableUserId {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let auth_user = parts
            .extensions
            .get::<AuthenticatedUser>()
            .cloned()
            .ok_or_else(|| AppError::Unauthorized("Not authenticated".into()))?;
        let UserIdParam(target_id) = UserIdParam::from_request_parts(parts, state).await?;
        ensure_can_view_user(state.db(), &auth_user, &target_id).await?;
        Ok(ViewableUserId(target_id))
    }
}


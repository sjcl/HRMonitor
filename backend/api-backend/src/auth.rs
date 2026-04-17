use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use std::sync::Arc;

use common::error::AppError;

use crate::AppState;

pub use common::access::{ensure_active_member, ensure_can_view_user};
pub use common::auth::{AuthConfig, AuthContext, AuthenticatedUser, UserIdParam};

pub async fn require_auth(
    state: axum::extract::State<Arc<AppState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, axum::http::StatusCode> {
    common::auth::require_auth::<AppState>(state, req, next).await
}

pub struct ViewableUserId(pub String);

impl FromRequestParts<Arc<AppState>> for ViewableUserId {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let auth_user = parts
            .extensions
            .get::<AuthenticatedUser>()
            .cloned()
            .ok_or_else(|| AppError::Unauthorized("Not authenticated".into()))?;
        let UserIdParam(target_id) = UserIdParam::from_request_parts(parts, state).await?;
        ensure_can_view_user(&state.db, &auth_user, &target_id).await?;
        Ok(ViewableUserId(target_id))
    }
}

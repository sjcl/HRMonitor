use axum::extract::{Path, State};
use axum::{Extension, Json};
use common::time::unix_now_secs;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;

use common::error::AppError;

use crate::AppState;
use crate::auth::{AuthenticatedUser, ensure_active_member};
use crate::models::{
    AcceptInviteRequest, AcceptInviteResponse, CreateGroupRequest, CreateInviteRequest,
    CreateInviteResponse, GroupDetail, GroupListItem, GroupMemberInfo, GroupMemberPreview,
    InviteInfo, InviteListItem, UpdateGroupRequest, UpdateMembershipRequest,
};

const VALID_INVITE_POLICIES: &[&str] = &["group", "group+"];

fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

fn generate_token() -> String {
    use rand::RngExt;
    let bytes: [u8; 32] = rand::rng().random();
    hex::encode(bytes)
}

/// Compute display name for a group from the viewer's perspective.
fn compute_display_name<T>(
    group_name: &Option<String>,
    members: &[T],
    viewer_id: &str,
    get_user_id: impl Fn(&T) -> &str,
    get_display_name: impl Fn(&T) -> &str,
) -> Option<String> {
    if let Some(name) = group_name
        && !name.is_empty()
    {
        return Some(name.clone());
    }
    let others: Vec<&str> = members
        .iter()
        .filter(|m| get_user_id(m) != viewer_id)
        .map(get_display_name)
        .collect();
    match others.len() {
        0 => None,
        1 => Some(others[0].to_string()),
        _ => Some(format!("{}, {}", others[0], others[1])),
    }
}

// --- Helper: fetch active members for a group, ordered stably ---

#[derive(Debug, sqlx::FromRow)]
struct MemberRow {
    user_id: String,
    display_name: String,
    avatar_url: Option<String>,
    role: String,
    sharing: bool,
}

async fn fetch_active_members(
    db: &sqlx::PgPool,
    group_id: &str,
) -> Result<Vec<GroupMemberInfo>, AppError> {
    let rows: Vec<MemberRow> = sqlx::query_as(
        "SELECT gm.user_id, u.display_name, a.provider_image as avatar_url,
                gm.role, gm.sharing
         FROM group_members gm
         JOIN users u ON u.id = gm.user_id
         LEFT JOIN accounts a ON a.user_id = u.id AND a.provider = 'discord'
         WHERE gm.group_id = $1 AND gm.status = 'active'
         ORDER BY gm.created_at, gm.user_id",
    )
    .bind(group_id)
    .fetch_all(db)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| GroupMemberInfo {
            user_id: r.user_id,
            display_name: r.display_name,
            avatar_url: r.avatar_url,
            role: r.role,
            sharing: r.sharing,
        })
        .collect())
}

// =============================================================================
// Group CRUD
// =============================================================================

pub async fn create_group(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Json(body): Json<CreateGroupRequest>,
) -> Result<Json<GroupDetail>, AppError> {
    if let Some(ref policy) = body.invite_policy
        && !VALID_INVITE_POLICIES.contains(&policy.as_str())
    {
        return Err(AppError::BadRequest(
            "invite_policy must be one of: group, group+".into(),
        ));
    }

    let name = body
        .name
        .as_deref()
        .map(|s| crate::validation::validate_optional_name(s, "name"))
        .transpose()?;

    let policy = body.invite_policy.as_deref().unwrap_or("group");

    let mut tx = state.db.begin().await?;

    let (group_id, created_at): (String, i64) = sqlx::query_as(
        "INSERT INTO groups (name, invite_policy)
         VALUES ($1, $2)
         RETURNING id, EXTRACT(EPOCH FROM created_at)::BIGINT as created_at",
    )
    .bind(&name)
    .bind(policy)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO group_members (group_id, user_id, role, sharing, status)
         VALUES ($1, $2, 'owner', true, 'active')",
    )
    .bind(&group_id)
    .bind(&auth_user.id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    let members = fetch_active_members(&state.db, &group_id).await?;
    let display_name = compute_display_name(
        &name,
        &members,
        &auth_user.id,
        |m| &m.user_id,
        |m| &m.display_name,
    );

    Ok(Json(GroupDetail {
        id: group_id,
        name,
        display_name,
        invite_policy: policy.to_string(),
        my_sharing: true,
        my_role: "owner".into(),
        members,
        created_at,
    }))
}

pub async fn list_groups(
    State(state): State<Arc<AppState>>,
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> Result<Json<Vec<GroupListItem>>, AppError> {
    #[derive(sqlx::FromRow)]
    struct GroupRow {
        id: String,
        name: Option<String>,
        invite_policy: String,
        my_role: String,
        my_sharing: bool,
        member_count: i64,
        created_at: i64,
    }

    let rows: Vec<GroupRow> = sqlx::query_as(
        "SELECT g.id, g.name, g.invite_policy,
                gm.role as my_role, gm.sharing as my_sharing,
                (SELECT COUNT(*) FROM group_members
                 WHERE group_id = g.id AND status = 'active') as member_count,
                EXTRACT(EPOCH FROM g.created_at)::BIGINT as created_at
         FROM groups g
         JOIN group_members gm ON gm.group_id = g.id
         WHERE gm.user_id = $1 AND gm.status = 'active'
         ORDER BY g.created_at DESC",
    )
    .bind(&auth_user.id)
    .fetch_all(&state.db)
    .await?;

    if rows.is_empty() {
        return Ok(Json(Vec::new()));
    }

    // Batch-fetch all members for all groups in one query
    let group_ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();

    #[derive(sqlx::FromRow)]
    struct GroupListMemberRow {
        group_id: String,
        user_id: String,
        display_name: String,
        avatar_url: Option<String>,
    }

    let member_rows: Vec<GroupListMemberRow> = sqlx::query_as(
        "SELECT group_id, user_id, display_name, avatar_url
         FROM (
             SELECT gm.group_id, gm.user_id, u.display_name, a.provider_image as avatar_url,
                    ROW_NUMBER() OVER (PARTITION BY gm.group_id ORDER BY gm.created_at, gm.user_id) as rn
             FROM group_members gm
             JOIN users u ON u.id = gm.user_id
             LEFT JOIN accounts a ON a.user_id = u.id AND a.provider = 'discord'
             WHERE gm.group_id = ANY($1) AND gm.status = 'active' AND gm.user_id != $2
         ) sub
         WHERE rn <= 2
         ORDER BY group_id",
    )
    .bind(&group_ids)
    .bind(&auth_user.id)
    .fetch_all(&state.db)
    .await?;

    let mut members_map: HashMap<String, Vec<GroupMemberPreview>> = HashMap::new();
    for r in member_rows {
        members_map
            .entry(r.group_id)
            .or_default()
            .push(GroupMemberPreview {
                user_id: r.user_id,
                display_name: r.display_name,
                avatar_url: r.avatar_url,
            });
    }

    let items = rows
        .into_iter()
        .map(|row| {
            let member_previews = members_map.remove(&row.id).unwrap_or_default();
            let display_name = compute_display_name(
                &row.name,
                &member_previews,
                &auth_user.id,
                |m| &m.user_id,
                |m| &m.display_name,
            );
            GroupListItem {
                id: row.id,
                name: row.name,
                display_name,
                member_count: row.member_count,
                my_sharing: row.my_sharing,
                my_role: row.my_role,
                invite_policy: row.invite_policy,
                member_previews,
                created_at: row.created_at,
            }
        })
        .collect();

    Ok(Json(items))
}

pub async fn get_group(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> Result<Json<GroupDetail>, AppError> {
    let (my_role, my_sharing) = ensure_active_member(&state.db, &id, &auth_user.id).await?;

    #[derive(sqlx::FromRow)]
    struct GroupRow {
        name: Option<String>,
        invite_policy: String,
        created_at: i64,
    }

    let group: GroupRow = sqlx::query_as(
        "SELECT name, invite_policy,
                EXTRACT(EPOCH FROM created_at)::BIGINT as created_at
         FROM groups WHERE id = $1",
    )
    .bind(&id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Group not found".into()))?;

    let members = fetch_active_members(&state.db, &id).await?;
    let display_name = compute_display_name(
        &group.name,
        &members,
        &auth_user.id,
        |m| &m.user_id,
        |m| &m.display_name,
    );

    Ok(Json(GroupDetail {
        id,
        name: group.name,
        display_name,
        invite_policy: group.invite_policy,
        my_sharing,
        my_role,
        members,
        created_at: group.created_at,
    }))
}

pub async fn update_group(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Json(body): Json<UpdateGroupRequest>,
) -> Result<Json<GroupDetail>, AppError> {
    let (role, _) = ensure_active_member(&state.db, &id, &auth_user.id).await?;
    if role != "owner" {
        return Err(AppError::Forbidden(
            "Only the owner can update the group".into(),
        ));
    }

    if let Some(ref policy) = body.invite_policy
        && !VALID_INVITE_POLICIES.contains(&policy.as_str())
    {
        return Err(AppError::BadRequest(
            "invite_policy must be one of: group, group+".into(),
        ));
    }

    let name = body
        .name
        .as_deref()
        .map(|s| crate::validation::validate_optional_name(s, "name"))
        .transpose()?;

    let result = sqlx::query(
        "UPDATE groups SET
            name = COALESCE($1, name),
            invite_policy = COALESCE($2, invite_policy)
         WHERE id = $3",
    )
    .bind(&name)
    .bind(&body.invite_policy)
    .bind(&id)
    .execute(&state.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Group not found".into()));
    }

    get_group(State(state), Path(id), Extension(auth_user)).await
}

pub async fn delete_group(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> Result<axum::http::StatusCode, AppError> {
    let (role, _) = ensure_active_member(&state.db, &id, &auth_user.id).await?;
    if role != "owner" {
        return Err(AppError::Forbidden(
            "Only the owner can delete the group".into(),
        ));
    }

    let result = sqlx::query("DELETE FROM groups WHERE id = $1")
        .bind(&id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Group not found".into()));
    }

    Ok(axum::http::StatusCode::NO_CONTENT)
}

// =============================================================================
// Membership
// =============================================================================

pub async fn update_my_membership(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Json(body): Json<UpdateMembershipRequest>,
) -> Result<axum::http::StatusCode, AppError> {
    ensure_active_member(&state.db, &id, &auth_user.id).await?;

    sqlx::query(
        "UPDATE group_members SET sharing = $1
         WHERE group_id = $2 AND user_id = $3 AND status = 'active'",
    )
    .bind(body.sharing)
    .bind(&id)
    .bind(&auth_user.id)
    .execute(&state.db)
    .await?;

    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn leave_group(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> Result<axum::http::StatusCode, AppError> {
    let (role, _) = ensure_active_member(&state.db, &id, &auth_user.id).await?;

    if role == "owner" {
        // Check if there are other active members
        let other_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM group_members
             WHERE group_id = $1 AND user_id != $2 AND status = 'active'",
        )
        .bind(&id)
        .bind(&auth_user.id)
        .fetch_one(&state.db)
        .await?;

        if other_count > 0 {
            return Err(AppError::Forbidden(
                "Owner cannot leave a group with other members. Delete the group instead.".into(),
            ));
        }

        // Sole owner: delete the group entirely (CASCADE)
        sqlx::query("DELETE FROM groups WHERE id = $1")
            .bind(&id)
            .execute(&state.db)
            .await?;
    } else {
        // Member: logical leave
        sqlx::query(
            "UPDATE group_members
             SET status = 'left', left_at = now(), sharing = false
             WHERE group_id = $1 AND user_id = $2 AND status = 'active'",
        )
        .bind(&id)
        .bind(&auth_user.id)
        .execute(&state.db)
        .await?;
    }

    Ok(axum::http::StatusCode::NO_CONTENT)
}

// =============================================================================
// Invitations
// =============================================================================

pub async fn create_invite(
    State(state): State<Arc<AppState>>,
    Path(group_id): Path<String>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    Json(body): Json<CreateInviteRequest>,
) -> Result<Json<CreateInviteResponse>, AppError> {
    let (role, _) = ensure_active_member(&state.db, &group_id, &auth_user.id).await?;

    // Check invite policy
    let policy: String = sqlx::query_scalar("SELECT invite_policy FROM groups WHERE id = $1")
        .bind(&group_id)
        .fetch_one(&state.db)
        .await?;

    if policy == "group" && role != "owner" {
        return Err(AppError::Forbidden(
            "Only the owner can create invites in this group".into(),
        ));
    }

    let expires_in_hours = body.expires_in_hours.unwrap_or(24 * 7); // 7 days default
    crate::validation::validate_range(expires_in_hours, 1i64, 8760, "expires_in_hours")?;

    if let Some(max_uses) = body.max_uses {
        crate::validation::validate_range(max_uses, 1i32, 1000, "max_uses")?;
    }

    if let Some(ref target_user_id) = body.target_user_id {
        super::utils::check_user_exists(&state.db, target_user_id).await?;
    }

    let token = generate_token();
    let token_hash = hash_token(&token);

    let result = sqlx::query_as(
        "INSERT INTO group_invites (group_id, token_hash, created_by, expires_at, max_uses, target_user_id)
         VALUES ($1, $2, $3, now() + make_interval(hours => $4), $5, $6)
         RETURNING id, EXTRACT(EPOCH FROM expires_at)::BIGINT as expires_at",
    )
    .bind(&group_id)
    .bind(&token_hash)
    .bind(&auth_user.id)
    .bind(expires_in_hours as i32)
    .bind(body.max_uses)
    .bind(&body.target_user_id)
    .fetch_one(&state.db)
    .await;

    // Auto-generated constraint name from: group_invites.target_user_id REFERENCES users(id)
    // See migration/migrations/20260407000000_share_groups.sql:52
    let (invite_id, expires_at): (String, i64) = match result {
        Ok(row) => row,
        Err(sqlx::Error::Database(ref db_err))
            if db_err.is_foreign_key_violation()
                && db_err.constraint() == Some("group_invites_target_user_id_fkey") =>
        {
            return Err(AppError::NotFound("User not found".into()));
        }
        Err(e) => return Err(e.into()),
    };

    Ok(Json(CreateInviteResponse {
        id: invite_id,
        token,
        expires_at,
    }))
}

pub async fn list_invites(
    State(state): State<Arc<AppState>>,
    Path(group_id): Path<String>,
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> Result<Json<Vec<InviteListItem>>, AppError> {
    ensure_active_member(&state.db, &group_id, &auth_user.id).await?;

    #[derive(sqlx::FromRow)]
    struct InviteRow {
        id: String,
        created_by: String,
        created_by_name: String,
        expires_at: i64,
        max_uses: Option<i32>,
        use_count: i32,
        created_at: i64,
    }

    let rows: Vec<InviteRow> = sqlx::query_as(
        "SELECT gi.id, gi.created_by, u.display_name as created_by_name,
                EXTRACT(EPOCH FROM gi.expires_at)::BIGINT as expires_at,
                gi.max_uses, gi.use_count,
                EXTRACT(EPOCH FROM gi.created_at)::BIGINT as created_at
         FROM group_invites gi
         JOIN users u ON u.id = gi.created_by
         WHERE gi.group_id = $1
           AND gi.revoked = false
           AND gi.expires_at > now()
           AND (gi.max_uses IS NULL OR gi.use_count < gi.max_uses)
         ORDER BY gi.created_at DESC",
    )
    .bind(&group_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(
        rows.into_iter()
            .map(|r| InviteListItem {
                id: r.id,
                created_by: r.created_by,
                created_by_name: r.created_by_name,
                expires_at: r.expires_at,
                max_uses: r.max_uses,
                use_count: r.use_count,
                created_at: r.created_at,
            })
            .collect(),
    ))
}

pub async fn revoke_invite(
    State(state): State<Arc<AppState>>,
    Path((group_id, invite_id)): Path<(String, String)>,
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> Result<axum::http::StatusCode, AppError> {
    let (role, _) = ensure_active_member(&state.db, &group_id, &auth_user.id).await?;

    // Check that the invite belongs to this group and can be revoked by this user
    let created_by: Option<String> =
        sqlx::query_scalar("SELECT created_by FROM group_invites WHERE id = $1 AND group_id = $2")
            .bind(&invite_id)
            .bind(&group_id)
            .fetch_optional(&state.db)
            .await?;

    let created_by = created_by.ok_or_else(|| AppError::NotFound("Invite not found".into()))?;

    if role != "owner" && created_by != auth_user.id {
        return Err(AppError::Forbidden(
            "Only the owner or invite creator can revoke this invite".into(),
        ));
    }

    sqlx::query("UPDATE group_invites SET revoked = true WHERE id = $1")
        .bind(&invite_id)
        .execute(&state.db)
        .await?;

    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn get_invite_info(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    Extension(auth_user): Extension<AuthenticatedUser>,
) -> Result<Json<InviteInfo>, AppError> {
    let token_hash = hash_token(&token);

    #[derive(sqlx::FromRow)]
    struct InviteRow {
        group_id: String,
        group_name: Option<String>,
        inviter_name: String,
        expires_at: i64,
        revoked: bool,
        max_uses: Option<i32>,
        use_count: i32,
        target_user_id: Option<String>,
    }

    let row: InviteRow = sqlx::query_as(
        "SELECT gi.group_id, g.name as group_name,
                u.display_name as inviter_name,
                EXTRACT(EPOCH FROM gi.expires_at)::BIGINT as expires_at,
                gi.revoked, gi.max_uses, gi.use_count,
                gi.target_user_id
         FROM group_invites gi
         JOIN groups g ON g.id = gi.group_id
         JOIN users u ON u.id = gi.created_by
         WHERE gi.token_hash = $1",
    )
    .bind(&token_hash)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Invite not found".into()))?;

    let now = unix_now_secs();

    let (valid, reason) = if row.revoked {
        (false, Some("revoked".to_string()))
    } else if row.expires_at <= now {
        (false, Some("expired".to_string()))
    } else if row.max_uses.is_some_and(|max| row.use_count >= max) {
        (false, Some("usage_limit_reached".to_string()))
    } else if row
        .target_user_id
        .as_ref()
        .is_some_and(|tid| tid != &auth_user.id)
    {
        (false, Some("not_for_you".to_string()))
    } else {
        (true, None)
    };

    // Check if already a member
    let already_member: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM group_members
            WHERE group_id = $1 AND user_id = $2 AND status = 'active'
        )",
    )
    .bind(&row.group_id)
    .bind(&auth_user.id)
    .fetch_one(&state.db)
    .await?;

    // Compute display name
    let members = fetch_active_members(&state.db, &row.group_id).await?;
    let group_display_name = compute_display_name(
        &row.group_name,
        &members,
        &auth_user.id,
        |m| &m.user_id,
        |m| &m.display_name,
    );

    Ok(Json(InviteInfo {
        group_name: row.group_name,
        group_display_name,
        group_id: row.group_id,
        inviter_name: row.inviter_name,
        expires_at: row.expires_at,
        valid,
        reason,
        already_member,
    }))
}

pub async fn accept_invite(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    Extension(auth_user): Extension<AuthenticatedUser>,
    body: Option<Json<AcceptInviteRequest>>,
) -> Result<Json<AcceptInviteResponse>, AppError> {
    let req = body
        .map(|Json(v)| v)
        .unwrap_or(AcceptInviteRequest { sharing: true });
    let token_hash = hash_token(&token);

    let mut tx = state.db.begin().await?;

    // Lock the invite row
    #[derive(sqlx::FromRow)]
    struct InviteRow {
        id: String,
        group_id: String,
        revoked: bool,
        max_uses: Option<i32>,
        use_count: i32,
        target_user_id: Option<String>,
        expired: bool,
    }

    let invite: InviteRow = sqlx::query_as(
        "SELECT id, group_id, revoked, max_uses, use_count, target_user_id,
                (expires_at <= now()) as expired
         FROM group_invites
         WHERE token_hash = $1
         FOR UPDATE",
    )
    .bind(&token_hash)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound("Invite not found".into()))?;

    // Validate
    if invite.revoked {
        return Err(AppError::Gone("This invite has been revoked".into()));
    }
    if invite.expired {
        return Err(AppError::Gone("This invite has expired".into()));
    }
    if invite.max_uses.is_some_and(|max| invite.use_count >= max) {
        return Err(AppError::Gone(
            "This invite has reached its usage limit".into(),
        ));
    }
    if invite
        .target_user_id
        .as_ref()
        .is_some_and(|tid| tid != &auth_user.id)
    {
        return Err(AppError::Forbidden("This invite is not for you".into()));
    }

    // Insert or re-activate membership
    let result = sqlx::query(
        "INSERT INTO group_members (group_id, user_id, role, sharing, status, joined_at)
         VALUES ($1, $2, 'member', $3, 'active', now())
         ON CONFLICT (group_id, user_id) DO UPDATE
         SET status = 'active', sharing = $3, left_at = NULL,
             role = 'member', joined_at = now()
         WHERE group_members.status = 'left'",
    )
    .bind(&invite.group_id)
    .bind(&auth_user.id)
    .bind(req.sharing)
    .execute(&mut *tx)
    .await?;

    if result.rows_affected() == 0 {
        // Check if already active
        let is_active: bool = sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM group_members
                WHERE group_id = $1 AND user_id = $2 AND status = 'active'
            )",
        )
        .bind(&invite.group_id)
        .bind(&auth_user.id)
        .fetch_one(&mut *tx)
        .await?;

        if is_active {
            return Err(AppError::Conflict("Already a member of this group".into()));
        }
        return Err(AppError::Internal("Failed to join group".into()));
    }

    // Increment use count
    sqlx::query("UPDATE group_invites SET use_count = use_count + 1 WHERE id = $1")
        .bind(&invite.id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    Ok(Json(AcceptInviteResponse {
        group_id: invite.group_id,
    }))
}

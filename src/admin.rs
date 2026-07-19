use crate::auth::{generate_invite_code, AdminUser, AuthError};
use crate::models::{AdminUserView, AuditView, InviteCodeView, UserRole, UserStatus};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct ListUsersQuery {
    pub status: Option<String>,
}

pub async fn list_users(
    State(state): State<crate::AppState>,
    _admin: AdminUser,
    Query(q): Query<ListUsersQuery>,
) -> Result<Json<Vec<AdminUserView>>, AuthError> {
    if let Some(ref s) = q.status {
        if UserStatus::parse(s).is_none() {
            return Err(AuthError::BadRequest("无效的 status 过滤".into()));
        }
    }
    let users = state
        .db
        .list_users(q.status.as_deref())
        .await
        .map_err(|_| AuthError::Internal)?;
    Ok(Json(users))
}

pub async fn approve_user(
    State(state): State<crate::AppState>,
    admin: AdminUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AuthError> {
    let user = state
        .db
        .get_user_by_id(&id)
        .await
        .map_err(|_| AuthError::Internal)?
        .ok_or_else(|| AuthError::BadRequest("用户不存在".into()))?;

    if user.role == UserRole::Admin && user.id == admin.0.user_id {
        // no-op ok
    }

    if !matches!(
        user.status,
        UserStatus::PendingApproval | UserStatus::Rejected
    ) {
        return Err(AuthError::BadRequest("该用户当前状态不可审核通过".into()));
    }

    let next = if user.totp_enabled {
        UserStatus::Active
    } else {
        UserStatus::ApprovedUnbound
    };

    state
        .db
        .update_user_status(&id, next, Some(admin.0.user_id))
        .await
        .map_err(|_| AuthError::Internal)?;

    let _ = state
        .db
        .insert_audit(&admin.0.user_id, "approve", Some(id), None)
        .await;

    // Refresh any restricted sessions for this user is optional; they re-login.
    tracing::info!(admin = %admin.0.username, target = %id, "user approved");
    Ok(StatusCode::NO_CONTENT)
}

pub async fn reject_user(
    State(state): State<crate::AppState>,
    admin: AdminUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AuthError> {
    let user = state
        .db
        .get_user_by_id(&id)
        .await
        .map_err(|_| AuthError::Internal)?
        .ok_or_else(|| AuthError::BadRequest("用户不存在".into()))?;

    if user.role == UserRole::Admin {
        return Err(AuthError::Forbidden("不能拒绝管理员账号".into()));
    }

    state
        .db
        .set_user_status_simple(&id, UserStatus::Rejected)
        .await
        .map_err(|_| AuthError::Internal)?;

    state.auth.destroy_user_sessions(id);

    let _ = state
        .db
        .insert_audit(&admin.0.user_id, "reject", Some(id), None)
        .await;

    Ok(StatusCode::NO_CONTENT)
}

pub async fn disable_user(
    State(state): State<crate::AppState>,
    admin: AdminUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AuthError> {
    let user = state
        .db
        .get_user_by_id(&id)
        .await
        .map_err(|_| AuthError::Internal)?
        .ok_or_else(|| AuthError::BadRequest("用户不存在".into()))?;

    if user.id == admin.0.user_id {
        return Err(AuthError::BadRequest("不能禁用自己".into()));
    }
    if user.role == UserRole::Admin {
        return Err(AuthError::Forbidden("不能禁用其他管理员".into()));
    }

    state
        .db
        .set_user_status_simple(&id, UserStatus::Disabled)
        .await
        .map_err(|_| AuthError::Internal)?;

    state.auth.destroy_user_sessions(id);
    let _ = state.db.revoke_all_devices(&id).await;

    let _ = state
        .db
        .insert_audit(&admin.0.user_id, "disable", Some(id), None)
        .await;

    // Best-effort force logout for this user only
    let _ = state.chat.tx.send(crate::models::WsServerEvent::ForceLogout {
        user_id: id,
        reason: "账号已被停用".into(),
    });

    Ok(StatusCode::NO_CONTENT)
}

pub async fn enable_user(
    State(state): State<crate::AppState>,
    admin: AdminUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AuthError> {
    let user = state
        .db
        .get_user_by_id(&id)
        .await
        .map_err(|_| AuthError::Internal)?
        .ok_or_else(|| AuthError::BadRequest("用户不存在".into()))?;

    if user.status != UserStatus::Disabled {
        return Err(AuthError::BadRequest("用户未处于停用状态".into()));
    }

    let next = if user.totp_enabled {
        UserStatus::Active
    } else {
        UserStatus::ApprovedUnbound
    };

    state
        .db
        .set_user_status_simple(&id, next)
        .await
        .map_err(|_| AuthError::Internal)?;

    let _ = state
        .db
        .insert_audit(&admin.0.user_id, "enable", Some(id), None)
        .await;

    Ok(StatusCode::NO_CONTENT)
}

pub async fn reset_totp(
    State(state): State<crate::AppState>,
    admin: AdminUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AuthError> {
    let user = state
        .db
        .get_user_by_id(&id)
        .await
        .map_err(|_| AuthError::Internal)?
        .ok_or_else(|| AuthError::BadRequest("用户不存在".into()))?;

    if user.status == UserStatus::Disabled {
        return Err(AuthError::BadRequest("请先启用账号再重置验证器".into()));
    }

    state
        .db
        .reset_totp(&id)
        .await
        .map_err(|_| AuthError::Internal)?;
    let _ = state.db.revoke_all_devices(&id).await;
    state.auth.destroy_user_sessions(id);

    let _ = state
        .db
        .insert_audit(&admin.0.user_id, "reset_totp", Some(id), None)
        .await;

    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_audit(
    State(state): State<crate::AppState>,
    _admin: AdminUser,
) -> Result<Json<Vec<AuditView>>, AuthError> {
    let rows = state
        .db
        .list_audit(100)
        .await
        .map_err(|_| AuthError::Internal)?;
    Ok(Json(rows))
}

// ---------- Invite codes ----------

#[derive(Serialize)]
pub struct CreateInviteResponse {
    pub invite: InviteCodeView,
}

pub async fn create_invite(
    State(state): State<crate::AppState>,
    admin: AdminUser,
) -> Result<Json<CreateInviteResponse>, AuthError> {
    let now = chrono::Utc::now();
    let expires_at =
        now + chrono::Duration::hours(state.config.invite_ttl_hours as i64);

    // Retry a few times on rare unique collisions
    for _ in 0..5 {
        let code = generate_invite_code();
        let id = Uuid::new_v4();
        match state
            .db
            .insert_invite_code(id, &code, &admin.0.user_id, expires_at)
            .await
        {
            Ok(()) => {
                let _ = state
                    .db
                    .insert_audit(
                        &admin.0.user_id,
                        "create_invite",
                        Some(id),
                        Some(&format!(
                            r#"{{"code":"{code}","ttl_hours":{}}}"#,
                            state.config.invite_ttl_hours
                        )),
                    )
                    .await;
                return Ok(Json(CreateInviteResponse {
                    invite: InviteCodeView {
                        id,
                        code,
                        created_by: admin.0.user_id,
                        created_at: now,
                        expires_at,
                        used_at: None,
                        used_by: None,
                        revoked_at: None,
                        status: "unused".into(),
                    },
                }));
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("UNIQUE") || msg.contains("unique") {
                    continue;
                }
                tracing::error!(%e, "create invite failed");
                return Err(AuthError::Internal);
            }
        }
    }
    Err(AuthError::Internal)
}

pub async fn list_invites(
    State(state): State<crate::AppState>,
    _admin: AdminUser,
) -> Result<Json<Vec<InviteCodeView>>, AuthError> {
    let rows = state
        .db
        .list_invite_codes(500)
        .await
        .map_err(|_| AuthError::Internal)?;
    Ok(Json(rows))
}

pub async fn revoke_invite(
    State(state): State<crate::AppState>,
    admin: AdminUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AuthError> {
    let ok = state
        .db
        .revoke_invite_code(&id)
        .await
        .map_err(|_| AuthError::Internal)?;
    if !ok {
        return Err(AuthError::BadRequest("邀请码不存在、已使用或已吊销".into()));
    }
    let _ = state
        .db
        .insert_audit(&admin.0.user_id, "revoke_invite", Some(id), None)
        .await;
    Ok(StatusCode::NO_CONTENT)
}

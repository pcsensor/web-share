use crate::config::Config;
use crate::crypto_seal;
use crate::db::{hash_token, Db};
use crate::models::{
    AuthLevel, ChangePasswordRequest, LoginRequest, NextStep, RecoverRequest, RegisterRequest,
    TotpConfirmRequest, TwoFaVerifyRequest, UserInfo, UserRole, UserRow, UserStatus,
};
use crate::totp;
use axum::{
    extract::{FromRequestParts, Path, State},
    http::{header, request::Parts, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use qrcode::render::svg;
use qrcode::QrCode;
use rand::{Rng, RngExt};
use serde::Serialize;
use std::sync::Arc;
use uuid::Uuid;

const SESSION_COOKIE: &str = "chat_session";
const DEVICE_COOKIE: &str = "chat_device";

#[derive(Clone)]
pub struct Session {
    pub user_id: Uuid,
    pub username: String,
    pub nickname: String,
    pub role: UserRole,
    pub status: UserStatus,
    pub auth_level: AuthLevel,
    pub totp_enabled: bool,
    /// In-memory only: pending secret during TOTP setup (raw bytes).
    pub pending_totp_secret: Option<Vec<u8>>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl Session {
    pub fn to_user_info(&self) -> UserInfo {
        UserInfo {
            id: self.user_id,
            username: self.username.clone(),
            nickname: self.nickname.clone(),
            role: self.role,
            status: self.status,
            auth_level: self.auth_level,
            totp_enabled: self.totp_enabled,
            next_step: next_step(self.status, self.auth_level),
        }
    }
}

pub fn next_step(status: UserStatus, auth_level: AuthLevel) -> NextStep {
    match status {
        UserStatus::PendingApproval => NextStep::WaitApproval,
        UserStatus::Rejected => NextStep::Rejected,
        UserStatus::Disabled => NextStep::Disabled,
        UserStatus::ApprovedUnbound => NextStep::SetupTotp,
        UserStatus::Active => match auth_level {
            AuthLevel::PendingTotpSetup => NextStep::SetupTotp,
            AuthLevel::Pending2fa => NextStep::Verify2fa,
            AuthLevel::Full => NextStep::Chat,
        },
    }
}

#[derive(Clone)]
pub struct AuthState {
    pub config: Config,
    sessions: Arc<DashMap<String, Session>>,
    login_attempts: Arc<DashMap<String, Vec<DateTime<Utc>>>>,
}

impl AuthState {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            sessions: Arc::new(DashMap::new()),
            login_attempts: Arc::new(DashMap::new()),
        }
    }

    pub fn check_rate_limit(&self, key: &str) -> bool {
        let now = Utc::now();
        let window = Duration::seconds(self.config.login_window_secs as i64);
        let mut entry = self.login_attempts.entry(key.to_string()).or_default();
        entry.retain(|t| now.signed_duration_since(*t) < window);
        entry.len() < self.config.login_max_attempts as usize
    }

    pub fn record_failed_login(&self, key: &str) {
        self.login_attempts
            .entry(key.to_string())
            .or_default()
            .push(Utc::now());
    }

    pub fn clear_login_attempts(&self, key: &str) {
        self.login_attempts.remove(key);
    }

    fn random_token() -> String {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        hex::encode(bytes)
    }

    pub fn create_session(&self, mut session: Session) -> (String, Session) {
        let token = Self::random_token();
        let now = Utc::now();
        session.created_at = now;
        let ttl = match session.auth_level {
            AuthLevel::Pending2fa | AuthLevel::PendingTotpSetup => {
                self.config.pending_2fa_ttl_secs
            }
            AuthLevel::Full => self.config.session_ttl_secs,
        };
        session.expires_at = now + Duration::seconds(ttl as i64);
        self.sessions.insert(token.clone(), session.clone());
        (token, session)
    }

    pub fn get_session(&self, token: &str) -> Option<Session> {
        let now = Utc::now();
        if let Some(entry) = self.sessions.get(token) {
            if entry.expires_at > now {
                return Some(entry.clone());
            }
        }
        self.sessions.remove(token);
        None
    }

    pub fn update_session<F>(&self, token: &str, f: F) -> Option<Session>
    where
        F: FnOnce(&mut Session),
    {
        let mut entry = self.sessions.get_mut(token)?;
        if entry.expires_at <= Utc::now() {
            drop(entry);
            self.sessions.remove(token);
            return None;
        }
        f(&mut entry);
        Some(entry.clone())
    }

    pub fn destroy_session(&self, token: &str) {
        self.sessions.remove(token);
    }

    pub fn destroy_user_sessions(&self, user_id: Uuid) {
        self.sessions.retain(|_, s| s.user_id != user_id);
    }

    /// Insert a session under an existing token (e.g. after password change).
    pub fn insert_session_token(&self, token: String, session: Session) {
        self.sessions.insert(token, session);
    }

    pub fn purge_expired(&self) {
        let now = Utc::now();
        self.sessions.retain(|_, s| s.expires_at > now);
    }

    pub fn build_session_cookie(&self, token: &str, max_age_secs: u64) -> Cookie<'static> {
        let mut cookie = Cookie::build((SESSION_COOKIE, token.to_string()))
            .path("/")
            .http_only(true)
            .same_site(SameSite::Strict)
            .max_age(cookie::time::Duration::seconds(max_age_secs as i64))
            .build();
        if self.config.secure_cookie {
            cookie.set_secure(true);
        }
        cookie
    }

    pub fn clear_session_cookie(&self) -> Cookie<'static> {
        let mut cookie = Cookie::build((SESSION_COOKIE, ""))
            .path("/")
            .http_only(true)
            .same_site(SameSite::Strict)
            .max_age(cookie::time::Duration::seconds(0))
            .build();
        if self.config.secure_cookie {
            cookie.set_secure(true);
        }
        cookie
    }

    pub fn build_device_cookie(&self, token: &str) -> Cookie<'static> {
        let max_age = (self.config.device_trust_days as i64) * 24 * 3600;
        let mut cookie = Cookie::build((DEVICE_COOKIE, token.to_string()))
            .path("/")
            .http_only(true)
            .same_site(SameSite::Strict)
            .max_age(cookie::time::Duration::seconds(max_age))
            .build();
        if self.config.secure_cookie {
            cookie.set_secure(true);
        }
        cookie
    }

    pub fn clear_device_cookie(&self) -> Cookie<'static> {
        let mut cookie = Cookie::build((DEVICE_COOKIE, ""))
            .path("/")
            .http_only(true)
            .same_site(SameSite::Strict)
            .max_age(cookie::time::Duration::seconds(0))
            .build();
        if self.config.secure_cookie {
            cookie.set_secure(true);
        }
        cookie
    }
}

/// Any authenticated session (including pending 2FA / TOTP setup).
#[derive(Clone)]
pub struct AuthUser {
    pub user_id: Uuid,
    pub username: String,
    pub nickname: String,
    pub role: UserRole,
    pub status: UserStatus,
    pub auth_level: AuthLevel,
    pub totp_enabled: bool,
    pub token: String,
}

impl AuthUser {
    pub fn to_user_info(&self) -> UserInfo {
        UserInfo {
            id: self.user_id,
            username: self.username.clone(),
            nickname: self.nickname.clone(),
            role: self.role,
            status: self.status,
            auth_level: self.auth_level,
            totp_enabled: self.totp_enabled,
            next_step: next_step(self.status, self.auth_level),
        }
    }
}

/// Full chat access only.
#[derive(Clone)]
pub struct FullUser(pub AuthUser);

/// Admin + full access.
#[derive(Clone)]
pub struct AdminUser(pub AuthUser);

impl FromRequestParts<crate::AppState> for AuthUser {
    type Rejection = AuthError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &crate::AppState,
    ) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        let token = jar
            .get(SESSION_COOKIE)
            .map(|c| c.value().to_string())
            .ok_or(AuthError::Unauthorized)?;

        let session = state
            .auth
            .get_session(&token)
            .ok_or(AuthError::Unauthorized)?;

        Ok(AuthUser {
            user_id: session.user_id,
            username: session.username,
            nickname: session.nickname,
            role: session.role,
            status: session.status,
            auth_level: session.auth_level,
            totp_enabled: session.totp_enabled,
            token,
        })
    }
}

impl FromRequestParts<crate::AppState> for FullUser {
    type Rejection = AuthError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &crate::AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        if user.auth_level != AuthLevel::Full || user.status != UserStatus::Active {
            return Err(AuthError::Forbidden(
                "需要完成身份验证后才能访问".into(),
            ));
        }
        Ok(FullUser(user))
    }
}

impl FromRequestParts<crate::AppState> for AdminUser {
    type Rejection = AuthError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &crate::AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        if user.auth_level != AuthLevel::Full
            || user.status != UserStatus::Active
            || user.role != UserRole::Admin
        {
            return Err(AuthError::Forbidden("需要管理员权限".into()));
        }
        Ok(AdminUser(user))
    }
}

#[derive(Debug)]
pub enum AuthError {
    Unauthorized,
    Forbidden(String),
    RateLimited,
    BadRequest(String),
    Conflict(String),
    Internal,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            AuthError::Unauthorized => (StatusCode::UNAUTHORIZED, "未登录或会话已过期".to_string()),
            AuthError::Forbidden(m) => (StatusCode::FORBIDDEN, m),
            AuthError::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                "尝试过于频繁，请稍后再试".to_string(),
            ),
            AuthError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            AuthError::Conflict(m) => (StatusCode::CONFLICT, m),
            AuthError::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "服务器内部错误".to_string(),
            ),
        };
        (status, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

#[derive(Serialize)]
pub struct AuthResponse {
    pub user: UserInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_codes: Option<Vec<String>>,
}

#[derive(Serialize)]
pub struct TotpSetupResponse {
    pub secret_base32: String,
    pub otpauth_uri: String,
    pub qr_svg: String,
}

// ---------- Bootstrap ----------

pub async fn bootstrap_admin(db: &Db, config: &Config) -> anyhow::Result<()> {
    let admins = db.count_admins().await?;

    if admins == 0 {
        let (Some(username), Some(password)) = (
            config.bootstrap_admin_user.as_ref(),
            config.bootstrap_admin_password.as_ref(),
        ) else {
            tracing::warn!(
                "no admin users and CHAT_BOOTSTRAP_ADMIN_USER/PASSWORD not set — register is open but nobody can approve"
            );
            return Ok(());
        };

        let username_clean = sanitize_username(username)
            .ok_or_else(|| anyhow::anyhow!("invalid bootstrap admin username"))?;
        if password.len() < 8 {
            anyhow::bail!("bootstrap admin password must be at least 8 characters");
        }

        let now = Utc::now();
        let hash = bcrypt::hash(password.as_bytes(), bcrypt::DEFAULT_COST)?;
        let user = UserRow {
            id: Uuid::new_v4(),
            username: username_clean.clone(),
            username_norm: username_clean.to_lowercase(),
            password_hash: hash,
            display_name: username_clean.clone(),
            role: UserRole::Admin,
            // Admin still must bind TOTP before chat/admin APIs (full).
            status: UserStatus::ApprovedUnbound,
            totp_secret_enc: None,
            totp_enabled: false,
            last_totp_step: None,
            created_at: now,
            updated_at: now,
            approved_at: Some(now),
            approved_by: None,
        };
        db.insert_user(&user).await?;
        tracing::info!(
            username = %user.username,
            "bootstrap admin created (must bind TOTP on first login)"
        );
        return Ok(());
    }

    // Emergency recovery (opt-in via env). Bootstrap password only applies at first create
    // unless these flags are set — this is a common source of "can't login admin".
    let username_norm = config
        .bootstrap_admin_user
        .as_ref()
        .and_then(|u| sanitize_username(u))
        .map(|u| u.to_lowercase());

    if config.reset_admin_password {
        let (Some(username_norm), Some(password)) =
            (username_norm.as_ref(), config.bootstrap_admin_password.as_ref())
        else {
            anyhow::bail!(
                "CHAT_RESET_ADMIN_PASSWORD requires CHAT_BOOTSTRAP_ADMIN_USER and CHAT_BOOTSTRAP_ADMIN_PASSWORD"
            );
        };
        if password.len() < 8 {
            anyhow::bail!("bootstrap admin password must be at least 8 characters");
        }
        let Some(user) = db.get_user_by_username_norm(username_norm).await? else {
            anyhow::bail!("CHAT_RESET_ADMIN_PASSWORD: user `{username_norm}` not found");
        };
        if user.role != UserRole::Admin {
            anyhow::bail!("CHAT_RESET_ADMIN_PASSWORD: `{username_norm}` is not an admin");
        }
        let hash = bcrypt::hash(password.as_bytes(), bcrypt::DEFAULT_COST)?;
        db.update_password(&user.id, &hash).await?;
        let _ = db.revoke_all_devices(&user.id).await;
        tracing::warn!(
            username = %user.username,
            "CHAT_RESET_ADMIN_PASSWORD applied — admin password updated from env; disable this flag after restart"
        );
    }

    if config.reset_admin_totp {
        let Some(username_norm) = username_norm.as_ref() else {
            anyhow::bail!("CHAT_RESET_ADMIN_TOTP requires CHAT_BOOTSTRAP_ADMIN_USER");
        };
        let Some(user) = db.get_user_by_username_norm(username_norm).await? else {
            anyhow::bail!("CHAT_RESET_ADMIN_TOTP: user `{username_norm}` not found");
        };
        if user.role != UserRole::Admin {
            anyhow::bail!("CHAT_RESET_ADMIN_TOTP: `{username_norm}` is not an admin");
        }
        db.reset_totp(&user.id).await?;
        let _ = db.revoke_all_devices(&user.id).await;
        tracing::warn!(
            username = %user.username,
            "CHAT_RESET_ADMIN_TOTP applied — admin must re-bind authenticator; disable this flag after restart"
        );
    }

    Ok(())
}

// ---------- Handlers ----------

pub async fn register(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    Json(body): Json<RegisterRequest>,
) -> Result<Json<serde_json::Value>, AuthError> {
    let invite_raw = body
        .invite_code
        .as_deref()
        .map(normalize_invite_code)
        .filter(|s| !s.is_empty());

    // Open registration OR a valid invite path must be allowed.
    // Invite is validated later; without invite, open registration is required.
    if invite_raw.is_none() && !state.config.registration_open {
        return Err(AuthError::Forbidden(
            "当前未开放公开注册，请使用邀请码".into(),
        ));
    }

    let ip = client_ip(&headers);
    let rate_key = format!("reg:{ip}");
    if !state.auth.check_rate_limit(&rate_key) {
        return Err(AuthError::RateLimited);
    }

    let username = sanitize_username(&body.username)
        .ok_or_else(|| AuthError::BadRequest("用户名无效（3–32 位字母数字下划线）".into()))?;
    let display_name = sanitize_nickname(&body.display_name)
        .ok_or_else(|| AuthError::BadRequest("昵称无效（1–24 字符）".into()))?;
    validate_password(&body.password)?;

    let username_norm = username.to_lowercase();
    if state
        .db
        .username_exists(&username_norm)
        .await
        .map_err(|_| AuthError::Internal)?
    {
        state.auth.record_failed_login(&rate_key);
        return Err(AuthError::Conflict("用户名不可用".into()));
    }

    let now = Utc::now();
    let password_hash = bcrypt::hash(body.password.as_bytes(), bcrypt::DEFAULT_COST)
        .map_err(|_| AuthError::Internal)?;

    let via_invite = invite_raw.is_some();
    let status = if via_invite {
        UserStatus::ApprovedUnbound
    } else {
        UserStatus::PendingApproval
    };

    let user = UserRow {
        id: Uuid::new_v4(),
        username: username.clone(),
        username_norm,
        password_hash,
        display_name,
        role: UserRole::User,
        status,
        totp_secret_enc: None,
        totp_enabled: false,
        last_totp_step: None,
        created_at: now,
        updated_at: now,
        approved_at: if via_invite { Some(now) } else { None },
        approved_by: None,
    };

    match state
        .db
        .insert_user_with_invite(&user, invite_raw.as_deref())
        .await
    {
        Ok(true) => {
            state.auth.clear_login_attempts(&rate_key);
            tracing::info!(username = %user.username, "user registered via invite (skip approval)");
            Ok(Json(serde_json::json!({
                "ok": true,
                "via_invite": true,
                "message": "注册成功，请登录并绑定身份验证器"
            })))
        }
        Ok(false) => {
            state.auth.clear_login_attempts(&rate_key);
            tracing::info!(username = %user.username, "user registered (pending approval)");
            Ok(Json(serde_json::json!({
                "ok": true,
                "via_invite": false,
                "message": "注册成功，请等待管理员审核"
            })))
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("invalid_invite") {
                state.auth.record_failed_login(&rate_key);
                return Err(AuthError::BadRequest(
                    "邀请码无效、已使用或已过期".into(),
                ));
            }
            tracing::error!(%e, "register insert failed");
            Err(AuthError::Conflict("用户名不可用".into()))
        }
    }
}

fn normalize_invite_code(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_uppercase())
        .collect()
}

/// Generate a one-time invite code like `A3K9-M2X7-Q4WP`.
pub fn generate_invite_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rng = rand::rng();
    let mut parts = Vec::with_capacity(3);
    for _ in 0..3 {
        let mut part = String::with_capacity(4);
        for _ in 0..4 {
            let idx = rng.random_range(0..ALPHABET.len());
            part.push(ALPHABET[idx] as char);
        }
        parts.push(part);
    }
    parts.join("-")
}

pub async fn login(
    State(state): State<crate::AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Result<(CookieJar, Json<AuthResponse>), AuthError> {
    let ip = client_ip(&headers);
    let rate_key = format!("login:{ip}");
    if !state.auth.check_rate_limit(&rate_key) {
        return Err(AuthError::RateLimited);
    }

    let username = body.username.trim();
    if username.is_empty() || body.password.is_empty() || body.password.len() > 256 {
        state.auth.record_failed_login(&rate_key);
        return Err(AuthError::Unauthorized);
    }

    let username_norm = username.to_lowercase();
    let user = state
        .db
        .get_user_by_username_norm(&username_norm)
        .await
        .map_err(|_| AuthError::Internal)?;

    let Some(user) = user else {
        state.auth.record_failed_login(&rate_key);
        // dummy bcrypt to reduce timing gap slightly
        let _ = bcrypt::hash(b"dummy-password-check", 4);
        return Err(AuthError::Unauthorized);
    };

    let password_ok = bcrypt::verify(body.password.as_bytes(), &user.password_hash).unwrap_or(false);
    if !password_ok {
        state.auth.record_failed_login(&rate_key);
        tracing::warn!(%ip, username = %user.username, "failed login");
        return Err(AuthError::Unauthorized);
    }

    state.auth.clear_login_attempts(&rate_key);

    if user.status == UserStatus::Disabled {
        return Err(AuthError::Forbidden("账号已停用".into()));
    }

    // Determine auth level
    let (auth_level, session_status) = match user.status {
        UserStatus::PendingApproval | UserStatus::Rejected => {
            // Restricted session: can only see status via /api/me
            (AuthLevel::Full, user.status)
        }
        UserStatus::ApprovedUnbound => (AuthLevel::PendingTotpSetup, user.status),
        UserStatus::Active => {
            if !user.totp_enabled {
                (AuthLevel::PendingTotpSetup, UserStatus::ApprovedUnbound)
            } else {
                // Check device trust
                let trusted = is_device_trusted(&state, &user.id, &jar).await?;
                if trusted {
                    (AuthLevel::Full, UserStatus::Active)
                } else {
                    (AuthLevel::Pending2fa, UserStatus::Active)
                }
            }
        }
        UserStatus::Disabled => unreachable!(),
    };

    // For pending/rejected we use a short-ish full-level session that still can't chat
    // because chat requires status=active. next_step guides the UI.
    let session = Session {
        user_id: user.id,
        username: user.username.clone(),
        nickname: user.display_name.clone(),
        role: user.role,
        status: session_status,
        auth_level,
        totp_enabled: user.totp_enabled,
        pending_totp_secret: None,
        created_at: Utc::now(),
        expires_at: Utc::now(), // set in create_session
    };

    let (token, session) = state.auth.create_session(session);
    let max_age = match session.auth_level {
        AuthLevel::Pending2fa | AuthLevel::PendingTotpSetup => state.config.pending_2fa_ttl_secs,
        AuthLevel::Full => {
            if matches!(
                session.status,
                UserStatus::PendingApproval | UserStatus::Rejected
            ) {
                state.config.session_ttl_secs
            } else {
                state.config.session_ttl_secs
            }
        }
    };
    let cookie = state.auth.build_session_cookie(&token, max_age);

    tracing::info!(
        user_id = %session.user_id,
        username = %session.username,
        auth_level = session.auth_level.as_str(),
        status = session.status.as_str(),
        "user logged in"
    );

    Ok((
        jar.add(cookie),
        Json(AuthResponse {
            user: session.to_user_info(),
            recovery_codes: None,
        }),
    ))
}

async fn is_device_trusted(
    state: &crate::AppState,
    user_id: &Uuid,
    jar: &CookieJar,
) -> Result<bool, AuthError> {
    let Some(cookie) = jar.get(DEVICE_COOKIE) else {
        return Ok(false);
    };
    let token = cookie.value();
    if token.is_empty() {
        return Ok(false);
    }
    let token_hash = hash_token(token);
    match state
        .db
        .find_trusted_device(user_id, &token_hash)
        .await
        .map_err(|_| AuthError::Internal)?
    {
        Some((device_id, _)) => {
            let _ = state.db.touch_device(&device_id).await;
            Ok(true)
        }
        None => Ok(false),
    }
}

pub async fn logout(
    State(state): State<crate::AppState>,
    jar: CookieJar,
    user: AuthUser,
) -> (CookieJar, StatusCode) {
    state.auth.destroy_session(&user.token);
    (
        jar.add(state.auth.clear_session_cookie()),
        StatusCode::NO_CONTENT,
    )
}

pub async fn me(user: AuthUser) -> Json<UserInfo> {
    Json(user.to_user_info())
}

/// Public, non-sensitive app settings for UI copy (e.g. trust-device days).
pub async fn public_config(
    State(state): State<crate::AppState>,
) -> Json<crate::models::PublicConfig> {
    Json(crate::models::PublicConfig {
        device_trust_days: state.config.device_trust_days,
        invite_ttl_hours: state.config.invite_ttl_hours,
        registration_open: state.config.registration_open,
    })
}

// ---------- TOTP setup ----------

pub async fn totp_setup_start(
    State(state): State<crate::AppState>,
    user: AuthUser,
) -> Result<Json<TotpSetupResponse>, AuthError> {
    if user.auth_level != AuthLevel::PendingTotpSetup
        && !(user.status == UserStatus::ApprovedUnbound)
    {
        // Allow if status requires setup
        if user.status != UserStatus::ApprovedUnbound {
            return Err(AuthError::Forbidden("当前账号无需绑定验证器".into()));
        }
    }

    let secret = totp::generate_secret();
    let secret_b32 = totp::secret_to_base32(&secret);
    let uri = totp::otpauth_uri(
        &state.config.totp_issuer,
        &user.username,
        &secret,
    );

    state
        .auth
        .update_session(&user.token, |s| {
            s.pending_totp_secret = Some(secret);
            s.auth_level = AuthLevel::PendingTotpSetup;
        })
        .ok_or(AuthError::Unauthorized)?;

    let qr_svg = QrCode::new(uri.as_bytes())
        .map_err(|_| AuthError::Internal)?
        .render::<svg::Color>()
        .min_dimensions(200, 200)
        .dark_color(svg::Color("#e8edf7"))
        .light_color(svg::Color("#121826"))
        .build();

    Ok(Json(TotpSetupResponse {
        secret_base32: secret_b32,
        otpauth_uri: uri,
        qr_svg,
    }))
}

pub async fn totp_setup_confirm(
    State(state): State<crate::AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    user: AuthUser,
    Json(body): Json<TotpConfirmRequest>,
) -> Result<(CookieJar, Json<AuthResponse>), AuthError> {
    let rate_key = format!("2fa:{}:{}", user.user_id, client_ip(&headers));
    if !state.auth.check_rate_limit(&rate_key) {
        return Err(AuthError::RateLimited);
    }

    let session = state
        .auth
        .get_session(&user.token)
        .ok_or(AuthError::Unauthorized)?;
    let secret = session
        .pending_totp_secret
        .clone()
        .ok_or_else(|| AuthError::BadRequest("请先开始绑定验证器".into()))?;

    let now = Utc::now().timestamp() as u64;
    let Some(step) = totp::verify_code(&secret, &body.code, now) else {
        state.auth.record_failed_login(&rate_key);
        return Err(AuthError::BadRequest("验证码不正确".into()));
    };

    let sealed = crypto_seal::seal(&state.config.secret_key, &secret)
        .map_err(|_| AuthError::Internal)?;

    state
        .db
        .enable_totp(&user.user_id, &sealed)
        .await
        .map_err(|_| AuthError::Internal)?;
    state
        .db
        .update_last_totp_step(&user.user_id, step as i64)
        .await
        .map_err(|_| AuthError::Internal)?;

    let recovery_codes = totp::generate_recovery_codes(10);
    let hashes: Vec<String> = recovery_codes
        .iter()
        .map(|c| totp::hash_recovery_code(c))
        .collect();
    state
        .db
        .replace_recovery_codes(&user.user_id, &hashes)
        .await
        .map_err(|_| AuthError::Internal)?;

    // Upgrade session to full
    let upgraded = state
        .auth
        .update_session(&user.token, |s| {
            s.pending_totp_secret = None;
            s.auth_level = AuthLevel::Full;
            s.status = UserStatus::Active;
            s.totp_enabled = true;
            s.expires_at = Utc::now() + Duration::seconds(state.config.session_ttl_secs as i64);
        })
        .ok_or(AuthError::Unauthorized)?;

    // Trust current device
    let (jar, _) = trust_device_for_user(&state, jar, &user.user_id, &headers).await?;

    let cookie = state
        .auth
        .build_session_cookie(&user.token, state.config.session_ttl_secs);

    state.auth.clear_login_attempts(&rate_key);
    tracing::info!(user_id = %user.user_id, "TOTP bound");

    Ok((
        jar.add(cookie),
        Json(AuthResponse {
            user: upgraded.to_user_info(),
            recovery_codes: Some(recovery_codes),
        }),
    ))
}

// ---------- 2FA verify ----------

pub async fn verify_2fa(
    State(state): State<crate::AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    user: AuthUser,
    Json(body): Json<TwoFaVerifyRequest>,
) -> Result<(CookieJar, Json<AuthResponse>), AuthError> {
    if user.auth_level != AuthLevel::Pending2fa {
        return Err(AuthError::Forbidden("当前会话不需要二次验证".into()));
    }

    let rate_key = format!("2fa:{}:{}", user.user_id, client_ip(&headers));
    if !state.auth.check_rate_limit(&rate_key) {
        return Err(AuthError::RateLimited);
    }

    let db_user = state
        .db
        .get_user_by_id(&user.user_id)
        .await
        .map_err(|_| AuthError::Internal)?
        .ok_or(AuthError::Unauthorized)?;

    let secret_enc = db_user
        .totp_secret_enc
        .as_deref()
        .ok_or_else(|| AuthError::Forbidden("未绑定验证器".into()))?;
    let secret = crypto_seal::unseal(&state.config.secret_key, secret_enc)
        .map_err(|_| AuthError::Internal)?;

    let now = Utc::now().timestamp() as u64;
    let Some(step) = totp::verify_code(&secret, &body.code, now) else {
        state.auth.record_failed_login(&rate_key);
        return Err(AuthError::BadRequest("验证码不正确".into()));
    };

    if let Some(last) = db_user.last_totp_step {
        if step as i64 <= last {
            state.auth.record_failed_login(&rate_key);
            return Err(AuthError::BadRequest("验证码已使用，请等待新验证码".into()));
        }
    }

    state
        .db
        .update_last_totp_step(&user.user_id, step as i64)
        .await
        .map_err(|_| AuthError::Internal)?;

    let upgraded = state
        .auth
        .update_session(&user.token, |s| {
            s.auth_level = AuthLevel::Full;
            s.expires_at = Utc::now() + Duration::seconds(state.config.session_ttl_secs as i64);
        })
        .ok_or(AuthError::Unauthorized)?;

    let mut jar = jar;
    if body.trust_device {
        let (j, _) = trust_device_for_user(&state, jar, &user.user_id, &headers).await?;
        jar = j;
    }

    let cookie = state
        .auth
        .build_session_cookie(&user.token, state.config.session_ttl_secs);
    state.auth.clear_login_attempts(&rate_key);

    Ok((
        jar.add(cookie),
        Json(AuthResponse {
            user: upgraded.to_user_info(),
            recovery_codes: None,
        }),
    ))
}

pub async fn recover_2fa(
    State(state): State<crate::AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    user: AuthUser,
    Json(body): Json<RecoverRequest>,
) -> Result<(CookieJar, Json<AuthResponse>), AuthError> {
    if user.auth_level != AuthLevel::Pending2fa {
        return Err(AuthError::Forbidden("当前会话不需要二次验证".into()));
    }

    let rate_key = format!("2fa:{}:{}", user.user_id, client_ip(&headers));
    if !state.auth.check_rate_limit(&rate_key) {
        return Err(AuthError::RateLimited);
    }

    let code_hash = totp::hash_recovery_code(&body.recovery_code);
    let ok = state
        .db
        .consume_recovery_code(&user.user_id, &code_hash)
        .await
        .map_err(|_| AuthError::Internal)?;

    if !ok {
        state.auth.record_failed_login(&rate_key);
        return Err(AuthError::BadRequest("恢复码无效或已使用".into()));
    }

    // Force re-bind TOTP after recovery
    state
        .db
        .reset_totp(&user.user_id)
        .await
        .map_err(|_| AuthError::Internal)?;
    let _ = state.db.revoke_all_devices(&user.user_id).await;

    let upgraded = state
        .auth
        .update_session(&user.token, |s| {
            s.auth_level = AuthLevel::PendingTotpSetup;
            s.status = UserStatus::ApprovedUnbound;
            s.totp_enabled = false;
            s.pending_totp_secret = None;
            s.expires_at = Utc::now() + Duration::seconds(state.config.pending_2fa_ttl_secs as i64);
        })
        .ok_or(AuthError::Unauthorized)?;

    let jar = jar
        .add(state.auth.clear_device_cookie())
        .add(
            state
                .auth
                .build_session_cookie(&user.token, state.config.pending_2fa_ttl_secs),
        );

    state.auth.clear_login_attempts(&rate_key);
    tracing::info!(user_id = %user.user_id, "recovery code used — TOTP reset required");

    Ok((
        jar,
        Json(AuthResponse {
            user: upgraded.to_user_info(),
            recovery_codes: None,
        }),
    ))
}

async fn trust_device_for_user(
    state: &crate::AppState,
    jar: CookieJar,
    user_id: &Uuid,
    headers: &HeaderMap,
) -> Result<(CookieJar, String), AuthError> {
    let token = AuthState::random_token();
    let token_hash = hash_token(&token);
    let id = Uuid::new_v4();
    let expires = Utc::now() + Duration::days(state.config.device_trust_days as i64);
    let label = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.chars().take(120).collect::<String>());

    state
        .db
        .insert_trusted_device(id, user_id, &token_hash, label.as_deref(), expires)
        .await
        .map_err(|_| AuthError::Internal)?;

    Ok((jar.add(state.auth.build_device_cookie(&token)), token))
}

// ---------- Security: devices / password ----------

pub async fn list_devices(
    State(state): State<crate::AppState>,
    jar: CookieJar,
    user: FullUser,
) -> Result<Json<Vec<crate::models::DeviceView>>, AuthError> {
    let current_hash = jar
        .get(DEVICE_COOKIE)
        .map(|c| hash_token(c.value()));
    let devices = state
        .db
        .list_devices(&user.0.user_id, current_hash.as_deref())
        .await
        .map_err(|_| AuthError::Internal)?;
    Ok(Json(devices))
}

pub async fn revoke_device(
    State(state): State<crate::AppState>,
    user: FullUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AuthError> {
    let ok = state
        .db
        .revoke_device(&user.0.user_id, &id)
        .await
        .map_err(|_| AuthError::Internal)?;
    if !ok {
        return Err(AuthError::BadRequest("设备不存在".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn change_password(
    State(state): State<crate::AppState>,
    jar: CookieJar,
    user: FullUser,
    Json(body): Json<ChangePasswordRequest>,
) -> Result<(CookieJar, StatusCode), AuthError> {
    validate_password(&body.new_password)?;

    let db_user = state
        .db
        .get_user_by_id(&user.0.user_id)
        .await
        .map_err(|_| AuthError::Internal)?
        .ok_or(AuthError::Unauthorized)?;

    let ok = bcrypt::verify(body.current_password.as_bytes(), &db_user.password_hash)
        .unwrap_or(false);
    if !ok {
        return Err(AuthError::BadRequest("当前密码不正确".into()));
    }

    let hash = bcrypt::hash(body.new_password.as_bytes(), bcrypt::DEFAULT_COST)
        .map_err(|_| AuthError::Internal)?;
    state
        .db
        .update_password(&user.0.user_id, &hash)
        .await
        .map_err(|_| AuthError::Internal)?;

    // Revoke all other sessions and devices
    let current_token = user.0.token.clone();
    state.auth.destroy_user_sessions(user.0.user_id);
    // Re-insert current session
    let session = Session {
        user_id: user.0.user_id,
        username: user.0.username.clone(),
        nickname: user.0.nickname.clone(),
        role: user.0.role,
        status: user.0.status,
        auth_level: AuthLevel::Full,
        totp_enabled: user.0.totp_enabled,
        pending_totp_secret: None,
        created_at: Utc::now(),
        expires_at: Utc::now() + Duration::seconds(state.config.session_ttl_secs as i64),
    };
    state
        .auth
        .insert_session_token(current_token.clone(), session);

    let _ = state.db.revoke_all_devices(&user.0.user_id).await;

    Ok((
        jar.add(state.auth.clear_device_cookie())
            .add(
                state
                    .auth
                    .build_session_cookie(&current_token, state.config.session_ttl_secs),
            ),
        StatusCode::NO_CONTENT,
    ))
}

// ---------- Helpers ----------

pub fn client_ip(headers: &HeaderMap) -> String {
    if let Some(v) = headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return v;
    }
    if let Some(v) = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return v;
    }
    "unknown".into()
}

fn sanitize_username(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let len = trimmed.chars().count();
    if len < 3 || len > 32 {
        return None;
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    Some(trimmed.to_string())
}

pub fn sanitize_nickname(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 24 {
        return None;
    }
    if trimmed.chars().any(|c| c.is_control() || c == '\u{202E}') {
        return None;
    }
    Some(trimmed.to_string())
}

fn validate_password(password: &str) -> Result<(), AuthError> {
    if password.len() < 8 {
        return Err(AuthError::BadRequest("密码至少 8 位".into()));
    }
    if password.len() > 128 {
        return Err(AuthError::BadRequest("密码过长".into()));
    }
    Ok(())
}

/// Optional helper for websocket upgrade auth from cookie header.
pub fn session_from_cookie_header(
    auth: &AuthState,
    cookie_header: Option<&header::HeaderValue>,
) -> Option<Session> {
    let header = cookie_header?.to_str().ok()?;
    for part in header.split(';') {
        let part = part.trim();
        if let Some(val) = part.strip_prefix(&format!("{SESSION_COOKIE}=")) {
            let session = auth.get_session(val)?;
            if session.auth_level == AuthLevel::Full && session.status == UserStatus::Active {
                return Some(session);
            }
            return None;
        }
    }
    None
}

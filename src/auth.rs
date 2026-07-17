use crate::config::Config;
use crate::models::UserInfo;
use axum::{
    extract::{FromRequestParts, State},
    http::{header, request::Parts, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use rand::Rng;
use serde::Serialize;
use std::sync::Arc;
use uuid::Uuid;

const SESSION_COOKIE: &str = "chat_session";

#[derive(Clone)]
pub struct Session {
    pub user_id: Uuid,
    pub nickname: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct AuthState {
    pub config: Config,
    /// token -> session
    sessions: Arc<DashMap<String, Session>>,
    /// client_ip -> attempt timestamps
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

    pub fn verify_password(&self, password: &str) -> bool {
        bcrypt::verify(password, &self.config.password_hash).unwrap_or(false)
    }

    pub fn check_rate_limit(&self, ip: &str) -> bool {
        let now = Utc::now();
        let window = Duration::seconds(self.config.login_window_secs as i64);
        let mut entry = self.login_attempts.entry(ip.to_string()).or_default();
        entry.retain(|t| now.signed_duration_since(*t) < window);
        entry.len() < self.config.login_max_attempts as usize
    }

    pub fn record_failed_login(&self, ip: &str) {
        self.login_attempts
            .entry(ip.to_string())
            .or_default()
            .push(Utc::now());
    }

    pub fn clear_login_attempts(&self, ip: &str) {
        self.login_attempts.remove(ip);
    }

    pub fn create_session(&self, nickname: &str) -> (String, Session) {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let token = hex::encode(bytes);

        let now = Utc::now();
        let session = Session {
            user_id: Uuid::new_v4(),
            nickname: nickname.to_string(),
            created_at: now,
            expires_at: now + Duration::seconds(self.config.session_ttl_secs as i64),
        };
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

    pub fn destroy_session(&self, token: &str) {
        self.sessions.remove(token);
    }

    pub fn purge_expired(&self) {
        let now = Utc::now();
        self.sessions.retain(|_, s| s.expires_at > now);
    }

    pub fn build_session_cookie(&self, token: &str) -> Cookie<'static> {
        let mut cookie = Cookie::build((SESSION_COOKIE, token.to_string()))
            .path("/")
            .http_only(true)
            .same_site(SameSite::Strict)
            .max_age(cookie::time::Duration::seconds(
                self.config.session_ttl_secs as i64,
            ))
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
}

#[derive(Clone)]
pub struct AuthUser {
    pub user_id: Uuid,
    pub nickname: String,
    pub token: String,
}

impl From<AuthUser> for UserInfo {
    fn from(u: AuthUser) -> Self {
        UserInfo {
            id: u.user_id,
            nickname: u.nickname,
        }
    }
}

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
            nickname: session.nickname,
            token,
        })
    }
}

#[derive(Debug)]
pub enum AuthError {
    Unauthorized,
    RateLimited,
    BadRequest(String),
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            AuthError::Unauthorized => (StatusCode::UNAUTHORIZED, "未登录或会话已过期".to_string()),
            AuthError::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                "登录尝试过于频繁，请稍后再试".to_string(),
            ),
            AuthError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
        };
        (status, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub user: UserInfo,
}

pub async fn login(
    State(state): State<crate::AppState>,
    jar: CookieJar,
    headers: axum::http::HeaderMap,
    Json(body): Json<crate::models::LoginRequest>,
) -> Result<(CookieJar, Json<LoginResponse>), AuthError> {
    let ip = client_ip(&headers);

    if !state.auth.check_rate_limit(&ip) {
        return Err(AuthError::RateLimited);
    }

    let nickname = sanitize_nickname(&body.nickname)
        .ok_or_else(|| AuthError::BadRequest("昵称无效（1-24 字符，不含特殊控制符）".into()))?;

    if body.password.is_empty() || body.password.len() > 256 {
        state.auth.record_failed_login(&ip);
        return Err(AuthError::Unauthorized);
    }

    if !state.auth.verify_password(&body.password) {
        state.auth.record_failed_login(&ip);
        tracing::warn!(%ip, "failed login attempt");
        return Err(AuthError::Unauthorized);
    }

    state.auth.clear_login_attempts(&ip);
    let (token, session) = state.auth.create_session(&nickname);
    let cookie = state.auth.build_session_cookie(&token);

    tracing::info!(user_id = %session.user_id, %nickname, "user logged in");

    Ok((
        jar.add(cookie),
        Json(LoginResponse {
            user: UserInfo {
                id: session.user_id,
                nickname: session.nickname,
            },
        }),
    ))
}

pub async fn logout(
    State(state): State<crate::AppState>,
    jar: CookieJar,
    user: AuthUser,
) -> (CookieJar, StatusCode) {
    state.auth.destroy_session(&user.token);
    (jar.add(state.auth.clear_session_cookie()), StatusCode::NO_CONTENT)
}

pub async fn me(user: AuthUser) -> Json<UserInfo> {
    Json(user.into())
}

fn client_ip(headers: &axum::http::HeaderMap) -> String {
    // Prefer direct connection IP; X-Forwarded-For only trustworthy behind known reverse proxy.
    // We read X-Real-IP / first X-Forwarded-For when present (document in deploy guide).
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

fn sanitize_nickname(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 24 {
        return None;
    }
    if trimmed.chars().any(|c| c.is_control() || c == '\u{202E}') {
        return None;
    }
    Some(trimmed.to_string())
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
            return auth.get_session(val);
        }
    }
    None
}

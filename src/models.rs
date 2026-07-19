use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Text,
    Image,
    Video,
    File,
    System,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileMeta {
    pub id: Uuid,
    pub name: String,
    pub size: u64,
    pub mime: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    pub user_id: Uuid,
    pub nickname: String,
    pub kind: MessageKind,
    pub content: String,
    pub file: Option<FileMeta>,
    pub ts: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UserRole {
    User,
    Admin,
}

impl UserRole {
    pub fn as_str(self) -> &'static str {
        match self {
            UserRole::User => "user",
            UserRole::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(UserRole::User),
            "admin" => Some(UserRole::Admin),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UserStatus {
    PendingApproval,
    Rejected,
    ApprovedUnbound,
    Active,
    Disabled,
}

impl UserStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            UserStatus::PendingApproval => "pending_approval",
            UserStatus::Rejected => "rejected",
            UserStatus::ApprovedUnbound => "approved_unbound",
            UserStatus::Active => "active",
            UserStatus::Disabled => "disabled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending_approval" => Some(UserStatus::PendingApproval),
            "rejected" => Some(UserStatus::Rejected),
            "approved_unbound" => Some(UserStatus::ApprovedUnbound),
            "active" => Some(UserStatus::Active),
            "disabled" => Some(UserStatus::Disabled),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthLevel {
    /// Password ok, waiting for TOTP on unfamiliar device.
    #[serde(rename = "pending_2fa")]
    Pending2fa,
    /// Must bind authenticator before chat access.
    PendingTotpSetup,
    /// Full chat access.
    Full,
}

impl AuthLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthLevel::Pending2fa => "pending_2fa",
            AuthLevel::PendingTotpSetup => "pending_totp_setup",
            AuthLevel::Full => "full",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct UserInfo {
    pub id: Uuid,
    pub username: String,
    pub nickname: String,
    pub role: UserRole,
    pub status: UserStatus,
    pub auth_level: AuthLevel,
    pub totp_enabled: bool,
    /// Frontend routing hint.
    pub next_step: NextStep,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NextStep {
    WaitApproval,
    Rejected,
    Disabled,
    SetupTotp,
    #[serde(rename = "verify_2fa")]
    Verify2fa,
    Chat,
    None,
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
    pub display_name: String,
    /// Optional one-time invite code — skips admin approval when valid.
    #[serde(default)]
    pub invite_code: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct TotpConfirmRequest {
    pub code: String,
}

#[derive(Debug, Deserialize)]
pub struct TwoFaVerifyRequest {
    pub code: String,
    #[serde(default)]
    pub trust_device: bool,
}

#[derive(Debug, Deserialize)]
pub struct RecoverRequest {
    pub recovery_code: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub trust_device: bool,
}

#[derive(Debug, Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Debug, Deserialize)]
pub struct TextMessageRequest {
    pub content: String,
}

/// Messages pushed over WebSocket.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsServerEvent {
    Message { message: Message },
    History { messages: Vec<Message> },
    Presence { online: usize },
    Error { message: String },
    /// Force a specific user to log out (e.g. account disabled).
    ForceLogout { user_id: Uuid, reason: String },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsClientEvent {
    Ping,
    Text { content: String },
}

#[derive(Clone, Debug)]
pub struct UserRow {
    pub id: Uuid,
    pub username: String,
    pub username_norm: String,
    pub password_hash: String,
    pub display_name: String,
    pub role: UserRole,
    pub status: UserStatus,
    pub totp_secret_enc: Option<String>,
    pub totp_enabled: bool,
    pub last_totp_step: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub approved_at: Option<DateTime<Utc>>,
    pub approved_by: Option<Uuid>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AdminUserView {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    pub role: UserRole,
    pub status: UserStatus,
    pub totp_enabled: bool,
    pub created_at: DateTime<Utc>,
    pub approved_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DeviceView {
    pub id: Uuid,
    pub label: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub current: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct AuditView {
    pub id: Uuid,
    pub admin_id: Uuid,
    pub action: String,
    pub target_id: Option<Uuid>,
    pub meta_json: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Non-sensitive settings exposed to the frontend (no auth required).
#[derive(Clone, Debug, Serialize)]
pub struct PublicConfig {
    pub device_trust_days: u64,
    pub invite_ttl_hours: u64,
    pub registration_open: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct InviteCodeView {
    pub id: Uuid,
    pub code: String,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
    pub used_by: Option<Uuid>,
    pub revoked_at: Option<DateTime<Utc>>,
    /// unused | used | revoked | expired
    pub status: String,
}

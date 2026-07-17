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

#[derive(Clone, Debug, Serialize)]
pub struct UserInfo {
    pub id: Uuid,
    pub nickname: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub password: String,
    pub nickname: String,
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
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsClientEvent {
    Ping,
    Text { content: String },
}

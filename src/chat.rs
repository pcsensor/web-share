use crate::auth::{session_from_cookie_header, AuthUser};
use crate::db::Db;
use crate::models::{
    FileMeta, Message, MessageKind, TextMessageRequest, WsClientEvent, WsServerEvent,
};
use axum::{
    extract::{
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::broadcast;
use uuid::Uuid;

#[derive(Clone)]
pub struct ChatHub {
    db: Db,
    pub(crate) tx: broadcast::Sender<WsServerEvent>,
    online: Arc<std::sync::atomic::AtomicUsize>,
    max_message_len: usize,
}

impl ChatHub {
    pub fn new(db: Db, max_message_len: usize) -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            db,
            tx,
            online: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            max_message_len,
        }
    }

    pub async fn history_snapshot(&self) -> Vec<Message> {
        match self.db.list_messages().await {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(%e, "failed to load message history");
                Vec::new()
            }
        }
    }

    pub fn online_count(&self) -> usize {
        self.online.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WsServerEvent> {
        self.tx.subscribe()
    }

    async fn push_message(&self, message: Message) -> Result<(), String> {
        self.db
            .insert_message(&message)
            .await
            .map_err(|e| {
                tracing::error!(%e, "failed to persist message");
                "消息保存失败".to_string()
            })?;
        let _ = self.tx.send(WsServerEvent::Message {
            message: message.clone(),
        });
        Ok(())
    }

    pub async fn post_text(
        &self,
        user_id: Uuid,
        nickname: &str,
        content: &str,
    ) -> Result<Message, String> {
        let content = content.trim();
        if content.is_empty() {
            return Err("消息不能为空".into());
        }
        if content.chars().count() > self.max_message_len {
            return Err(format!("消息过长（最多 {} 字符）", self.max_message_len));
        }
        let content: String = content
            .chars()
            .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
            .collect();

        let message = Message {
            id: Uuid::new_v4(),
            user_id,
            nickname: nickname.to_string(),
            kind: MessageKind::Text,
            content,
            file: None,
            ts: Utc::now(),
        };
        self.push_message(message.clone()).await?;
        Ok(message)
    }

    /// Broadcast a file message that was already persisted with the file row.
    pub fn broadcast_message(&self, message: Message) {
        let _ = self.tx.send(WsServerEvent::Message { message });
    }

    pub async fn post_file_message(
        &self,
        user_id: Uuid,
        nickname: &str,
        kind: MessageKind,
        caption: String,
        file: FileMeta,
    ) -> Result<Message, String> {
        // Used only when file+message not already in one TX — prefer insert_file_and_message
        let message = Message {
            id: Uuid::new_v4(),
            user_id,
            nickname: nickname.to_string(),
            kind,
            content: caption,
            file: Some(file),
            ts: Utc::now(),
        };
        self.push_message(message.clone()).await?;
        Ok(message)
    }
}

pub async fn get_messages(
    State(state): State<crate::AppState>,
    _user: AuthUser,
) -> Json<Vec<Message>> {
    Json(state.chat.history_snapshot().await)
}

pub async fn post_text(
    State(state): State<crate::AppState>,
    user: AuthUser,
    Json(body): Json<TextMessageRequest>,
) -> Result<Json<Message>, (axum::http::StatusCode, Json<serde_json::Value>)> {
    match state
        .chat
        .post_text(user.user_id, &user.nickname, &body.content)
        .await
    {
        Ok(m) => Ok(Json(m)),
        Err(e) => {
            let status = if e.contains("保存") {
                axum::http::StatusCode::INTERNAL_SERVER_ERROR
            } else {
                axum::http::StatusCode::BAD_REQUEST
            };
            Err((status, Json(serde_json::json!({ "error": e }))))
        }
    }
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<crate::AppState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = session_from_cookie_header(&state.auth, headers.get(axum::http::header::COOKIE));
    match session {
        Some(session) => ws.on_upgrade(move |socket| handle_socket(socket, state, session)),
        None => axum::http::StatusCode::UNAUTHORIZED.into_response(),
    }
}

async fn handle_socket(
    socket: WebSocket,
    state: crate::AppState,
    session: crate::auth::Session,
) {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = state.chat.subscribe();

    let online = state
        .chat
        .online
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        + 1;
    let _ = state.chat.tx.send(WsServerEvent::Presence { online });

    let history = state.chat.history_snapshot().await;
    let hello = [
        WsServerEvent::History { messages: history },
        WsServerEvent::Presence {
            online: state.chat.online_count(),
        },
    ];
    for ev in hello {
        if let Ok(text) = serde_json::to_string(&ev) {
            if sender.send(WsMessage::Text(text.into())).await.is_err() {
                decrement_online(&state);
                return;
            }
        }
    }

    let user_id = session.user_id;
    let nickname = session.nickname.clone();
    let chat = state.chat.clone();
    let max_len = chat.max_message_len;

    let mut send_task = tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            match serde_json::to_string(&event) {
                Ok(text) => {
                    if sender.send(WsMessage::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                Err(_) => continue,
            }
        }
    });

    let chat2 = chat.clone();
    let nickname2 = nickname.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                WsMessage::Text(text) => {
                    let Ok(event) = serde_json::from_str::<WsClientEvent>(&text) else {
                        continue;
                    };
                    match event {
                        WsClientEvent::Ping => {}
                        WsClientEvent::Text { content } => {
                            if content.chars().count() > max_len {
                                continue;
                            }
                            let _ = chat2.post_text(user_id, &nickname2, &content).await;
                        }
                    }
                }
                WsMessage::Close(_) => break,
                WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Binary(_) => {}
            }
        }
    });

    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }

    decrement_online(&state);
    tracing::debug!(%user_id, %nickname, "ws disconnected");
}

fn decrement_online(state: &crate::AppState) {
    let prev = state
        .chat
        .online
        .fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |n| Some(n.saturating_sub(1)),
        )
        .unwrap_or(1);
    let online = prev.saturating_sub(1);
    let _ = state.chat.tx.send(WsServerEvent::Presence { online });
}

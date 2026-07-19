use crate::auth::FullUser;
use crate::db::{Db, StoredFile};
use crate::models::{FileMeta, Message, MessageKind};
use axum::{
    body::Body,
    extract::{Multipart, Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use serde::Serialize;
use std::path::{Path as FsPath, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

#[derive(Clone)]
pub struct FileStore {
    root: PathBuf,
    max_file_size: usize,
    db: Db,
}

impl FileStore {
    pub fn new(root: PathBuf, max_file_size: usize, db: Db) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            max_file_size,
            db,
        })
    }

    pub async fn get(&self, id: &Uuid) -> Option<StoredFile> {
        match self.db.get_file(id).await {
            Ok(Some(mut f)) => {
                f.path = self.db.absolute_file_path(&f);
                Some(f)
            }
            Ok(None) => None,
            Err(e) => {
                tracing::error!(%e, %id, "get_file failed");
                None
            }
        }
    }

    fn safe_display_name(name: &str) -> String {
        let name = name.trim();
        let name = FsPath::new(name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("file");
        let cleaned: String = name
            .chars()
            .filter(|c| !c.is_control() && *c != '/' && *c != '\\')
            .take(180)
            .collect();
        if cleaned.is_empty() {
            "file".into()
        } else {
            cleaned
        }
    }

    fn classify_kind(mime: &str) -> MessageKind {
        if mime.starts_with("image/") {
            MessageKind::Image
        } else if mime.starts_with("video/") {
            MessageKind::Video
        } else {
            MessageKind::File
        }
    }
}

#[derive(Serialize)]
pub struct UploadResponse {
    pub message_id: Uuid,
    pub file: FileMeta,
    pub kind: MessageKind,
}

pub async fn upload(
    State(state): State<crate::AppState>,
    user: FullUser,
    mut multipart: Multipart,
) -> Result<Json<UploadResponse>, (StatusCode, Json<serde_json::Value>)> {
    let user = user.0;
    let mut filename: Option<String> = None;
    let mut content_type: Option<String> = None;
    let mut caption = String::new();
    let mut data: Option<Vec<u8>> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| bad_request(format!("multipart 错误: {e}")))?
    {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "caption" => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| bad_request(format!("读取 caption 失败: {e}")))?;
                caption = text.chars().take(500).collect();
            }
            "file" => {
                filename = field.file_name().map(FileStore::safe_display_name);
                content_type = field
                    .content_type()
                    .map(|s| s.to_string())
                    .or_else(|| Some("application/octet-stream".into()));

                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| bad_request(format!("读取文件失败: {e}")))?;

                if bytes.len() > state.files.max_file_size {
                    return Err((
                        StatusCode::PAYLOAD_TOO_LARGE,
                        Json(serde_json::json!({
                            "error": format!(
                                "文件过大，最大 {} MB",
                                state.files.max_file_size / 1024 / 1024
                            )
                        })),
                    ));
                }
                if bytes.is_empty() {
                    return Err(bad_request("空文件".into()));
                }
                data = Some(bytes.to_vec());
            }
            _ => {
                let _ = field.bytes().await;
            }
        }
    }

    let data = data.ok_or_else(|| bad_request("缺少 file 字段".into()))?;
    let display_name = filename.unwrap_or_else(|| "file".into());
    let mime = content_type.unwrap_or_else(|| {
        mime_guess::from_path(&display_name)
            .first_or_octet_stream()
            .essence_str()
            .to_string()
    });
    let mime = sniff_mime(&data, &mime);

    let id = Uuid::new_v4();
    let stored_name = format!("{id}.bin");
    let path = state.files.root.join(&stored_name);

    let canonical_root = fs::canonicalize(&state.files.root)
        .await
        .map_err(|_| internal("存储目录不可用"))?;

    let mut file = fs::File::create(&path)
        .await
        .map_err(|_| internal("无法创建文件"))?;
    if file.write_all(&data).await.is_err() || file.flush().await.is_err() {
        let _ = fs::remove_file(&path).await;
        return Err(internal("写入文件失败"));
    }

    if let Ok(canon) = fs::canonicalize(&path).await {
        if !canon.starts_with(&canonical_root) {
            let _ = fs::remove_file(&path).await;
            return Err(internal("非法路径"));
        }
    }

    let now = Utc::now();
    let meta = FileMeta {
        id,
        name: display_name,
        size: data.len() as u64,
        mime: mime.clone(),
    };

    let kind = FileStore::classify_kind(&mime);
    let message = Message {
        id: Uuid::new_v4(),
        user_id: user.user_id,
        nickname: user.nickname.clone(),
        kind: kind.clone(),
        content: caption,
        file: Some(meta.clone()),
        ts: now,
    };

    let stored = StoredFile {
        meta: meta.clone(),
        path: path.clone(),
        uploader: user.user_id,
        created_at: now,
    };

    if let Err(e) = state.db.insert_file_and_message(&stored, &message).await {
        tracing::error!(%e, "failed to persist file+message");
        let _ = fs::remove_file(&path).await;
        return Err(internal("元数据保存失败"));
    }

    state.chat.broadcast_message(message.clone());

    Ok(Json(UploadResponse {
        message_id: message.id,
        file: meta,
        kind,
    }))
}

pub async fn download(
    State(state): State<crate::AppState>,
    _user: FullUser,
    Path(id): Path<Uuid>,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    serve_file(&state, id, true).await
}

pub async fn preview(
    State(state): State<crate::AppState>,
    _user: FullUser,
    Path(id): Path<Uuid>,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    let stored = state
        .files
        .get(&id)
        .await
        .ok_or_else(|| not_found("文件不存在或已过期"))?;
    let mime = &stored.meta.mime;
    if !(mime.starts_with("image/") || mime.starts_with("video/")) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "该类型不支持在线预览，请下载" })),
        ));
    }
    serve_file(&state, id, false).await
}

async fn serve_file(
    state: &crate::AppState,
    id: Uuid,
    as_attachment: bool,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    let stored = state
        .files
        .get(&id)
        .await
        .ok_or_else(|| not_found("文件不存在或已过期"))?;

    let data = fs::read(&stored.path)
        .await
        .map_err(|_| not_found("文件已丢失"))?;

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&stored.meta.mime)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=60"),
    );

    let disposition_type = if as_attachment {
        "attachment"
    } else {
        "inline"
    };
    let filename = stored.meta.name.replace('"', "");
    let cd = format!(
        "{disposition_type}; filename=\"{}\"; filename*=UTF-8''{}",
        filename,
        urlencoding::encode(&stored.meta.name)
    );
    if let Ok(v) = HeaderValue::from_str(&cd) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }

    Ok((headers, Body::from(data)).into_response())
}

fn sniff_mime(data: &[u8], claimed: &str) -> String {
    if data.len() >= 3 && data[0] == 0xFF && data[1] == 0xD8 && data[2] == 0xFF {
        return "image/jpeg".into();
    }
    if data.len() >= 8 && data[0..8] == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] {
        return "image/png".into();
    }
    if data.len() >= 6 && (&data[0..6] == b"GIF87a" || &data[0..6] == b"GIF89a") {
        return "image/gif".into();
    }
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return "image/webp".into();
    }
    if data.len() >= 12 && &data[4..8] == b"ftyp" {
        return "video/mp4".into();
    }
    if data.len() >= 4 && &data[0..4] == b"\x1A\x45\xDF\xA3" {
        return "video/webm".into();
    }
    claimed.to_string()
}

fn bad_request(msg: String) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": msg })),
    )
}

fn not_found(msg: &str) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": msg })),
    )
}

fn internal(msg: &str) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": msg })),
    )
}

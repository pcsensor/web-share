# Chat Transfer

基于 **Rust (Axum)** 的私有群聊 + 文件传输工具。部署在公网服务器后，用户在浏览器输入 IP（或域名）即可访问。

- 进入站点需输入**管理员密码**
- 所有通过验证的用户进入**同一个群聊**
- 支持文本、图片、视频（可在线预览），以及其它文件（可下载）
- UI 风格参考 Telegram（深色主题、气泡消息、附件上传）
- 实时通信使用 WebSocket
- **消息与文件持久化**，统一**仅保留最近 3 小时**（可配置）

---

## 功能一览

| 能力 | 说明 |
|------|------|
| 密码门禁 | 统一管理员密码（bcrypt 存储/校验） |
| 昵称 | 登录时自选显示名（1–24 字符） |
| 文本消息 | 实时同步，写入 SQLite，长度可配置 |
| 图片 / 视频 | 上传后群内在线预览，点击放大 |
| 其它文件 | 展示文件名与大小，支持下载 |
| 在线人数 | 顶部显示当前 WebSocket 连接数 |
| 会话 Cookie | HttpOnly + SameSite=Strict，可配置 Secure |
| 数据保留 | 默认 3 小时；到期自动删除 DB 记录与磁盘文件 |
| 进程重启 | 近 3 小时内的消息与文件在重启后仍可访问 |

---

## 架构

```
浏览器 ──HTTPS/HTTP──▶ Nginx（可选）──▶ chat-transfer (Axum)
                              │              │
                              │              ├─ 会话 / 登录限流（内存）
                              │              ├─ 消息 + 文件元数据（SQLite: data/chat.db）
                              │              └─ 文件内容（data/uploads/{uuid}.bin）
                              └─ WebSocket /ws
```

- **后端**: Rust · Axum · Tokio · WebSocket · Multipart · SQLite (sqlx)
- **前端**: 原生 HTML/CSS/JS（无构建步骤，静态资源内嵌服务）
- **存储**:
  - `data/chat.db`：消息、文件元数据
  - `data/uploads/*.bin`：上传文件本体
  - 登录会话仍在内存（默认 TTL 与保留窗口一致为 3 小时）

---

## 数据持久化与 3 小时保留

### 存什么

| 数据 | 位置 | 重启后 |
|------|------|--------|
| 文本 / 图 / 视频 / 文件消息 | SQLite `messages` | 保留（未过期部分） |
| 文件名、大小、MIME、路径 | SQLite `files` | 保留（未过期部分） |
| 文件二进制 | `uploads/{uuid}.bin` | 保留（未过期部分） |
| 登录会话 | 内存 | **丢失**（需重新输入密码） |

### 过期规则

- 以服务器 **UTC** 的 `created_at` 为准。
- 默认保留窗口：`CHAT_RETENTION_SECS=10800`（**3 小时**）。
- 消息与文件**同一套**保留策略：过期后既不可见，也不可下载。
- 后台任务每 `CHAT_PURGE_INTERVAL_SECS`（默认 60 秒）清理一次：
  1. 删除过期 `messages` / `files` 行
  2. 删除对应磁盘 `.bin`
  3. 清理无 DB 记录的孤儿文件
- 启动时会立即执行一次 purge。
- **读路径双保险**：列表与下载查询均带 `created_at >= now - retention`，即使清理任务尚未跑到，也不会返回已过期数据。

### 写入一致性

- 文本消息：先写 SQLite，再 WebSocket 广播。
- 文件消息：先写磁盘文件 → 同一事务写入 `files` + `messages` → 成功后再广播；DB 失败则删除刚写入的文件。

### 历史加载上限

在 3 小时窗口内，若消息量极大，另有 `CHAT_MAX_HISTORY`（默认 2000）作为加载条数硬上限，防止单次查询过大。

### 目录结构示例

```
data/
├── chat.db                 # SQLite
└── uploads/
    ├── <uuid>.bin
    └── ...
```

> 旧版若存在 `uploads/index.json`，新版本**不再使用**，可手动删除。元数据已迁到 SQLite。

---

## 安全设计

### 认证与会话

1. **统一密码门禁**：未登录无法访问 API、WebSocket、文件预览/下载。
2. **bcrypt**：`CHAT_PASSWORD` 启动时哈希（或直接配置 `$2…` 哈希）。
3. **服务端会话**：256-bit 随机 token 存于内存 `DashMap`，Cookie 名 `chat_session`。
4. **Cookie 属性**：`HttpOnly`、`SameSite=Strict`；生产开启 `CHAT_SECURE_COOKIE=true`（需 HTTPS）。
5. **登录限流**：按 IP（`X-Real-IP` / `X-Forwarded-For` / fallback）限制失败次数。
6. **会话过期**：默认 3 小时（`CHAT_SESSION_TTL_SECS`），与数据保留窗口对齐，可单独调整。

### 输入与上传

7. **昵称净化**：长度限制、拒绝控制字符与双向覆盖字符。
8. **消息净化**：去除危险控制字符；最大长度可配置。
9. **文件大小上限**：默认 100MB；Axum `DefaultBodyLimit` 同步限制。
10. **路径穿越防护**：存储名使用 UUID；展示名剥离路径组件；`canonicalize` 校验落盘路径。
11. **MIME 嗅探**：对常见图片/视频做 magic-byte 识别。
12. **非预览类型**：仅 image/video 可 `inline` 预览；其它类型走下载。

### HTTP 加固

13. 响应头：CSP、`X-Frame-Options: DENY`、`nosniff`、`Referrer-Policy`、`Permissions-Policy`。
14. **无 CORS 开放**：同源 Cookie 会话。
15. 生产建议：**仅反代暴露 443**，应用监听 `127.0.0.1`，启用 HTTPS 与 `CHAT_SECURE_COOKIE=true`。

### 威胁模型说明（请知悉）

- 所有拿到密码的人处于**同一群**，彼此可见近 3 小时内的消息与文件。
- 服务器管理员可读 `chat.db` 与 `uploads/` 中未过期内容；**未做端到端加密**。
- 3 小时后数据会被物理删除，但在此之前备份介质中仍可能残留，请按需管理备份策略。
- 图片/视频在浏览器中渲染，恶意文件仍可能带来客户端风险——仅在可信群体中使用。

---

## 环境变量

复制模板为 `.env` 后可直接 `cargo run --release`（见本地运行章节）：

```bash
cp .env.example .env   # Windows: copy .env.example .env
```

也可在 shell / systemd `EnvironmentFile` / Docker `environment` 中设置同名变量。`.env` 仅作本地开发便利，**生产仍建议用 systemd/容器注入密钥**。

`.env.example` 字段说明：

| 变量 | 默认 | 说明 |
|------|------|------|
| `CHAT_BIND` | `0.0.0.0:8080` | 监听地址 |
| `CHAT_PASSWORD` | `change-me-now` | 管理员密码（或 bcrypt 哈希） |
| `CHAT_DATA_DIR` | `./data` | 数据目录（库 + 上传） |
| `CHAT_MAX_FILE_MB` | `100` | 单文件上限（MB） |
| `CHAT_MAX_MSG_LEN` | `4000` | 文本最大字符数 |
| `CHAT_MAX_HISTORY` | `2000` | 保留窗口内历史条数硬上限 |
| `CHAT_RETENTION_SECS` | `10800` | **数据保留时长（秒），默认 3 小时** |
| `CHAT_PURGE_INTERVAL_SECS` | `60` | 后台清理周期（秒） |
| `CHAT_SESSION_TTL_SECS` | `10800` | 会话 Cookie TTL（秒） |
| `CHAT_LOGIN_MAX_ATTEMPTS` | `8` | 限流：窗口内最大失败次数 |
| `CHAT_LOGIN_WINDOW_SECS` | `300` | 限流窗口（秒） |
| `CHAT_SECURE_COOKIE` | `false` | HTTPS 下设为 `true` |
| `RUST_LOG` | — | 如 `chat_transfer=info` |

---

## 本地快速运行

### 前置条件

- Rust stable（推荐 1.85+，本仓库使用 edition 2024）
- Windows / Linux / macOS

### 编译与启动

**推荐：使用 `.env` 文件**（启动时自动加载，已用 `dotenvy` 集成）：

```bash
cd chat-transfer

# 从模板复制，再编辑密码等配置（.env 已被 gitignore，勿提交）
cp .env.example .env          # Windows: copy .env.example .env
# 编辑 .env，至少修改 CHAT_PASSWORD=...

cargo run --release
```

程序会在启动时读取当前工作目录（及向上查找）中的 `.env`。  
**已存在的系统/ shell 环境变量优先，不会被 `.env` 覆盖。**

也可以不建 `.env`，直接在 shell 里导出变量：

```bash
# PowerShell:
$env:CHAT_PASSWORD = "YourStrongPassword_Here"
$env:CHAT_BIND = "0.0.0.0:8080"

# bash:
# export CHAT_PASSWORD='YourStrongPassword_Here'
# export CHAT_BIND=0.0.0.0:8080

cargo run --release
```

浏览器访问：`http://127.0.0.1:8080`  
输入昵称 + 密码即可进入群聊。

> 静态资源目录为 `static/`，请从项目根目录启动，或保证工作目录能解析到 `static/`。

### 仅编译

```bash
cargo build --release
# 产物: target/release/chat-transfer  (Windows: chat-transfer.exe)
```

### 验证持久化（可选）

1. 发送几条消息并上传一个文件。
2. 停止进程再启动。
3. 重新登录后应仍能看到近 3 小时内的消息与文件。
4. 将 `CHAT_RETENTION_SECS` 临时设为较小值（如 `30`）可快速验证自动清理。

---

## 公网部署流程（推荐）

以下以 **Ubuntu 22.04/24.04** 为例，其它发行版命令类似。

### 方案 A：二进制 + systemd + Nginx（推荐生产）

#### 1. 服务器准备

```bash
sudo apt update
sudo apt install -y build-essential pkg-config nginx

# 可选：在服务器上编译
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

也可在本地 `cargo build --release` 后只上传二进制与 `static/`（目标平台需一致，或用 Docker）。

#### 2. 创建用户与目录

```bash
sudo useradd --system --home /opt/chat-transfer --shell /usr/sbin/nologin chatxfer || true
sudo mkdir -p /opt/chat-transfer/data/uploads /opt/chat-transfer/static
sudo chown -R chatxfer:chatxfer /opt/chat-transfer
```

#### 3. 部署文件

```bash
cargo build --release

sudo cp target/release/chat-transfer /opt/chat-transfer/
sudo cp -r static/* /opt/chat-transfer/static/
sudo chown -R chatxfer:chatxfer /opt/chat-transfer
sudo chmod 750 /opt/chat-transfer
sudo chmod 550 /opt/chat-transfer/chat-transfer
```

#### 4. 配置环境文件

```bash
sudo tee /opt/chat-transfer/env >/dev/null <<'EOF'
CHAT_BIND=127.0.0.1:8080
CHAT_PASSWORD=请改成足够长的随机密码
CHAT_DATA_DIR=/opt/chat-transfer/data
CHAT_MAX_FILE_MB=100
CHAT_RETENTION_SECS=10800
CHAT_PURGE_INTERVAL_SECS=60
CHAT_SESSION_TTL_SECS=10800
CHAT_SECURE_COOKIE=true
RUST_LOG=chat_transfer=info,tower_http=info
EOF

sudo chown root:chatxfer /opt/chat-transfer/env
sudo chmod 640 /opt/chat-transfer/env
```

> 若暂时没有 HTTPS，先设 `CHAT_SECURE_COOKIE=false`，并尽快上证书。

#### 5. systemd 服务

```bash
sudo cp deploy/chat-transfer.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now chat-transfer
sudo systemctl status chat-transfer
```

日志：

```bash
journalctl -u chat-transfer -f
```

可关注日志中的 `retention purge` / `startup purge` 行，确认过期清理在运行。

#### 6. Nginx 反代

```bash
sudo cp deploy/nginx.conf /etc/nginx/sites-available/chat-transfer
sudo ln -sf /etc/nginx/sites-available/chat-transfer /etc/nginx/sites-enabled/chat-transfer
# 按需编辑 server_name、ssl 证书路径
sudo nginx -t && sudo systemctl reload nginx
```

#### 7. HTTPS（有域名时，Let's Encrypt）

```bash
sudo apt install -y certbot python3-certbot-nginx
sudo certbot --nginx -d your.domain.example
```

证书生效后确认：

- Nginx 监听 443
- 应用 `CHAT_SECURE_COOKIE=true`
- `sudo systemctl restart chat-transfer`

#### 8. 防火墙

```bash
sudo ufw allow OpenSSH
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
# 不要对公网开放 8080（应用只听 127.0.0.1）
sudo ufw enable
```

#### 9. 访问

- 有域名：`https://your.domain.example`
- 仅 IP：`http://你的公网IP`（经 Nginx 80；强烈建议配置证书或 VPN/IP 白名单）

---

### 方案 B：Docker Compose

```bash
cd chat-transfer
export CHAT_PASSWORD='YourStrongPassword_Here'
docker compose up -d --build
```

默认映射 `8080:8080`。生产环境建议：

1. 前面再加 Nginx/Caddy 做 TLS；
2. 将 `ports` 改为仅本机，或接入外部反代网络；
3. 设置 `CHAT_SECURE_COOKIE=true`；
4. 可在 `docker-compose.yml` 中增加 `CHAT_RETENTION_SECS` 等环境变量。

数据卷：`chat_data` → 容器内 `/app/data`（含 `chat.db` 与 `uploads/`）。

```bash
docker compose logs -f
docker compose down
```

---

### 方案 C：仅暴露二进制（内网 / 临时）

```bash
export CHAT_BIND=0.0.0.0:8080
export CHAT_PASSWORD='...'
./target/release/chat-transfer
```

云厂商安全组放行 **TCP 8080**。**不推荐**在无 TLS 的公网长期如此运行。

---

## 交叉编译备忘（可选）

```bash
rustup target add x86_64-unknown-linux-gnu
cargo build --release --target x86_64-unknown-linux-gnu
```

Windows 本机开发可直接 `cargo run`；部署到 Linux 请使用 Linux 产物或 Docker。

---

## API 摘要（需登录 Cookie，除 login）

| 方法 | 路径 | 说明 |
|------|------|------|
| `POST` | `/api/login` | `{ "password", "nickname" }` |
| `POST` | `/api/logout` | 注销 |
| `GET` | `/api/me` | 当前用户 |
| `GET` | `/api/messages` | 近保留窗口内历史消息 |
| `POST` | `/api/messages/text` | 发文本（WS 也可用） |
| `POST` | `/api/upload` | `multipart`: `file`，可选 `caption` |
| `GET` | `/api/files/{id}` | 图片/视频预览（未过期） |
| `GET` | `/api/files/{id}/download` | 下载（未过期） |
| `GET` | `/ws` | WebSocket（Cookie 鉴权） |

WebSocket 事件（JSON）：

- 服务端：`history` / `message` / `presence` / `error`
- 客户端：`ping` / `text`

---

## 运维建议

1. **备份**：若需要事故恢复，可定期备份整个 `CHAT_DATA_DIR`（注意备份中含近 3 小时明文内容）。
2. **磁盘**：过期文件会自动删除；若调大保留窗口或上传很大文件，请监控磁盘占用。
3. **轮换密码**：修改 `CHAT_PASSWORD` 后重启；已有会话在 TTL 内仍有效，重启会清空内存会话。
4. **调整保留时间**：改 `CHAT_RETENTION_SECS` 后重启即可；更短的窗口会在下次 purge 时删掉更早的数据。
5. **升级**：替换二进制 → `systemctl restart chat-transfer`；先备份 `data/`。SQLite schema 使用 `CREATE IF NOT EXISTS`，当前版本向前兼容新建表结构。
6. **日志**：关注 `retention purge`、登录失败与上传错误。

---

## 项目结构

```
chat-transfer/
├── Cargo.toml
├── Dockerfile
├── docker-compose.yml
├── .env.example
├── README.md
├── deploy/
│   ├── nginx.conf
│   └── chat-transfer.service
├── src/
│   ├── main.rs      # 路由、安全头、启动与 purge 任务
│   ├── config.rs    # 环境配置
│   ├── auth.rs      # 登录、会话、限流
│   ├── chat.rs      # 消息与 WebSocket
│   ├── db.rs        # SQLite 持久化与过期清理
│   ├── files.rs     # 上传/下载/预览
│   └── models.rs    # 数据结构
└── static/
    ├── index.html
    └── assets/
        ├── app.css
        └── app.js
```

---

## 许可证

按需自用；未附加开源许可证时可视为私有项目代码。

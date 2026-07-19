# Chat Transfer

基于 **Rust (Axum)** 的私有群聊 + 文件传输工具。部署在公网服务器后，用户在浏览器输入 IP（或域名）即可访问。

- **用户注册**：公开注册需管理员审核；**邀请码**注册可跳过审核（默认 24 小时有效、一次性）
- 每用户强制 **TOTP** 绑定；**新设备**登录需验证码（可信任设备）
- 聊天内可管理**受信任设备**与**修改密码**
- 所有通过验证的用户进入**同一个群聊**
- 支持文本、图片、视频（可在线预览），以及其它文件（可下载）
- UI 风格参考 Telegram（深色主题、气泡消息、附件上传）
- 实时通信使用 WebSocket
- **消息与文件持久化**，统一**仅保留最近 3 小时**（可配置）；账号 / 设备 / 邀请码持久化

---

## 功能一览

| 能力 | 说明 |
|------|------|
| 注册 / 登录 | 独立账号密码（bcrypt） |
| 管理员审核 | 公开注册后待审；通过后须绑定 TOTP |
| 邀请码 | 后台生成；**24h 有效**、一次性；用后免审；可取消未使用码 |
| 管理员后台 | `/admin`：用户审核/停用、重置 TOTP、邀请码、审计 |
| TOTP 二次验证 | 强制绑定；陌生设备需验证码 + 可选信任设备 |
| 安全设置 | 聊天内面板：设备列表/吊销、修改密码 |
| 文本消息 | 实时同步，写入 SQLite，长度可配置 |
| 图片 / 视频 | 上传后群内在线预览，点击放大 |
| 其它文件 | 展示文件名与大小，支持下载 |
| 在线人数 | 顶部显示当前 WebSocket 连接数 |
| 会话 Cookie | HttpOnly + SameSite=Strict，可配置 Secure |
| 数据保留 | 消息/文件默认 3 小时；**用户与设备信任持久化** |
| 进程重启 | 近 3 小时消息仍在；内存会话丢失需重新登录（设备信任仍有效） |

---

## 架构

```
浏览器 ──HTTPS/HTTP──▶ Nginx（可选）──▶ chat-transfer (Axum)
                              │              │
                              │              ├─ 会话 / 限流（内存）
                              │              ├─ 用户 / 设备 / 邀请码 / 消息（SQLite）
                              │              └─ 文件内容（data/uploads/{uuid}.bin）
                              └─ WebSocket /ws
```

- **后端**: Rust · Axum · Tokio · WebSocket · Multipart · SQLite (sqlx)
- **前端**: 原生 HTML/CSS/JS（无构建步骤，静态资源内嵌服务）
- **存储**:
  - `data/chat.db`：用户、TOTP、设备信任、邀请码、消息与文件元数据
  - `data/uploads/*.bin`：上传文件本体
  - 登录会话在内存（默认完整会话 TTL 24 小时；重启后会话丢失）

---

## 数据持久化与 3 小时保留

### 存什么

| 数据 | 位置 | 重启后 |
|------|------|--------|
| 用户账号 / 密码哈希 / TOTP | SQLite `users` | 保留 |
| 受信任设备 | SQLite `trusted_devices` | 保留 |
| 邀请码 | SQLite `invite_codes` | 保留 |
| 文本 / 图 / 视频 / 文件消息 | SQLite `messages` | 保留（未过期部分） |
| 文件名、大小、MIME、路径 | SQLite `files` | 保留（未过期部分） |
| 文件二进制 | `uploads/{uuid}.bin` | 保留（未过期部分） |
| 登录会话 | 内存 | **丢失**（需重新登录；可信设备可跳过 2FA） |

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

1. **账号准入**：
   - 公开注册 → `pending_approval` → 管理员通过 → 绑定 TOTP → 进群；或
   - **邀请码注册** → 直接 `approved_unbound`（免审）→ 绑定 TOTP → 进群。
2. **邀请码**：一次性；默认 **24 小时**过期（`CHAT_INVITE_TTL_HOURS`）；状态：未使用 / 已使用 / 已过期 / 已吊销；后台可查看全部历史并取消未使用码。
3. **Bootstrap 管理员**：空库时用 `CHAT_BOOTSTRAP_ADMIN_USER` / `CHAT_BOOTSTRAP_ADMIN_PASSWORD` 创建首位管理员（同样须绑定 TOTP）。
4. **会话分级**：`pending_2fa` / `pending_totp_setup` / `full`；仅 `full` + `active` 可访问聊天、上传、WebSocket、管理 API。
5. **bcrypt** 存密码；TOTP secret 用 `CHAT_SECRET_KEY` 做 AES-GCM 密封后入库（**生产必须固定该密钥**）。
6. **服务端会话**：256-bit token 存内存 `DashMap`，Cookie `chat_session`。
7. **设备信任**：Cookie `chat_device` + SQLite `trusted_devices`（存 token hash）；可在安全设置中吊销。
8. **Cookie 属性**：`HttpOnly`、`SameSite=Strict`；生产开启 `CHAT_SECURE_COOKIE=true`。
9. **登录 / 2FA 限流**：按 IP（及 2FA 按 user）限制失败次数。

### 输入与上传

10. **昵称 / 用户名净化**：长度与字符限制、拒绝控制字符与双向覆盖字符。
11. **消息净化**：去除危险控制字符；最大长度可配置。
12. **文件大小上限**：默认 100MB；Axum `DefaultBodyLimit` 同步限制。
13. **路径穿越防护**：存储名使用 UUID；展示名剥离路径组件；`canonicalize` 校验落盘路径。
14. **MIME 嗅探**：对常见图片/视频做 magic-byte 识别。
15. **非预览类型**：仅 image/video 可 `inline` 预览；其它类型走下载。

### HTTP 加固

16. 响应头：CSP、`X-Frame-Options: DENY`、`nosniff`、`Referrer-Policy`、`Permissions-Policy`。
17. **无 CORS 开放**：同源 Cookie 会话。
18. 生产建议：**仅反代暴露 443**，应用监听 `127.0.0.1`，启用 HTTPS 与 `CHAT_SECURE_COOKIE=true`。

### 威胁模型说明（请知悉）

- 通过审核（或邀请码）并完成 TOTP 的成员处于**同一群**，彼此可见近 3 小时内的消息与文件。
- 服务器管理员可读 `chat.db` 与 `uploads/` 中未过期内容；**未做端到端加密**。
- 3 小时后消息/文件会被物理删除，但在此之前备份介质中仍可能残留。
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
| `CHAT_SECRET_KEY` | （临时生成） | 密封 TOTP 的 32 字节密钥（建议 `openssl rand -hex 32`） |
| `CHAT_BOOTSTRAP_ADMIN_USER` | — | 空库时创建的管理员用户名 |
| `CHAT_BOOTSTRAP_ADMIN_PASSWORD` | — | 空库时创建的管理员密码（≥8 位） |
| `CHAT_REGISTRATION_OPEN` | `true` | 是否开放**无邀请码**的自助注册 |
| `CHAT_INVITE_TTL_HOURS` | `24` | 邀请码有效小时数 |
| `CHAT_DEVICE_TRUST_DAYS` | `60` | 信任设备天数 |
| `CHAT_TOTP_ISSUER` | `Chat Transfer` | Authenticator 显示名（**含空格必须加引号**，否则整份 env 可能解析失败） |
| `CHAT_RESET_ADMIN_PASSWORD` | `false` | 紧急：把 bootstrap 用户密码重置为 env 中的密码（用完改回 false） |
| `CHAT_RESET_ADMIN_TOTP` | `false` | 紧急：清除 bootstrap 管理员 TOTP（用完改回 false） |
| `CHAT_DATA_DIR` | `./data` | 数据目录（库 + 上传） |
| `CHAT_MAX_FILE_MB` | `100` | 单文件上限（MB） |
| `CHAT_MAX_MSG_LEN` | `4000` | 文本最大字符数 |
| `CHAT_MAX_HISTORY` | `2000` | 保留窗口内历史条数硬上限 |
| `CHAT_RETENTION_SECS` | `10800` | **消息/文件**保留时长（秒），默认 3 小时 |
| `CHAT_PURGE_INTERVAL_SECS` | `60` | 后台清理周期（秒） |
| `CHAT_SESSION_TTL_SECS` | `86400` | 完整会话 Cookie TTL（秒） |
| `CHAT_PENDING_2FA_TTL_SECS` | `300` | 等待 2FA/绑定 的短会话 TTL |
| `CHAT_LOGIN_MAX_ATTEMPTS` | `8` | 限流：窗口内最大失败次数 |
| `CHAT_LOGIN_WINDOW_SECS` | `300` | 限流窗口（秒） |
| `CHAT_SECURE_COOKIE` | `false` | HTTPS 下设为 `true` |
| `RUST_LOG` | — | 如 `chat_transfer=info` |

> 已废弃：旧版统一门禁 `CHAT_PASSWORD`，请改用 bootstrap 管理员 + 用户注册。

**管理员登不上时（常见）：**

1. `.env` 的 `CHAT_BOOTSTRAP_ADMIN_PASSWORD` **只在首次创建库时生效**；之后改 .env 不会改库里密码。  
2. 会话存在内存里，**进程重启后需重新登录**。  
3. 管理员同样要完成 **TOTP**；新设备还要二次验证。可直接打开 `/admin` 在本页完成登录/验证/绑定。  
4. 紧急恢复（设置后重启一次，成功后务必关掉）：
   ```env
   CHAT_RESET_ADMIN_PASSWORD=true
   CHAT_RESET_ADMIN_TOTP=true
   ```

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
# 编辑 .env，设置 CHAT_SECRET_KEY 与 bootstrap 管理员

cargo run --release
```

程序会在启动时读取当前工作目录（及向上查找）中的 `.env`。  
**已存在的系统/ shell 环境变量优先，不会被 `.env` 覆盖。**

也可以不建 `.env`，直接在 shell 里导出变量：

```bash
# PowerShell:
$env:CHAT_BOOTSTRAP_ADMIN_USER = "admin"
$env:CHAT_BOOTSTRAP_ADMIN_PASSWORD = "YourStrongPassword_Here"
$env:CHAT_SECRET_KEY = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
$env:CHAT_BIND = "0.0.0.0:8080"

# bash:
# export CHAT_BOOTSTRAP_ADMIN_USER='admin'
export CHAT_BOOTSTRAP_ADMIN_PASSWORD='YourStrongPassword_Here'
export CHAT_SECRET_KEY="$(openssl rand -hex 32)"
# export CHAT_BIND=0.0.0.0:8080

cargo run --release
```

浏览器访问：`http://127.0.0.1:8080`

**首次使用：**

1. 使用 `.env` 中的 bootstrap 管理员账号登录  
2. 按提示绑定 Authenticator（TOTP）  
3. 打开 `/admin` 审核用户，或生成邀请码分享给他人  
4. 其它用户注册（公开注册待审，或填邀请码免审）→ 登录 → 绑定 TOTP → 进入同一群聊  

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

以下以 **Ubuntu 22.04/24.04** 为例。生产推荐：**方案 A（二进制 + systemd + Nginx + HTTPS）**。

要点（与旧版「统一密码门禁」不同）：

| 项 | 生产建议 |
|----|----------|
| 监听 | 应用只绑 `127.0.0.1:8080`，公网只暴露 Nginx 443 |
| 密钥 | 固定 `CHAT_SECRET_KEY`（`openssl rand -hex 32`），写入 env 文件并备份 |
| 管理员 | 首次启动用 bootstrap 创建；登录后 **必须绑定 TOTP** 才能进 `/admin` 与群聊 |
| 准入 | 公开注册 + 后台审核，或 **邀请码**（默认 24h、一次性） |
| 静态资源 | `WorkingDirectory` 须为部署根目录（含 `static/`） |
| Cookie | HTTPS 下 `CHAT_SECURE_COOKIE=true` |
| 环境文件 | 用 systemd `EnvironmentFile`（`/opt/chat-transfer/env`），**不要**把含空格的值写错格式 |

### 方案 A：二进制 + systemd + Nginx（推荐生产）

#### 1. 服务器准备

```bash
sudo apt update
sudo apt install -y build-essential pkg-config nginx curl

# 可选：在服务器上编译
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

也可在 **同架构** Linux 上 `cargo build --release`，再只上传：

- `target/release/chat-transfer`
- 整个 `static/` 目录
- `deploy/chat-transfer.service`、`deploy/nginx.conf`

Windows 本机编译的 `.exe` **不能**直接部署到 Linux。

#### 2. 创建用户与目录

```bash
sudo useradd --system --home /opt/chat-transfer --shell /usr/sbin/nologin chatxfer || true
sudo mkdir -p /opt/chat-transfer/data/uploads /opt/chat-transfer/static
sudo chown -R chatxfer:chatxfer /opt/chat-transfer
```

部署后目录结构应为：

```text
/opt/chat-transfer/
├── chat-transfer          # 二进制
├── env                    # 密钥与配置（权限 640，勿提交 git）
├── static/                # 前端（index.html / admin.html / assets）
│   ├── index.html
│   ├── admin.html
│   └── assets/
└── data/                  # CHAT_DATA_DIR
    ├── chat.db
    └── uploads/
```

#### 3. 部署二进制与静态资源

在项目根目录（或把编译产物拷到服务器后）：

```bash
cargo build --release

sudo cp target/release/chat-transfer /opt/chat-transfer/
sudo rsync -a --delete static/ /opt/chat-transfer/static/
sudo chown -R chatxfer:chatxfer /opt/chat-transfer
sudo chmod 750 /opt/chat-transfer
sudo chmod 550 /opt/chat-transfer/chat-transfer
```

> `systemd` 单元里 `WorkingDirectory=/opt/chat-transfer`，应用从该目录加载 `static/`。换目录部署时请同步改 unit 与 `CHAT_DATA_DIR`。

#### 4. 配置环境文件（生产关键）

```bash
# 生成固定密钥（只生成一次，务必备份）
SECRET_KEY="$(openssl rand -hex 32)"
ADMIN_PASS='请改成足够长的随机密码'

sudo tee /opt/chat-transfer/env >/dev/null <<EOF
# 仅本机；由 Nginx 反代
CHAT_BIND=127.0.0.1:8080

# 密封 TOTP；丢失后已绑定用户无法验证，只能重置 2FA
CHAT_SECRET_KEY=${SECRET_KEY}

# 空库时创建首位管理员（仅第一次生效；之后改此项不会改库中密码）
CHAT_BOOTSTRAP_ADMIN_USER=admin
CHAT_BOOTSTRAP_ADMIN_PASSWORD=${ADMIN_PASS}

# 准入
CHAT_REGISTRATION_OPEN=true
CHAT_INVITE_TTL_HOURS=24
CHAT_DEVICE_TRUST_DAYS=60

# 含空格的值必须加引号
CHAT_TOTP_ISSUER="Chat Transfer"

# 数据与限制
CHAT_DATA_DIR=/opt/chat-transfer/data
CHAT_MAX_FILE_MB=100
CHAT_MAX_MSG_LEN=4000
CHAT_MAX_HISTORY=2000
CHAT_RETENTION_SECS=10800
CHAT_PURGE_INTERVAL_SECS=60
CHAT_SESSION_TTL_SECS=86400
CHAT_PENDING_2FA_TTL_SECS=300
CHAT_LOGIN_MAX_ATTEMPTS=8
CHAT_LOGIN_WINDOW_SECS=300

# 生产 + HTTPS 必须 true；暂时只有 HTTP 时先 false
CHAT_SECURE_COOKIE=true

RUST_LOG=chat_transfer=info,tower_http=info
EOF

sudo chown root:chatxfer /opt/chat-transfer/env
sudo chmod 640 /opt/chat-transfer/env
```

**注意：**

1. `CHAT_SECRET_KEY` / bootstrap 密码写在 `env` 文件里，权限 `640`，属主 `root:chatxfer`。  
2. **备份** `env` 与 `data/`：丢了 `CHAT_SECRET_KEY`，库里密封的 TOTP 全部失效。  
3. `CHAT_BOOTSTRAP_ADMIN_PASSWORD` **仅空库首次创建 admin 时写入数据库**；以后改 `env` 不会改登录密码。忘记密码时临时加：
   ```bash
   # 写入 env 后：
   CHAT_RESET_ADMIN_PASSWORD=true
   CHAT_RESET_ADMIN_TOTP=true   # 可选：强制重新绑验证器
   # systemctl restart chat-transfer
   # 确认 journal 出现 “CHAT_RESET_ADMIN_* applied” 后立刻改回 false 并再重启
   ```
4. `.env` 若含 `CHAT_TOTP_ISSUER=Chat Transfer`（无引号）会导致**整文件解析失败**、配置全部回退默认值。生产用 `EnvironmentFile` 时同样：**含空格必须加引号**。

#### 5. 安装并启动 systemd

```bash
sudo cp deploy/chat-transfer.service /etc/systemd/system/
# 确认单元中：
#   WorkingDirectory=/opt/chat-transfer
#   EnvironmentFile=/opt/chat-transfer/env
#   ExecStart=/opt/chat-transfer/chat-transfer
sudo systemctl daemon-reload
sudo systemctl enable --now chat-transfer
sudo systemctl status chat-transfer
```

**启动后必看日志**（确认配置真的加载了）：

```bash
journalctl -u chat-transfer -n 50 --no-pager
```

应出现类似：

```text
config loaded device_trust_days=60 invite_ttl_hours=24 ...
bootstrap admin created ...          # 仅空库第一次
Chat Transfer listening on http://127.0.0.1:8080
```

若端口被占用（`Address already in use`）：

```bash
sudo ss -lptn 'sport = :8080'
sudo systemctl stop chat-transfer   # 或结束残留进程后再 start
```

持续跟踪：

```bash
journalctl -u chat-transfer -f
```

可关注：`config loaded`、`retention purge`、`user logged in`、邀请码与审核操作。

#### 6. Nginx 反代

```bash
sudo cp deploy/nginx.conf /etc/nginx/sites-available/chat-transfer
sudo ln -sf /etc/nginx/sites-available/chat-transfer /etc/nginx/sites-enabled/chat-transfer
# 编辑 server_name；client_max_body_size 须 ≥ CHAT_MAX_FILE_MB（示例 110m）
sudo nginx -t && sudo systemctl reload nginx
```

示例配置已包含：

- 反代到 `127.0.0.1:8080`
- `X-Real-IP` / `X-Forwarded-For` / `X-Forwarded-Proto`（登录限流与 Secure Cookie 依赖）
- **`/ws` WebSocket** 升级头（缺了则实时消息不可用）

#### 7. HTTPS（有域名时，Let's Encrypt）

```bash
sudo apt install -y certbot python3-certbot-nginx
sudo certbot --nginx -d your.domain.example
```

证书生效后确认：

1. Nginx 监听 **443**，HTTP 跳转 HTTPS  
2. 应用 `CHAT_SECURE_COOKIE=true`  
3. `sudo systemctl restart chat-transfer`  
4. 浏览器访问 `https://your.domain.example` 与 `https://your.domain.example/admin`

#### 8. 防火墙

```bash
sudo ufw allow OpenSSH
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
# 不要对公网开放 8080（应用只听 127.0.0.1）
sudo ufw enable
```

#### 9. 上线验收清单

| 检查项 | 方法 |
|--------|------|
| 进程正常 | `systemctl is-active chat-transfer` |
| 配置生效 | `journalctl` 中 `device_trust_days` 等与 `env` 一致 |
| 公开配置 | `curl -s http://127.0.0.1:8080/api/config` |
| 管理员首次登录 | 打开 `/` 或 `/admin`，用 bootstrap 账号登录 |
| 绑定 TOTP | 扫码确认后 `next_step` 为可进群/后台 |
| 管理后台 | `/admin`：审核用户、生成邀请码 |
| 邀请码注册 | 新用户带码注册 → 免审 → 绑 TOTP → 进群 |
| WebSocket | 两浏览器同群，消息实时同步 |
| 上传 | 图片预览 / 文件下载 |
| 安全 Cookie | HTTPS 下 Set-Cookie 含 `Secure` |

**首次管理员路径：**

1. 访问 `https://域名/` 或 `https://域名/admin`  
2. 用户名 `admin` + 你在 `env` 里写的 bootstrap 密码  
3. 绑定 Authenticator  
4. 在 `/admin` 生成邀请码或审核注册用户  

#### 10. 升级与备份

**备份（定期）：**

```bash
sudo systemctl stop chat-transfer
sudo tar czf ~/chat-transfer-backup-$(date +%F).tgz \
  /opt/chat-transfer/env \
  /opt/chat-transfer/data
sudo systemctl start chat-transfer
```

**升级：**

```bash
# 1. 备份 data/ 与 env
# 2. 编译新版本
cargo build --release
# 3. 替换二进制与 static
sudo systemctl stop chat-transfer
sudo cp target/release/chat-transfer /opt/chat-transfer/
sudo rsync -a --delete static/ /opt/chat-transfer/static/
sudo chown chatxfer:chatxfer /opt/chat-transfer/chat-transfer
sudo chmod 550 /opt/chat-transfer/chat-transfer
sudo systemctl start chat-transfer
# 4. journalctl 确认 config loaded / 无 panic
```

SQLite 表结构会自动 `CREATE IF NOT EXISTS` / 必要列迁移；**不要**用新库覆盖生产 `data/chat.db`，除非你有意清空用户。

**回滚：** 停服务 → 换回旧二进制与 static 备份 → 启动；`data/` 尽量与版本兼容使用。

---

### 方案 B：Docker Compose

```bash
cd chat-transfer
export CHAT_BOOTSTRAP_ADMIN_USER='admin'
export CHAT_BOOTSTRAP_ADMIN_PASSWORD='YourStrongPassword_Here'
export CHAT_SECRET_KEY="$(openssl rand -hex 32)"
export CHAT_DEVICE_TRUST_DAYS=60
export CHAT_INVITE_TTL_HOURS=24
export CHAT_SECURE_COOKIE=false   # 前有 HTTPS 反代时再改为 true
docker compose up -d --build
```

默认映射 `8080:8080`。生产建议：

1. 前面再加 Nginx/Caddy 做 TLS；  
2. `ports` 改为仅本机（如 `127.0.0.1:8080:8080`），或接入外部反代网络；  
3. HTTPS 后设 `CHAT_SECURE_COOKIE=true`；  
4. 按需传入 `CHAT_RETENTION_SECS`、`CHAT_REGISTRATION_OPEN` 等（见 `docker-compose.yml`）。

数据卷：`chat_data` → 容器内 `/app/data`（含 `chat.db` 与 `uploads/`）。**密钥用环境变量注入，不要写进镜像。**

```bash
docker compose logs -f
docker compose down
```

---

### 方案 C：仅暴露二进制（内网 / 临时）

```bash
export CHAT_BIND=0.0.0.0:8080
export CHAT_BOOTSTRAP_ADMIN_USER=admin
export CHAT_BOOTSTRAP_ADMIN_PASSWORD='...'
export CHAT_SECRET_KEY="$(openssl rand -hex 32)"
# 在项目根目录启动，以便加载 static/
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

## API 摘要

| 方法 | 路径 | 说明 |
|------|------|------|
| `POST` | `/api/auth/register` | 注册；可选 `invite_code` |
| `POST` | `/api/auth/login` | `{ "username", "password" }` |
| `POST` | `/api/auth/2fa/verify` | 新设备 TOTP；可选 `trust_device` |
| `POST` | `/api/auth/2fa/recover` | 恢复码（用后强制重绑 TOTP） |
| `POST` | `/api/auth/totp/setup/start` | 开始绑定（返回 QR / secret） |
| `POST` | `/api/auth/totp/setup/confirm` | 确认绑定 |
| `POST` | `/api/logout` | 注销 |
| `GET` | `/api/me` | 当前用户与 `next_step` |
| `GET` | `/api/config` | 公开配置（信任设备天数等，无需登录） |
| `GET` | `/api/security/devices` | 我的受信任设备 |
| `DELETE` | `/api/security/devices/{id}` | 吊销设备 |
| `POST` | `/api/security/password` | 修改密码 |
| `GET` | `/api/admin/users` | 用户列表（可 `?status=`） |
| `POST` | `/api/admin/users/{id}/approve` 等 | 通过 / 拒绝 / 停用 / 启用 / 重置 TOTP |
| `GET`/`POST` | `/api/admin/invites` | 列出 / 生成邀请码 |
| `DELETE` | `/api/admin/invites/{id}` | 取消未使用的邀请码 |
| `GET` | `/api/admin/audit` | 审计日志 |
| `GET` | `/api/messages` | 近保留窗口内历史消息 |
| `POST` | `/api/messages/text` | 发文本（WS 也可用） |
| `POST` | `/api/upload` | `multipart`: `file`，可选 `caption` |
| `GET` | `/api/files/{id}` | 图片/视频预览（未过期） |
| `GET` | `/api/files/{id}/download` | 下载（未过期） |
| `GET` | `/ws` | WebSocket（需 full 会话 Cookie） |

页面：`/` 聊天与认证；`/admin` 管理后台。

WebSocket 事件（JSON）：

- 服务端：`history` / `message` / `presence` / `error` / `force_logout`
- 客户端：`ping` / `text`

---

## 运维建议

1. **备份**：可定期备份整个 `CHAT_DATA_DIR`（含用户与近 3 小时消息明文；注意保护 `CHAT_SECRET_KEY`）。
2. **磁盘**：过期消息/文件会自动删除；调大保留窗口或大文件时请监控磁盘。
3. **密钥**：`CHAT_SECRET_KEY` 丢失或更换会导致已密封 TOTP 无法解密，需管理员对用户「重置验证器」。
4. **密码**：用户可在安全设置中改密；管理员口令勿再用已废弃的 `CHAT_PASSWORD`。
5. **邀请码**：后台可查看全部历史状态；未使用的可随时取消；过期码不可再注册。
6. **调整消息保留**：改 `CHAT_RETENTION_SECS` 后重启；更短窗口会在下次 purge 删除更早数据。
7. **升级**：替换二进制 → `systemctl restart chat-transfer`；先备份 `data/`。Schema 以 `CREATE IF NOT EXISTS` / 列迁移兼容。
8. **日志**：关注 `retention purge`、登录失败、邀请码使用与上传错误。

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
│   ├── main.rs         # 路由、安全头、启动与 purge
│   ├── config.rs       # 环境配置
│   ├── auth.rs         # 注册/登录、TOTP、会话、设备、改密
│   ├── admin.rs        # 用户审核、邀请码、审计
│   ├── chat.rs         # 消息与 WebSocket
│   ├── db.rs           # SQLite 持久化
│   ├── crypto_seal.rs  # TOTP 密封
│   ├── totp.rs         # TOTP / 恢复码
│   ├── files.rs        # 上传/下载/预览
│   └── models.rs       # 数据结构
└── static/
    ├── index.html      # 登录/注册/聊天/安全设置
    ├── admin.html      # 管理后台
    └── assets/
        ├── app.css
        ├── app.js
        └── admin.js
```

---

## 许可证

按需自用；未附加开源许可证时可视为私有项目代码。

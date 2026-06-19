<!-- Language Switcher -->
<div align="center">
  <a href="#english-version">English Version</a> | 
  <a href="#中文版本">中文版本</a>
</div>

---

<a name="english-version"></a>

# Cumments

Cumments is a decentralized comment system backend based on the **Matrix protocol**, utilizing an **Event-Sourced Coordinator Pattern**. It uses Matrix as an immutable event store and SQLite as a local high-speed read view.

## 1. Architecture

```
                        ┌──────────────────┐
                        │  User Request    │
                        │  (Browser/API)   │
                        └────────┬─────────┘
                                 │
                    ┌────────────▼────────────┐
                    │      cumments-api       │
                    │  (HTTP, PoW, Intents)   │
                    └────────────┬────────────┘
                                 │
              ┌──────────────────┼──────────────────┐
              │                  │                  │
     ┌────────▼────────┐ ┌──────▼──────┐  ┌────────▼────────┐
     │   Reconciler    │ │ EventStore  │  │   EventProcessor│
     │  (协调器/写入)  │ │ (Matrix)    │  │  (投影/读模型)  │
     └────────┬────────┘ └─────────────┘  └────────┬────────┘
              │                                    │
     ┌────────▼────────┐                  ┌────────▼────────┐
     │  MatrixDriver   │                  │  SyncAdapter    │
     │  (发送到Matrix) │                  │  (Bot模式接收)  │
     └────────┬────────┘                  │  PushReceiver   │
              │                           │  (AS模式接收)   │
              │                           └─────────────────┘
              │
    ┌─────────┼──────────┐
    │         │          │
    ▼         ▼          ▼
 BotDriver  AppService  Logging
 (matrix-sdk) (reqwest)  (调试)
```

The core projection logic lives in **EventProcessor**, shared by both reception paths:
- **Bot mode**: `SyncAdapter` receives events via matrix-sdk Sync
- **AppService mode**: `PushReceiver` receives events via HS HTTP Push

### Crates

| Crate | Responsibility |
|-------|---------------|
| `cumments-core` | Domain models, ports (traits), intents, events |
| `cumments-api` | HTTP API, PoW verification, validation |
| `cumments-store` | SQLite persistence (SeaORM), migrations |
| `cumments-reconciler` | Background orchestration — reads intents, calls MatrixDriver |
| `cumments-matrix` | MatrixDriver implementations (Bot, AppService, Logging) |
| `cumments-projector` | Event reception and projection (EventProcessor, SyncAdapter, PushReceiver) |
| `cumments` | CLI entry point, configuration, assembly |

---

## 2. Operation Modes

### Mode A: Bot Mode (recommended for testing)

Uses a single Matrix bot account. The bot creates rooms/spaces, posts messages as itself, and receives events via matrix-sdk's sync loop.

**Prerequisites**: A regular Matrix account and its `access_token`.

### Mode B: AppService Mode (recommended for production)

Registers as a Matrix **Application Service** with the homeserver. Each commenter gets an independent virtual Matrix user (`@_cumments_{site}_{hash}:domain`). Events are **pushed** by the HS via HTTP, no sync loop needed.

**Prerequisites**:
- Server-side access to the homeserver configuration
- A generated `registration.yaml` (via `cumments generate-registration`)

**Virtual user ID format**:
```
@_cumments_{site_id}_{sha256_trunc8(fingerprint)}:{server_name}
```

---

## 3. Environment Preparation

- **Operating System**: Linux / Windows / macOS
- **Build Environment**: Rust (latest stable version)
- **Database**: SQLite (the program automatically creates files, no service installation required)
- **Matrix Account**:
    - **Bot Mode**: Need a dedicated Matrix account + access token.
    - **AppService Mode**: Need server-side access to the homeserver.

---

## 4. Configuration

Configuration is loaded in order: **Environment Variables** > **`config.toml`** > **Defaults**.
Use `CUMMENTS__` prefix for env vars (e.g., `CUMMENTS_SERVER__PORT=7931`).

### Bot Mode (config.toml)

```toml
[server]
host = "0.0.0.0"
port = 7931
cors_origins = "*"
public_server_name = "matrix.org"

[database]
url = "sqlite://data/cumments.db"

[security]
identity_salt = "RANDOM_LONG_STRING"
admin_token = "admin_secret"
pow_secret = "pow_secret_key"
pow_difficulty = 4

[matrix]
mode = "bot"
homeserver_url = "https://matrix.org"
user = "@cumments_bot:matrix.org"
token = "syt_..."
device_id = "CUMMENTS_BOT"
owner_id = "@your_account:matrix.org"
```

### AppService Mode (config.toml)

```toml
[matrix]
mode = "appservice"
homeserver_url = "http://localhost:8008"
server_name = "your_server.tld"
as_token = "${AS_TOKEN_FROM_REGISTRATION}"
hs_token = "${HS_TOKEN_FROM_REGISTRATION}"
bot_localpart = "cumments"
push_listen_port = 3001
owner_id = "@admin:your_server.tld"
```

Generate the registration file:

```bash
cumments generate-registration --server-name your_server.tld
```

This outputs a `registration.yaml` that must be placed on the homeserver.

---

## 5. Compilation and Running

Ensure the `data` folder exists in the root directory.

```bash
# Development mode
RUST_LOG=info cargo run -p cumments

# Build Release
cargo build --release -p cumments

# Docker Deployment
docker build -t cumments -f misc/docker/Dockerfile .
docker run -p 7931:7931 -v $(pwd)/data:/app/data cumments
```

---

## 6. API Documentation

### Proof of Work (PoW)

**`GET /api/challenge`**
Returns a signed challenge string and difficulty.

**Response:**
```json
{
  "prefix": "timestamp_hex.random_hex.signature",
  "difficulty": 4
}
```

### Comment Operations

**`GET /api/sites/{site_id}/posts/{post_slug}/comments`**
- **Path Params**: `site_id` (1-64 chars, `[a-zA-Z0-9_-]`), `post_slug` (same rules)
- **Query Params**: `page`, `per_page`.

**`POST /api/sites/{site_id}/posts/{post_slug}/comments`**
- **Body**: `content`, `nickname`, `email`, `author_fingerprint`, `challenge_response`.
- Note: `challenge_response` format is `challenge_string|nonce`.

**`PATCH /api/sites/{site_id}/posts/{post_slug}/comments/{comment_id}`**
- **Body**: `content`, `author_fingerprint`, `challenge_response`.

**`DELETE /api/sites/{site_id}/posts/{post_slug}/comments/{comment_id}`**
- **Body**: `author_fingerprint`, `challenge_response`.

### Real-time Push (SSE)

**`GET /api/sites/{site_id}/posts/{post_slug}/sse`**
Server-Sent Events for real-time updates.
- Events: `new_comment`, `update_comment`, `delete_comment`.

---

## 7. Frontend Integration Guide

### 1. Identity (Fingerprint)
Backend calculates fingerprint to identify guest users. Frontend should generate and store a random string (e.g., `guest_token`) and derive/send a stable identifier.

### 2. PoW Calculation
1. Call `GET /api/challenge` to get `prefix` (the signed challenge) and `difficulty`.
2. Brute force `nonce` such that `SHA256(prefix + nonce)` starts with `difficulty` number of `0`s.
3. Submit `challenge_response = prefix + "|" + nonce`.

### 3. site_id and post_slug Validation
Both `site_id` and `post_slug` follow the same format:
- Allowed characters: `a-z`, `A-Z`, `0-9`, `_`, `-`
- Length: 1–64 characters
- Invalid values will receive a `400 VALIDATION_ERROR` response.

---

<a name="中文版本"></a>

# Cumments (中文)

Cumments 是一个基于 **Matrix 协议**的去中心化评论系统后端，采用**事件溯源协调器模式 (Event-Sourced Coordinator Pattern)**。它利用 Matrix 作为不可变事件存储，使用 SQLite 作为本地高速读视图。

## 1. 架构设计

系统采用分层解耦架构：

| 模块 | 职责 |
|------|------|
| `cumments-core` | 领域模型、端口定义、意图、事件 |
| `cumments-api` | HTTP API、PoW 验证、输入校验 |
| `cumments-store` | SQLite 持久化 (SeaORM)、数据库迁移 |
| `cumments-reconciler` | 后台协调器——处理意图队列，调用 MatrixDriver |
| `cumments-matrix` | Matrix 驱动实现（Bot / AppService / Logging 三种） |
| `cumments-projector` | 事件接收与投影核心（EventProcessor + SyncAdapter + PushReceiver） |
| `cumments` | CLI 入口、配置加载、依赖装配 |

核心投影逻辑位于 **EventProcessor**，两种接收路径共享：
- **Bot 模式**: `SyncAdapter` 通过 matrix-sdk Sync 接收事件
- **AppService 模式**: `PushReceiver` 通过 HS HTTP Push 接收事件

## 2. 运行模式

### A: Bot 模式（推荐测试使用）

使用一个 Matrix 机器人账号。机器人创建房间/空间、以自身身份发消息，通过 Sync 循环接收事件。

### B: AppService 模式（推荐生产使用）

以 Matrix Application Service 身份注册到 homeserver。每个评论者拥有独立的虚拟 Matrix 用户（`@_cumments_{站点}_{hash}:域名`）。事件由 HS 通过 HTTP 推送，无需 Sync 循环。

**虚拟用户 ID 格式**:
```
@_cumments_{site_id}_{sha256_trunc8(fingerprint)}:{server_name}
```

## 3. 环境准备

- **操作系统**: Linux / Windows / macOS
- **编译环境**: Rust (最新 stable 版本)
- **数据库**: SQLite（自动创建，无需单独安装）
- **Bot 模式**: 需要一个 Matrix 账号 + access_token
- **AppService 模式**: 需要服务器端访问 homeserver 配置权限

## 4. 配置说明

优先级：**环境变量** > **`config.toml`** > **默认值**。
环境变量前缀为 `CUMMENTS__`（例如 `CUMMENTS_SERVER__PORT=7931`）。

### Bot 模式

```toml
[matrix]
mode = "bot"
homeserver_url = "https://matrix.org"
user = "@cumments_bot:matrix.org"
token = "syt_..."
device_id = "CUMMENTS_BOT"
owner_id = "@your_account:matrix.org"
```

### AppService 模式

```toml
[matrix]
mode = "appservice"
homeserver_url = "http://localhost:8008"
server_name = "your_server.tld"
as_token = "${AS_TOKEN}"
hs_token = "${HS_TOKEN}"
bot_localpart = "cumments"
push_listen_port = 3001
owner_id = "@admin:your_server.tld"
```

生成注册文件：

```bash
cumments generate-registration --server-name your_server.tld
```

## 5. 编译与运行

```bash
# 开发模式
RUST_LOG=info cargo run -p cumments

# 构建 Release
cargo build --release -p cumments

# Docker
docker build -t cumments -f misc/docker/Dockerfile .
docker run -p 7931:7931 -v $(pwd)/data:/app/data cumments
```

## 6. 关键 API

- `GET /api/challenge`: 获取 PoW 挑战签名和难度。
- `GET /api/sites/{site_id}/posts/{post_slug}/comments`: 获取评论列表（支持分页）。
- `POST /api/sites/{site_id}/posts/{post_slug}/comments`: 提交发布评论意图。
- `PATCH /api/sites/{site_id}/posts/{post_slug}/comments/{id}`: 提交编辑评论意图。
- `DELETE /api/sites/{site_id}/posts/{post_slug}/comments/{id}`: 提交删除评论意图。
- `GET /api/sites/{site_id}/posts/{post_slug}/sse`: SSE 实时推送接口。

### site_id 和 post_slug 校验规则
- 允许字符: `a-z`, `A-Z`, `0-9`, `_`, `-`
- 长度: 1–64 字符
- 不合法时返回 `400 VALIDATION_ERROR`

## 7. 前端集成建议

### 1. PoW 计算逻辑
1. 调用 `/api/challenge` 获取挑战字符串 `prefix`。
2. 在前端寻找 `nonce` 使得 `SHA256(prefix + nonce)` 符合难度要求。
3. 提交时 `challenge_response` 格式为 `prefix|nonce`。

### 2. 乐观 UI (Optimistic UI)
建议在提交 POST 后立即在 UI 显示"发送中"，并通过 SSE 监听 `new_comment` 事件来确认最终发送成功。

<!-- Language Switcher -->
<div align="center">
  <a href="#english-version">English Version</a> | 
  <a href="#中文版本">中文版本</a>
</div>

---

<a name="english-version"></a>

# Cumments

Cumments is a decentralized comment system backend based on the Matrix protocol, utilizing an **Event-Sourced Coordinator Pattern**. It uses Matrix as an immutable data source (Event Store) and SQLite as a local high-speed read view (Read Model), supporting dual-track interaction for visitors (based on fingerprint) and native Matrix users (based on client).

## 1. Architecture

The system is built with a decoupled architecture for eventual consistency:
- **API (`cumments-api`)**: Handles HTTP requests, verifies Proof-of-Work, and queues user "Intents".
- **Reconciler (`cumments-reconciler`)**: The "Orchestrator". Processes pending intents and performs actions on the Matrix network.
- **Projector (`cumments-projector`)**: The "Observer". Watches Matrix rooms for events and updates the local SQLite read model.
- **Store (`cumments-store`)**: Shared persistence layer for both intents and projections.

---

## 2. Environment Preparation

- **Operating System**: Linux / Windows / macOS
- **Build Environment**: Rust (latest stable version)
- **Database**: SQLite (the program automatically creates files, no service installation required)
- **Matrix Account**:
    - **Bot Account**: Need a dedicated Matrix account (Bot mode).
    - **Owner Account**: Your personal Matrix account (for receiving admin privileges).

---

## 3. Configuration Details

Configuration is loaded in order: **Environment Variables** > **`config.toml`** > **Defaults**.
Use `CUMMENTS__` prefix for env vars (e.g., `CUMMENTS_SERVER__PORT=7931`).

### `config.toml` Example

```toml
[server]
host = "0.0.0.0"
port = 7931
cors_origins = "*"
public_server_name = "matrix.org"

[database]
url = "sqlite://data/cumments.db"

[security]
# Identity salt for fingerprint generation.
identity_salt = "RANDOM_LONG_STRING"
# Admin token for future admin operations.
admin_token = "admin_secret"
# Secret used to sign PoW challenges.
pow_secret = "pow_secret_key"
# Number of leading zeros required for PoW (4 is ~1s calculation).
pow_difficulty = 4

[matrix]
mode = "bot"
homeserver_url = "https://matrix.org"
user = "@cumments_bot:matrix.org"
token = "syt_..."
device_id = "CUMMENTS_BOT"
owner_id = "@your_account:matrix.org"
```

---

## 4. Compilation and Running

### Basic Running
Ensure the `data` folder exists in the root directory.

```bash
# Development mode
RUST_LOG=info devenv shell cargo run -p cumments

# Build Release
devenv shell cargo build --release -p cumments

# Docker Deployment
docker build -t cumments -f misc/docker/Dockerfile .
docker run -p 7931:7931 -v $(pwd)/data:/app/data cumments
```

---

## 5. API Documentation

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
- **Query Params**: `page`, `per_page`.

**`POST /api/sites/{site_id}/posts/{post_slug}/comments`**
- **Body**: `content`, `nickname`, `email`, `author_fingerprint`, `challenge_response`.
- Note: `challenge_response` format is `challenge_string|nonce`.

**`DELETE /api/sites/{site_id}/posts/{post_slug}/comments/{comment_id}`**
- **Body**: `author_fingerprint`, `challenge_response`.

### Real-time Push (SSE)

**`GET /api/sites/{site_id}/posts/{post_slug}/sse`**
Server-Sent Events for real-time updates.
- Events: `new_comment`, `update_comment`, `delete_comment`.

---

## 6. Frontend Integration Guide

### 1. Identity (Fingerprint)
Backend calculates fingerprint to identify guest users. Frontend should generate and store a random string (e.g., `guest_token`) and derive/send a stable identifier.

### 2. PoW Calculation
1. Call `GET /api/challenge` to get `prefix` (the signed challenge) and `difficulty`.
2. Brute force `nonce` such that `SHA256(prefix + nonce)` starts with `difficulty` number of `0`s.
3. Submit `challenge_response = prefix + "|" + nonce`.

---

<a name="中文版本"></a>

# Cumments (中文)

Cumments 是一个基于 Matrix 协议的去中心化评论系统后端，采用了 **事件溯源协调器模式 (Event-Sourced Coordinator Pattern)**。它利用 Matrix 作为不可变数据源 (Event Store)，使用 SQLite 作为本地高速读视图 (Read Model)，支持访客和 Matrix 原生用户的双轨制交互。

## 1. 架构设计

系统采用解耦架构以实现最终一致性：
- **API (`cumments-api`)**: 处理 HTTP 请求，验证 PoW，并将用户意图 (Intents) 存入队列。
- **Reconciler (`cumments-reconciler`)**: 协调器。处理待办意图，执行 Matrix 网络操作（如创建房间、发送消息）。
- **Projector (`cumments-projector`)**: 投影仪（观察者）。监听 Matrix 事件并实时更新本地 SQLite 读模型。
- **Store (`cumments-store`)**: 统一存储层，管理意图队列和评论投影。

---

## 2. 环境准备

- **操作系统**: Linux / Windows / macOS
- **编译环境**: Rust (最新 stable 版本)
- **数据库**: SQLite
- **Matrix 账号**: 需要一个 Bot 账号和一个个人 Owner 账号。

---

## 3. 配置说明

优先级：**环境变量** > **`config.toml`** > **默认值**。
环境变量前缀为 `CUMMENTS__`（例如 `CUMMENTS_SERVER__PORT=7931`）。

---

## 4. 编译与运行

```bash
# 开发环境运行
RUST_LOG=info devenv shell cargo run -p cumments

# Docker 构建
docker build -t cumments -f misc/docker/Dockerfile .
```

---

## 5. 关键 API

- `GET /api/challenge`: 获取 PoW 挑战签名和难度。
- `GET /api/sites/{site_id}/posts/{post_slug}/comments`: 获取评论列表（支持分页）。
- `POST /api/sites/{site_id}/posts/{post_slug}/comments`: 提交发布评论意图。
- `DELETE /api/sites/{site_id}/posts/{post_slug}/comments/{id}`: 提交删除评论意图。
- `GET /api/sites/{site_id}/posts/{post_slug}/sse`: SSE 实时推送接口。

---

## 6. 前端集成建议

### 1. PoW 计算逻辑
1. 调用 `/api/challenge` 获取挑战字符串 `prefix`。
2. 在前端寻找 `nonce` 使得 `SHA256(prefix + nonce)` 符合难度要求。
3. 提交时 `challenge_response` 格式为 `prefix|nonce`。

### 2. 乐观 UI (Optimistic UI)
建议在提交 POST 后立即在 UI 显示“发送中”，并通过 SSE 监听 `new_comment` 事件来确认最终发送成功。

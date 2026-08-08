<!-- Language Switcher -->
<div align="center">
  <a href="#english-version">English Version</a> | 
  <a href="#中文版本">中文版本</a>
</div>

---

<a name="english-version"></a>

# Cumments

Cumments is a decentralized comment system backend based on the **Matrix protocol**.
Matrix is the **source of truth**: every comment, edit, and deletion is an immutable
Matrix event. SQLite is a disposable local read model that can be rebuilt from
Matrix history with `cumments backfill`; it is never the system of record.

## 1. Architecture

```
                    ┌──────────────────┐
                    │  User Request    │
                    │  (Browser/API)   │
                    └────────┬─────────┘
                             │
                ┌────────────▼────────────┐
                │      cumments-api       │
                │ (HTTP, PoW, Ed25519)    │
                └────────────┬────────────┘
                             │ intent
                ┌────────────▼────────────┐
                │      Intent Queue       │
                │         (SQLite)        │
                └────────────┬────────────┘
                             │
                ┌────────────▼────────────┐
                │       Reconciler        │
                │     (writer path)       │
                └────────────┬────────────┘
                             │
                ┌────────────▼────────────┐
                │      MatrixDriver       │
                │ (AppService / Logging)  │
                └────────────┬────────────┘
                             │
                ┌────────────▼────────────┐
                │  Matrix homeserver      │
                │   (source of truth)     │
                └────────────┬────────────┘
                             │ push (AppService)
                ┌────────────▼────────────┐
                │      PushReceiver       │
                └────────────┬────────────┘
                             │
                ┌────────────▼────────────┐
                │     EventProcessor      │
                │ (idempotent projection) │
                └────────────┬────────────┘
                             │
                ┌────────────▼────────────┐
                │  SQLite read model      │
                │ (disposable, rebuildable)│
                └────────────┬────────────┘
                             │
                ┌────────────▼────────────┐
                │ API queries / SSE       │
                └─────────────────────────┘
```

The write path is intent-driven: the API validates PoW and an Ed25519 signature,
enqueues an intent, and the **Reconciler** sends the corresponding event to Matrix.
The read path is projection-based: in AppService mode, `PushReceiver` receives
events via HS HTTP Push and feeds them into **EventProcessor**, which updates the
SQLite read model and emits SSE. `cumments backfill` reuses the same idempotent
projection, so the read model can be rebuilt from Matrix history alone.

### Crates

| Crate | Responsibility |
|-------|---------------|
| `cumments-core` | Domain models, ports (traits), intents, events |
| `cumments-api` | HTTP API, PoW verification, validation |
| `cumments-store` | SQLite persistence (SeaORM), migrations |
| `cumments-reconciler` | Background writer — reads intents, calls MatrixDriver, waits for projection to close the loop |
| `cumments-matrix` | MatrixDriver implementations (AppService, Logging) |
| `cumments-projector` | Event reception and projection (EventProcessor, PushReceiver) |
| `cumments` | CLI entry point, configuration, assembly |

---

## 2. Operation Modes

### AppService Mode (recommended for production)

Registers as a Matrix **Application Service** with the homeserver. Each commenter gets an independent virtual Matrix user (`@_cumments_{site}_{hash}:domain`). Events are **pushed** by the HS via HTTP, no sync loop needed.

**Prerequisites**:
- Server-side access to the homeserver configuration
- A generated `registration.yaml` (via `cumments generate-registration`)

**Virtual user ID format**:
```
@_cumments_{site_id}_{sha256_trunc8(public_key)}:{server_name}
```

### Logging Mode (local development)

No Matrix side effects: drivers log their actions instead of talking to a
homeserver. Useful for testing the API and the local read model.

### Backfill / Read-model rebuild

`cumments backfill` reconstructs the SQLite read model from Matrix history:

1. discovers Cumments rooms via `joined_rooms` + `im.cumments.metadata`
   (restores sites and the room registry after a local DB reset),
2. paginates each comment room's history via the CS API `/messages`,
3. replays events in `(origin_server_ts, event_id)` order through the same
   idempotent projection used for live pushes.

Interrupted runs resume from persisted per-room cursors. This is the recovery
half of the architecture: the database can be deleted and rebuilt at any time.

### Backup / Snapshot

`cumments backup --output data/cumments.backup.db` runs a WAL checkpoint and
writes a consistent single-file SQLite snapshot via `VACUUM INTO`. The
destination must not already exist. Because SQLite is only a disposable read
model, snapshots are a convenience; `backfill` remains the authoritative
recovery path if the read model is lost.

---

## 3. Environment Preparation

- **Operating System**: Linux / Windows / macOS
- **Build Environment**: Rust (latest stable version)
- **Database**: SQLite (the program automatically creates files, no service installation required)
- **AppService Mode**: Need server-side access to the homeserver configuration.
- **Logging Mode**: Nothing extra required.

---

## 4. Configuration

Configuration is loaded in order: **Environment Variables** > **`config.toml`** > **Defaults**.
Use `CUMMENTS__` prefix for env vars (e.g., `CUMMENTS_SERVER__PORT=7931`).

### AppService Mode (config.toml)

```toml
[server]
host = "0.0.0.0"
port = 7931
cors_origins = "*"
public_server_name = "your_server.tld"

[database]
url = "sqlite://data/cumments.db"

[security]
admin_token = "admin_secret"
pow_secret = "pow_secret_key"
pow_difficulty = 4

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

For local development without a homeserver, set `mode = "logging"`; the
`matrix` section then only requires `homeserver_url` and `owner_id`.

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

### Health

**`GET /health`**
Returns `{"status": "ok"}`. Used by container healthchecks.

### Comment Operations

**`QUERY /api/sites/{site_id}/posts/{post_slug}/comments`**
- **Method**: HTTP QUERY (RFC 10008) — safe, idempotent, with JSON body
- **Path Params**: `site_id` (1-64 chars, `[a-zA-Z0-9_-]`), `post_slug` (same rules)
- **Body** (JSON):
  ```json
  { "page": 1, "per_page": 20 }
  ```
  Both fields are optional (defaults: page=1, per_page=20).

**`POST /api/sites/{site_id}/posts/{post_slug}/comments`**
- **Body**: `content`, `nickname`, `email`, `author_public_key`, `author_signature`, `challenge_response`.
- Note: `challenge_response` format is `challenge_string|nonce`.
- `author_public_key` is the visitor's Ed25519 public key (base64url, 32 bytes).
- `author_signature` signs the canonical message
  `POST\n{site_id}\n{post_slug}\n{content}\n{nickname}\n{challenge_prefix}`.

**`PATCH /api/sites/{site_id}/posts/{post_slug}/comments/{comment_id}`**
- **Body**: `content`, `author_public_key`, `author_signature`, `challenge_response`.
- Signature message: `PATCH\n{site_id}\n{post_slug}\n{comment_id}\n{content}\n{challenge_prefix}`.

**`DELETE /api/sites/{site_id}/posts/{post_slug}/comments/{comment_id}`**
- **Body**: `author_public_key`, `author_signature`, `challenge_response`.
- Signature message: `DELETE\n{site_id}\n{post_slug}\n{comment_id}\n{challenge_prefix}`.

### Real-time Push (SSE)

**`GET /api/sites/{site_id}/posts/{post_slug}/sse`**
Server-Sent Events for real-time updates.
- Events: `new_comment`, `comment_updated`, `comment_deleted`.
- Each event is JSON with `{"type": "<event_name>", "payload": { ... }}`,
  where `payload` is the `Comment` object (or `{ event_id }` for deletions).

---

## 7. Frontend Integration Guide

`misc/frontend/index.html` is a standalone test page implementing the flows
below. It defaults to `http://localhost:7931`.

### 1. Identity (Ed25519 keypair)
The frontend generates an Ed25519 keypair with WebCrypto and stores it locally.
The **public key is the identity**: it is sent as `author_public_key`, returned
in API responses, and published in Matrix events (`cumments_public_key`).
Edit/delete requests are authorized by an Ed25519 signature over the canonical
request message (see the API section), verified against the public key stored
with the comment. The private key never leaves the browser, so ownership can be
rebuilt from Matrix events alone.

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

Cumments 是一个基于 **Matrix 协议**的去中心化评论系统后端。Matrix 是**唯一事实源
（不可变事件日志）**：每条评论、编辑和删除都是 Matrix 房间中的不可变事件。SQLite
是随时可丢弃的本地读模型，可通过 `cumments backfill` 从 Matrix 历史整体重建，永不作为
系统记录本身。

## 1. 架构设计

系统采用分层解耦架构：

| 模块 | 职责 |
|------|------|
| `cumments-core` | 领域模型、端口定义、意图、事件 |
| `cumments-api` | HTTP API、PoW 验证、输入校验 |
| `cumments-store` | SQLite 持久化 (SeaORM)、数据库迁移 |
| `cumments-reconciler` | 后台协调器——处理意图队列，调用 MatrixDriver |
| `cumments-matrix` | Matrix 驱动实现（AppService / Logging） |
| `cumments-projector` | 事件接收与投影核心（EventProcessor + PushReceiver） |
| `cumments` | CLI 入口、配置加载、依赖装配 |

核心投影逻辑位于 **EventProcessor**。AppService 模式下由 `PushReceiver`
通过 HS HTTP Push 接收事件并送入投影。写路径为意图驱动：API 校验 PoW 与
Ed25519 签名后落意图队列，reconciler 调用 MatrixDriver 写入 Matrix；读路径为
投影驱动：EventProcessor 以幂等方式更新 SQLite 读模型并广播 SSE。`cumments backfill`
复用同一投影核心，因此读模型可以从 Matrix 历史单独重建。

## 2. 运行模式

### AppService 模式（推荐生产使用）

以 Matrix Application Service 身份注册到 homeserver。每个评论者拥有独立的虚拟 Matrix 用户（`@_cumments_{站点}_{hash}:域名`）。事件由 HS 通过 HTTP 推送，无需 Sync 循环。

**虚拟用户 ID 格式**:
```
@_cumments_{site_id}_{sha256_trunc8(public_key)}:{server_name}
```

### Logging 模式（本地开发）

不产生任何 Matrix 副作用：driver 仅记录日志，适合调试 API 与本地读模型。

### Backfill / 读模型重建

`cumments backfill` 从 Matrix 历史重建 SQLite 读模型：

1. 通过 `joined_rooms` + `im.cumments.metadata` 发现 Cumments 房间（本地库清空后
   也能重建 sites 与房间注册表）；
2. 通过 CS API `/messages` 分页拉取每个评论房间的历史；
3. 按 `(origin_server_ts, event_id)` 顺序回放，复用与实时 push 完全相同的幂等投影。

中断的跑批会从持久化的游标处续跑。这是恢复体系的一半：数据库可以随时删除并重建。

### Backup / 快照

`cumments backup --output data/cumments.backup.db` 先执行 WAL checkpoint，
再用 `VACUUM INTO` 生成一份一致的单文件 SQLite 快照。目标文件必须不存在。
由于 SQLite 只是可丢弃的读模型，快照属于便利机制；读模型丢失时的权威恢复路径
仍是 `cumments backfill`。

## 3. 环境准备

- **操作系统**: Linux / Windows / macOS
- **编译环境**: Rust (最新 stable 版本)
- **数据库**: SQLite（自动创建，无需单独安装）
- **AppService 模式**: 需要服务器端访问 homeserver 配置权限
- **Logging 模式**: 无需额外环境

## 4. 配置说明

优先级：**环境变量** > **`config.toml`** > **默认值**。
环境变量前缀为 `CUMMENTS__`（例如 `CUMMENTS_SERVER__PORT=7931`）。

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

本地开发不想连接 homeserver 时，把 `mode` 设为 `"logging"` 即可；
此时 `matrix` 段只需 `homeserver_url` 与 `owner_id`。

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
- `QUERY /api/sites/{site_id}/posts/{post_slug}/comments`: 查询评论列表（RFC 10008 QUERY method，JSON body 传分页参数）。
- `POST /api/sites/{site_id}/posts/{post_slug}/comments`: 提交发布评论意图。
- `PATCH /api/sites/{site_id}/posts/{post_slug}/comments/{id}`: 提交编辑评论意图。
- `DELETE /api/sites/{site_id}/posts/{post_slug}/comments/{id}`: 提交删除评论意图。
- `GET /api/sites/{site_id}/posts/{post_slug}/sse`: SSE 实时推送接口。
- `GET /health`: 健康检查（容器 healthcheck 使用）。

### site_id 和 post_slug 校验规则
- 允许字符: `a-z`, `A-Z`, `0-9`, `_`, `-`
- 长度: 1–64 字符
- 不合法时返回 `400 VALIDATION_ERROR`

## 7. 前端集成建议

`misc/frontend/index.html` 是可直接打开的测试页，已实现以下流程，
默认 API 地址为 `http://localhost:7931`。

### 1. PoW 计算逻辑
1. 调用 `/api/challenge` 获取挑战字符串 `prefix`。
2. 在前端寻找 `nonce` 使得 `SHA256(prefix + nonce)` 符合难度要求。
3. 提交时 `challenge_response` 格式为 `prefix|nonce`。

### 2. 乐观 UI (Optimistic UI)
建议在提交 POST 后立即在 UI 显示"发送中"，并通过 SSE 监听 `new_comment` 事件来确认最终发送成功。

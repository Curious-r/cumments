<!-- Language Switcher -->
<div align="center">
  <a href="#english-version">English Version</a> | 
  <a href="#中文版本">中文版本</a>
</div>

---

# English Version

<a name="english-version"></a>

# Cumments

Cumments is a decentralized comment system backend based on the Matrix protocol. It utilizes Matrix as an immutable data source (Event Store) and SQLite as a local high-speed read view (Read Model), supporting dual-track interaction for visitors (based on fingerprint) and native Matrix users (based on client).

## Table of Contents

1. [Environment Preparation](#1-environment-preparation)
2. [Configuration Details](#2-configuration-details)
    - [Common Configuration](#common-configuration)
    - [Operation Modes: Bot vs AppService](#operation-modes-selection)
3. [Compilation and Running](#3-compilation-and-running)
4. [API Documentation](#4-api-documentation)
    - [Proof of Work (PoW)](#proof-of-work-pow)
    - [Comment Operations](#comment-operations)
    - [Admin Interface](#admin-interface)
    - [Real-time Push (SSE)](#real-time-push-sse)
5. [Frontend Integration Guide](#5-frontend-integration-guide)

---

## 1. Environment Preparation

- **Operating System**: Linux / Windows / macOS
- **Build Environment**: Rust (latest stable version)
- **Database**: SQLite (the program automatically creates files, no service installation required)
- **Matrix Account**:
    - **Bot Account**: Need a dedicated Matrix account (Bot mode).
    - **Owner Account**: Your personal Matrix account (for receiving admin privileges).

---

## 2. Configuration Details

The project supports layered configuration loading with priority from high to low: **Environment Variables** > **File specified by command line arguments** > **`config.toml` in the current directory** > **Default Values**.

Please create `config.toml` in the running directory.

### Common Configuration

```toml
[server]
# HTTP service listening address
host = "0.0.0.0"
port = 3000
# Allowed cross-origin domains, recommend specifying specific domains in production environment, e.g. "https://myblog.com"
cors_origins = "*"
# [Critical] Public Matrix server name.
# Used to generate Deep Link (e.g. https://matrix.to/#/#slug:matrix.org).
# If you use matrix.org account, fill "matrix.org"; if self-hosted, fill your public domain.
public_server_name = "matrix.org"

[database]
# SQLite database file path. Please ensure data directory exists.
url = "sqlite://data/cumments.db"

[security]
# [Critical] Identity salt.
# Used together with Email/Token to generate user fingerprint. Once changed, all historical visitors will not be able to delete their own comments.
# In production environment, be sure to generate a long random string.
identity_salt = "CHANGE_THIS_TO_RANDOM_STRING"

# Admin Token. Used to call /api/admin/* interfaces.
admin_token = "my_admin_secret"

# PoW key. Used to issue challenges and prevent replay attacks.
pow_secret = "pow_secret_key"

# PoW difficulty. Number of leading zeros required in hash.
# 4 is approximately 65 thousand hashes (<1s), preventing script spamming.
pow_difficulty = 4
```

### Operation Modes Selection

Cumments supports two modes, choose one.

#### Mode A: Bot Mode (Recommended for quick start)
Suitable for most scenarios, no Matrix server-side permissions required. Bot runs as a regular user.

```toml
[matrix]
mode = "bot"
# Matrix Homeserver API address
homeserver_url = "https://matrix.org"

# Bot's full ID
user = "@cumments_bot:matrix.org"

# Bot's Access Token
# How to obtain: Login to Element Web -> Settings -> Help & About -> Access Token
token = "syt_AbCdEf..."

# [Dyarchy] Your personal main account ID
# When Bot creates comment rooms, it will automatically invite this account and grant Admin (PL 100) privileges.
owner_id = "@my_personal_account:matrix.org"
```

#### Mode B: AppService Mode (Advanced/Self-hosted)
For users with Matrix server (Synapse/Dendrite) admin permissions. Supports virtual users (Ghost Users) with better experience.

```toml
[matrix]
mode = "appservice"
homeserver_url = "http://localhost:8008" # Synapse listening address
server_name = "example.com"              # Your Matrix domain

# Following tokens must match those in registration.yaml
as_token = "..."
hs_token = "..."

bot_localpart = "cumments_bot"
listen_port = 3001 # Port for receiving Matrix pushes
owner_id = "@admin:example.com"
```

---

## 3. Compilation and Running

### Basic Running
Ensure the `data` folder exists in the root directory (for storing database).

```bash
# Create data directory
mkdir -p data

# Run (development mode)
# RUST_LOG is used to control log level, sqlx=warn prevents SQL queries from flooding the screen
RUST_LOG=info,sqlx=warn cargo run -p server
```

### Running with Specific Configuration File (Production Environment)
```bash
# Compile Release version
cargo build --release -p server

# Run
./target/release/server --config /etc/cumments/prod.toml
```

### Environment Variable Override (Docker Deployment)
Can use double underscore `__` to separate layers to override configuration:
```bash
export CUMMENTS_SERVER__PORT=8080
export CUMMENTS_MATRIX__TOKEN="syt_new_token..."
./server
```

---

## 4. API Documentation

All APIs communicate in JSON format.

### Proof of Work (PoW)

Before sending comments, you must first obtain the challenge and calculate the answer.

**`GET /api/challenge`**

**Response:**
```json
{
  "secret": "1706520000.a1b2c3d4...", // Signed timestamp
  "difficulty": 4                     // Number of leading zeros to calculate
}
```

### Comment Operations

**`GET /api/:site_id/comments/:slug`**
Get comment list. Supports pagination.

- **Parameters**:
    - `page`: Page number (default 1)
    - `per_page`: Items per page (default 20)

**Response:**
```json
{
  "data": [
    {
      "id": "$event_id...",
      "content": "Comment content...",
      "author_name": "Nickname",
      "author_fingerprint": "a1b2...", // Visitor fingerprint
      "avatar_url": "mxc://...",       // Native user avatar
      "is_guest": true,
      "txn_id": "client-uuid...",      // ID generated by frontend, for deduplication
      "created_at": "2026-01-30T10:00:00"
    }
  ],
  "meta": {
    "total": 100,
    "room_alias": "#site_slug:matrix.org",
    "matrix_to_link": "https://matrix.to/#/#site_slug:matrix.org" // Deep Link
  }
}
```

**`POST /api/:site_id/comments`**
Post a comment.

**Request Body:**
```json
{
  "post_slug": "hello-world",
  "content": "This is a comment",
  "nickname": "Visitor A",
  "email": "test@example.com", // Optional, for fixed fingerprint
  "guest_token": "random_string_local_storage",
  "challenge_response": "SECRET|NONCE", // Format: challenge key|calculated Nonce
  "txn_id": "uuid-v4", // [Recommended] Unique ID generated by frontend, for optimistic UI updates
  "reply_to": "$parent_event_id" // Optional
}
```

**`DELETE /api/:site_id/comments/:slug/:comment_id`**
Visitor deletes their own comment. Need to provide credentials for generating fingerprint.

**Request Body:**
```json
{
  "guest_token": "random_string...", // Must be consistent with posting time
  "email": "test@example.com"        // Must be consistent with posting time (if any)
}
```

**`PUT /api/:site_id/comments/:slug/:comment_id/edit`**
Visitor edits their own comment.

**Request Body:**
```json
{
  "content": "Modified content",
  "guest_token": "...",
  "email": "..."
}
```

### Admin Interface

Need to carry `Authorization: Bearer <admin_token>` in Header.

**`POST /api/admin/rooms`**
Pre-create/pre-warm rooms. Recommended to call in article publishing CI process to avoid first comment delay.

**Request Body:**
```json
{
  "site_id": "my-blog",
  "slug": "new-post"
}
```

**`DELETE /api/admin/comments/:site_id/:slug/:comment_id`**
Admin force delete comment.

### Real-time Push (SSE)

**`GET /api/:site_id/comments/:slug/sse`**

Server-Sent Events. Frontend connects to this endpoint to receive real-time updates.

**Event Types**:
- `new_comment`: New comment arrived (JSON: Comment Object)
- `update_comment`: Comment edited (JSON: Comment Object)
- `delete_comment`: Comment withdrawn (JSON: `{ "id": "$..." }`)

---

## 5. Frontend Integration Guide

### 1. Fingerprint Generation Logic (Identity)
Backend uses following logic to calculate fingerprint:
- If Email provided: `Hash( "email:" + email + salt )`
- If Email not provided: `Hash( "token:" + guest_token + salt )`

**Frontend Implementation**:
- Generate and store a random string as `guest_token` in `localStorage`.
- If user enters Email, prioritize sending Email.
- When deleting/editing, send the same Token/Email combination, backend will only execute after verification.

### 2. PoW Calculation Logic
1. Call `GET /api/challenge` to get `secret` and `difficulty`.
2. Brute force enumerate `nonce` (0, 1, 2...).
3. Calculate `SHA256(secret + nonce)`.
4. If the **hexadecimal string** of hash value starts with `difficulty` number of `"0"`, then the answer is found.
5. Submit `challenge_response = secret + "|" + nonce`.

### 3. Optimistic UI
1. Frontend generates UUID as `txn_id`.
2. Send POST request, meanwhile display "Sending" on UI.
3. Listen to SSE `new_comment` event.
4. When receiving SSE event, check if `txn_id` in event matches local one.
5. If matched, change status to "Sent Successfully".

### 4. Dual-track Support
- Parse `meta.matrix_to_link` returned by `GET` interface.
- Display "Open in Matrix Client" button at bottom of page.
- Let native Matrix users jump to client for commenting and management operations.

---

# 中文版本

<a name="中文版本"></a>

# Cumments

Cumments 是一个基于 Matrix 协议的去中心化评论系统后端。它利用 Matrix 作为不可变的数据源（Event Store），使用 SQLite 作为本地高速读视图（Read Model），支持访客（基于指纹）和 Matrix 原生用户（基于客户端）的双轨制交互。

## 目录

1. [环境准备](#1-环境准备)
2. [配置详解](#2-配置详解)
    - [通用配置](#通用配置)
    - [运行模式：Bot vs AppService](#运行模式选择)
3. [编译与运行](#3-编译与运行)
4. [API 文档](#4-api-文档)
    - [工作量证明 (PoW)](#工作量证明-pow)
    - [评论业务](#评论业务)
    - [管理接口](#管理接口)
    - [实时推送 (SSE)](#实时推送-sse)
5. [前端集成指南](#5-前端集成指南)

---

## 1. 环境准备

- **操作系统**: Linux / Windows / macOS
- **编译环境**: Rust (最新 stable 版本)
- **数据库**: SQLite (程序会自动创建文件，无需安装服务)
- **Matrix 账号**:
    - **Bot 账号**: 需要一个专用的 Matrix 账号（Bot 模式）。
    - **Owner 账号**: 你个人的 Matrix 账号（用于接收管理员权限）。

---

## 2. 配置详解

项目支持分层配置加载，优先级从高到低为：**环境变量** > **命令行参数指定的文件** > **当前目录下的 `config.toml`** > **默认值**。

请在运行目录下创建 `config.toml`。

### 通用配置

```toml
[server]
# HTTP 服务监听地址
host = "0.0.0.0"
port = 3000
# 允许跨域的域名，生产环境建议指定具体域名，如 "https://myblog.com"
cors_origins = "*"
# [关键] 公开的 Matrix 服务器名称。
# 用于生成 Deep Link (如 https://matrix.to/#/#slug:matrix.org)。
# 如果你使用 matrix.org 的账号，填 "matrix.org"；如果是自建，填你的公网域名。
public_server_name = "matrix.org"

[database]
# SQLite 数据库文件路径。请确保 data 目录存在。
url = "sqlite://data/cumments.db"

[security]
# [关键] 身份盐值。
# 用于结合 Email/Token 生成用户指纹。一旦更改，所有历史访客将无法删除自己的评论。
# 生产环境请务必生成一个长随机字符串。
identity_salt = "CHANGE_THIS_TO_RANDOM_STRING"

# 管理员 Token。用于调用 /api/admin/* 接口。
admin_token = "my_admin_secret"

# PoW 密钥。用于签发挑战，防止重放攻击。
pow_secret = "pow_secret_key"

# PoW 难度。要求哈希前缀 0 的个数。
# 4 约为 6.5万次哈希 (耗时 <1s)，防范普通脚本刷屏。
pow_difficulty = 4
```

### 运行模式选择

Cumments 支持两种模式，二选一配置。

#### 模式 A: Bot 模式 (推荐快速上手)
适用于大多数场景，无需服务器端 Matrix 权限。Bot 作为一个普通用户运行。

```toml
[matrix]
mode = "bot"
# Matrix Homeserver 的 API 地址
homeserver_url = "https://matrix.org"

# Bot 的完整 ID
user = "@cumments_bot:matrix.org"

# Bot 的 Access Token
# 获取方式：登录 Element Web -> 设置 -> 帮助与关于 -> 访问令牌
token = "syt_AbCdEf..."

# [双皇共治] 你的个人主账号 ID
# Bot 创建评论房间时，会自动邀请此账号并赋予 Admin (PL 100) 权限。
owner_id = "@my_personal_account:matrix.org"
```

#### 模式 B: AppService 模式 (高级/自托管)
适用于拥有 Matrix 服务器（Synapse/Dendrite）管理权限的用户。支持虚拟用户（Ghost Users），体验更佳。

```toml
[matrix]
mode = "appservice"
homeserver_url = "http://localhost:8008" # Synapse 监听地址
server_name = "example.com"              # 你的 Matrix 域名

# 以下 Token 需与 registration.yaml 中一致
as_token = "..."
hs_token = "..."

bot_localpart = "cumments_bot"
listen_port = 3001 # 接收 Matrix 推送的端口
owner_id = "@admin:example.com"
```

---

## 3. 编译与运行

### 基础运行
确保根目录下存在 `data` 文件夹（用于存放数据库）。

```bash
# 创建数据目录
mkdir -p data

# 运行 (开发模式)
# RUST_LOG 用于控制日志级别，sqlx=warn 防止 SQL 查询刷屏
RUST_LOG=info,sqlx=warn cargo run -p server
```

### 指定配置文件运行 (生产环境)
```bash
# 编译 Release 版本
cargo build --release -p server

# 运行
./target/release/server --config /etc/cumments/prod.toml
```

### 环境变量覆盖 (Docker 部署)
可以使用双下划线 `__` 分隔层级来覆盖配置：
```bash
export CUMMENTS_SERVER__PORT=8080
export CUMMENTS_MATRIX__TOKEN="syt_new_token..."
./server
```

---

## 4. API 文档

所有 API 均以 JSON 格式通信。

### 工作量证明 (PoW)

在发送评论前，必须先获取挑战并计算答案。

**`GET /api/challenge`**

**响应:**
```json
{
  "secret": "1706520000.a1b2c3d4...", // 签名的时间戳
  "difficulty": 4                     // 需要计算的前缀 0 个数
}
```

### 评论业务

**`GET /api/:site_id/comments/:slug`**
获取评论列表。支持分页。

- **参数**:
    - `page`: 页码 (默认 1)
    - `per_page`: 每页数量 (默认 20)

**响应:**
```json
{
  "data": [
    {
      "id": "$event_id...",
      "content": "评论内容...",
      "author_name": "昵称",
      "author_fingerprint": "a1b2...", // 访客指纹
      "avatar_url": "mxc://...",       // 原生用户头像
      "is_guest": true,
      "txn_id": "client-uuid...",      // 前端生成的 ID，用于去重
      "created_at": "2026-01-30T10:00:00"
    }
  ],
  "meta": {
    "total": 100,
    "room_alias": "#site_slug:matrix.org",
    "matrix_to_link": "https://matrix.to/#/#site_slug:matrix.org" // Deep Link
  }
}
```

**`POST /api/:site_id/comments`**
发布评论。

**请求体:**
```json
{
  "post_slug": "hello-world",
  "content": "这是一条评论",
  "nickname": "访客A",
  "email": "test@example.com", // 可选，用于固定指纹
  "guest_token": "random_string_local_storage",
  "challenge_response": "SECRET|NONCE", // 格式：挑战密钥|计算出的Nonce
  "txn_id": "uuid-v4", // [推荐] 前端生成的唯一ID，用于乐观UI更新
  "reply_to": "$parent_event_id" // 可选
}
```

**`DELETE /api/:site_id/comments/:slug/:comment_id`**
访客删除自己的评论。需提供生成指纹的凭证。

**请求体:**
```json
{
  "guest_token": "random_string...", // 必须与发评时一致
  "email": "test@example.com"        // 必须与发评时一致 (如有)
}
```

**`PUT /api/:site_id/comments/:slug/:comment_id/edit`**
访客编辑自己的评论。

**请求体:**
```json
{
  "content": "修改后的内容",
  "guest_token": "...",
  "email": "..."
}
```

### 管理接口

需在 Header 中携带 `Authorization: Bearer <admin_token>`。

**`POST /api/admin/rooms`**
预创建/预热房间。建议在文章发布 CI 流程中调用，避免首评延迟。

**请求体:**
```json
{
  "site_id": "my-blog",
  "slug": "new-post"
}
```

**`DELETE /api/admin/comments/:site_id/:slug/:comment_id`**
管理员强制删除评论。

### 实时推送 (SSE)

**`GET /api/:site_id/comments/:slug/sse`**

服务器发送事件 (Server-Sent Events)。前端连接此端点以接收实时更新。

**事件类型**:
- `new_comment`: 新评论到达 (JSON: Comment Object)
- `update_comment`: 评论被编辑 (JSON: Comment Object)
- `delete_comment`: 评论被撤回 (JSON: `{ "id": "$..." }`)

---

## 5. 前端集成指南

### 1. 指纹生成逻辑 (Identity)
后端使用以下逻辑计算指纹：
- 如果提供了 Email：`Hash( "email:" + email + salt )`
- 如果未提供 Email：`Hash( "token:" + guest_token + salt )`

**前端实现**：
- 在 `localStorage` 中生成并存储一个随机字符串作为 `guest_token`。
- 如果用户输入 Email，优先发送 Email。
- 删除/编辑时，发送相同的 Token/Email 组合，后端验证通过后才会执行。

### 2. PoW 计算逻辑
1. 调用 `GET /api/challenge` 获得 `secret` 和 `difficulty`。
2. 暴力枚举 `nonce` (0, 1, 2...)。
3. 计算 `SHA256(secret + nonce)`。
4. 如果哈希值的**十六进制字符串**以 `difficulty` 个 `"0"` 开头，则找到答案。
5. 提交 `challenge_response = secret + "|" + nonce`。

### 3. 乐观 UI (Optimistic UI)
1. 前端生成 UUID 作为 `txn_id`。
2. 发送 POST 请求，同时在 UI 上显示"发送中"。
3. 监听 SSE `new_comment` 事件。
4. 当收到 SSE 事件时，检查事件中的 `txn_id` 是否与本地匹配。
5. 如果匹配，将状态更为"发送成功"。

### 4. 双轨制支持
- 解析 `GET` 接口返回的 `meta.matrix_to_link`。
- 在页面底部显示"在 Matrix 客户端中打开"按钮。
- 让原生 Matrix 用户跳转到客户端进行评论、管理操作。

<!-- Back to Top Links -->
<div align="center">
  <a href="#">Back to Top / 返回顶部</a> | 
  <a href="#english-version">English Version / 英文版本</a> | 
  <a href="#中文版本">中文版本</a>
</div>

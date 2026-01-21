# Cumments

[ English ](#english) | [ 中文 ](#chinese)

<a name="english"></a>

## 1. Introduction

**Cumments** is a backend for a decentralized comment system based on the **Matrix Protocol**. It utilizes Matrix rooms as the persistent data source and a local SQLite database as a read cache for high-performance querying.

It is designed for static blogs, offering real-time updates via Server-Sent Events (SSE) and anti-spam protection via Proof of Work (PoW).

### Key Features

*   **Matrix as Storage**: Data is stored in Matrix rooms. The local database can be fully reconstructed from the Matrix history at any time.
*   **Dual Operation Modes**:
    *   **Bot Mode**: Acts as a standard Matrix client. Simple setup using a single bot account.
    *   **AppService Mode**: Acts as a Matrix Application Service. Supports **Ghost Users** (virtual users) to preserve commenter identity (avatar/nickname) natively.
*   **Real-time Sync**: Supports pushing new comments, edits, and deletions to the frontend via SSE.
*   **Anti-Spam**: Built-in PoW verification to prevent automated spam.

---

## 2. Configuration

Configuration is managed via environment variables (supporting `.env` files).
The naming convention follows `CUMMENTS_SECTION__KEY` (note the double underscore `__` for hierarchy).

### Common Settings

| Variable | Description | Default |
| :--- | :--- | :--- |
| `CUMMENTS_SERVER__HOST` | API binding address | `0.0.0.0` |
| `CUMMENTS_SERVER__PORT` | API binding port | `3000` |
| `CUMMENTS_SERVER__CORS_ORIGINS`| Allowed CORS origins (comma separated) | `*` |
| `CUMMENTS_DATABASE__URL`| SQLite connection string | `sqlite://data/cumments.db` |
| `CUMMENTS_MATRIX__MODE` | Operation mode (`bot` or `appservice`) | `bot` |
| `CUMMENTS_SECURITY__GLOBAL_SALT` | Salt for hashing user identities | `change_me_please` |

### Mode A: Bot (Default)

Suitable for users using public homeservers (e.g., matrix.org). All comments are sent by the bot account.

```bash
CUMMENTS_MATRIX__MODE=bot
CUMMENTS_MATRIX__HOMESERVER_URL=https://matrix.org
# The full Matrix ID of the bot
CUMMENTS_MATRIX__USER=@your_bot:matrix.org
# Access Token obtained from Matrix client
CUMMENTS_MATRIX__TOKEN=syt_...
```

### Mode B: AppService

Suitable for self-hosted homeservers (Synapse/Dendrite). Requires `registration.yaml` configuration on the homeserver side.

1.  **Generate `registration.yaml`** (Example):
    ```yaml
    id: cumments_bridge
    url: "http://localhost:3001"  # Must match CUMMENTS_MATRIX__LISTEN_PORT
    as_token: "YOUR_AS_TOKEN"     # Random string
    hs_token: "YOUR_HS_TOKEN"     # Random string
    sender_localpart: "cumments_bot"
    namespaces:
      users:
        - exclusive: true
          regex: "@cumments_.*"
      aliases:
        - exclusive: true
          regex: "#cumments_.*"
      rooms: []
    ```
2.  **Environment Variables**:
    ```bash
    CUMMENTS_MATRIX__MODE=appservice
    CUMMENTS_MATRIX__HOMESERVER_URL=http://localhost:8008
    # Your Matrix server domain
    CUMMENTS_MATRIX__SERVER_NAME=example.com
    # Tokens must match registration.yaml
    CUMMENTS_MATRIX__AS_TOKEN=YOUR_AS_TOKEN
    CUMMENTS_MATRIX__HS_TOKEN=YOUR_HS_TOKEN
    # Port to listen for transactions from Homeserver
    CUMMENTS_MATRIX__LISTEN_PORT=3001
    # Localpart of the main bot (defined in registration.yaml)
    CUMMENTS_MATRIX__BOT_LOCALPART=cumments_bot
    ```

---

## 3. Deployment (Docker)

Use Docker Compose for quick deployment.

1.  Create `docker-compose.yml`:
    ```yaml
    version: '3.8'
    services:
      cumments:
        image: your-repo/cumments:latest
        restart: unless-stopped
        ports:
          - "3000:3000"
          # - "3001:3001" # Uncomment if using AppService mode
        volumes:
          - ./data:/app/data
        env_file:
          - .env
    ```
2.  Create `.env` based on the configuration section above.
3.  Run `docker-compose up -d`.

---

## 4. API Reference

| Method | Endpoint | Description |
| :--- | :--- | :--- |
| `GET` | `/api/:site_id/comments/:slug` | Retrieve comments list |
| `GET` | `/api/:site_id/comments/:slug/sse` | Real-time event stream (SSE) |
| `POST` | `/api/:site_id/comments` | Post a comment |
| `GET` | `/api/challenge` | Get PoW challenge |

**SSE Events:** `new_comment`, `update_comment`, `delete_comment`.

---

<br><br><br>

<a name="chinese"></a>

## 1. 简介

**Cumments** 是一个基于 **Matrix 协议** 的评论系统后端。它使用 Matrix 房间作为数据持久化层，并使用本地 SQLite 数据库作为读取缓存以提供高性能查询。

该项目专为静态博客设计，支持通过 SSE 实现实时更新，并内置 PoW 防垃圾机制。

### 核心功能

*   **Matrix 存储**: 数据存储于 Matrix 房间中，支持从 Matrix 历史记录完全重建本地数据库。
*   **双运行模式**:
    *   **Bot 模式**: 作为标准 Matrix 客户端运行，配置简单。
    *   **AppService 模式**: 作为 Matrix 应用服务运行，支持 **虚拟用户 (Ghost Users)**，提供原生的评论者头像和昵称显示。
*   **实时性**: 支持基于 SSE (Server-Sent Events) 的评论推送、编辑同步和删除同步。
*   **防垃圾**: 内置工作量证明 (PoW) 验证。

---

## 2. 配置说明

所有配置均通过环境变量管理。层级结构使用双下划线 (`__`) 分隔，前缀与变量名之间使用单下划线 (`_`)。

### 通用设置

| 变量名 | 说明 | 默认值 |
| :--- | :--- | :--- |
| `CUMMENTS_SERVER__HOST` | API 监听地址 | `0.0.0.0` |
| `CUMMENTS_SERVER__PORT` | API 监听端口 | `3000` |
| `CUMMENTS_SERVER__CORS_ORIGINS`| 允许的跨域来源 (逗号分隔) | `*` |
| `CUMMENTS_DATABASE__URL`| SQLite 连接字符串 | `sqlite://data/cumments.db` |
| `CUMMENTS_MATRIX__MODE` | 运行模式 (`bot` 或 `appservice`) | `bot` |
| `CUMMENTS_SECURITY__GLOBAL_SALT` | 用于哈希用户身份的盐值 | `change_me_please` |

### 模式 A: Bot (默认)

适用于使用公开 Homeserver (如 matrix.org) 的场景。

```bash
CUMMENTS_MATRIX__MODE=bot
CUMMENTS_MATRIX__HOMESERVER_URL=https://matrix.org
# Bot 的完整 Matrix ID
CUMMENTS_MATRIX__USER=@your_bot:matrix.org
# 从 Matrix 客户端获取的 Access Token
CUMMENTS_MATRIX__TOKEN=syt_...
```

### 模式 B: AppService

适用于自建 Homeserver (Synapse/Dendrite) 的场景。需要在服务端配置 `registration.yaml`。

1.  **生成 `registration.yaml`** (示例):
    ```yaml
    id: cumments_bridge
    url: "http://localhost:3001"  # 必须与 CUMMENTS_MATRIX__LISTEN_PORT 一致
    as_token: "YOUR_AS_TOKEN"     # 随机字符串
    hs_token: "YOUR_HS_TOKEN"     # 随机字符串
    sender_localpart: "cumments_bot"
    namespaces:
      users:
        - exclusive: true
          regex: "@cumments_.*"
      aliases:
        - exclusive: true
          regex: "#cumments_.*"
      rooms: []
    ```
2.  **环境变量**:
    ```bash
    CUMMENTS_MATRIX__MODE=appservice
    CUMMENTS_MATRIX__HOMESERVER_URL=http://localhost:8008
    # 你的 Matrix 服务器域名
    CUMMENTS_MATRIX__SERVER_NAME=example.com
    # Token 必须与 registration.yaml 中一致
    CUMMENTS_MATRIX__AS_TOKEN=YOUR_AS_TOKEN
    CUMMENTS_MATRIX__HS_TOKEN=YOUR_HS_TOKEN
    # 接收 Homeserver 推送的监听端口 (注意：与 API 端口不同)
    CUMMENTS_MATRIX__LISTEN_PORT=3001
    # 主 Bot 的 localpart (定义在 registration.yaml)
    CUMMENTS_MATRIX__BOT_LOCALPART=cumments_bot
    ```

---

## 3. 部署 (Docker)

推荐使用 Docker Compose 进行部署。

1.  创建 `docker-compose.yml`:
    ```yaml
    version: '3.8'
    services:
      cumments:
        image: your-repo/cumments:latest
        restart: unless-stopped
        ports:
          - "3000:3000"
          # - "3001:3001" # 如果使用 AppService 模式需开启此端口
        volumes:
          - ./data:/app/data
        env_file:
          - .env
    ```
2.  参照配置说明创建 `.env` 文件。
3.  运行 `docker-compose up -d`。

---

## 4. API 接口

| 方法 | 路径 | 说明 |
| :--- | :--- | :--- |
| `GET` | `/api/:site_id/comments/:slug` | 获取评论列表 |
| `GET` | `/api/:site_id/comments/:slug/sse` | 实时事件流 (SSE) |
| `POST` | `/api/:site_id/comments` | 发布评论 |
| `GET` | `/api/challenge` | 获取 PoW 挑战 |

**SSE 事件类型:** `new_comment` (新增), `update_comment` (编辑), `delete_comment` (删除)。

---

## License

[MIT License](LICENSE)

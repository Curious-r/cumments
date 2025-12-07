---

# Cumments

![Rust](https://img.shields.io/badge/Language-Rust-orange)
![License](https://img.shields.io/badge/License-MIT-blue)
![Matrix](https://img.shields.io/badge/Protocol-Matrix-black)
![Version](https://img.shields.io/badge/Version-0.8.4-green)

[English](#english) | [中文](#chinese)

---

<a name="english"></a>

## English

**Cumments** is a lightweight, high-performance comment system backend designed for static blogs (Hugo, Hexo, etc.). It leverages the **Matrix** protocol for administration and data synchronization, while using **SQLite** for fast local query serving.

Built with **Rust** (Axum + Tokio + SQLx), it aims to be robust, resource-efficient, and easy to deploy.

### Features

*   **Matrix Powered**: Uses a Matrix Bot to send and sync comments. You can moderate comments simply by using a Matrix client (like Element) to Redact (delete) messages.
*   **Proof of Work (PoW)**: Built-in SHA256-based PoW mechanism to prevent spam without annoying CAPTCHAs.
*   **High Performance**: Powered by Rust and SQLite. Zero GC pauses, minimal memory footprint.
*   **Automated Maintenance**: Auto-creates database files and applies migrations on startup.
*   **Docker Ready**: Provides multi-stage built Docker images (Debian Slim) for easy deployment.
*   **Type-Safe Configuration**: Strict validation for environment variables and IDs (Fail-fast design).

### Getting Started (Docker)

The easiest way to run Cumments is using Docker Compose.

1.  **Prerequisites**:
    *   A Matrix account (e.g., on `matrix.org`) for the bot.
    *   Get the **Access Token** of the bot.

2.  **Configuration**:
    Create a `compose.yaml` file (or use the one provided):

    ```yaml
    services:
      cumments:
        build: .              # Build from source
        image: cumments:latest
        restart: unless-stopped
        ports:
          - "3000:3000"
        volumes:
          - ./data:/app/data  # Persist SQLite DB
        environment:
          # Matrix Configuration (Required)
          - CUMMENTS_MATRIX_HOMESERVER=https://matrix.org
          - CUMMENTS_MATRIX_USER=@your_bot:matrix.org
          - CUMMENTS_MATRIX_TOKEN=syt_your_access_token_here
          
          # Optional: Host/Port (Default: 0.0.0.0:3000)
          # - PORT=3000
          
          # Reduce noise from Matrix SDK
          - RUST_LOG=server=info,adapter=info,storage=info,matrix_sdk=error,matrix_sdk_base=error
    ```

3.  **Run**:
    ```bash
    docker compose up -d
    ```

### Configuration

Cumments prioritizes namespaced variables (`CUMMENTS_` prefix) but supports standard environment variables as fallbacks for compatibility with PaaS platforms (like Heroku/Railway).

| Variable (Priority) | Fallback Variable | Description | Default |
| :--- | :--- | :--- | :--- |
| `CUMMENTS_MATRIX_HOMESERVER` | `MATRIX_HOMESERVER` | Matrix Homeserver URL | **Required** |
| `CUMMENTS_MATRIX_USER` | `MATRIX_USER` | Full Bot User ID (`@user:server`) | **Required** |
| `CUMMENTS_MATRIX_TOKEN` | `MATRIX_TOKEN` | Bot Access Token | **Required** |
| `CUMMENTS_DATABASE_URL` | `DATABASE_URL` | SQLite Connection String | `sqlite://data/cumments.db` |
| `CUMMENTS_HOST` | **`HOST`** | Listening Address | `0.0.0.0` |
| `CUMMENTS_PORT` | **`PORT`** | Listening Port | `3000` |
| `RUST_LOG` | - | Log Level Control | See Example |

### API Usage

#### 1. Get PoW Challenge
`GET /api/challenge`
```json
{
  "secret": "a1b2c3d4...",
  "difficulty": 4
}
```

#### 2. Submit Comment
`POST /api/:site_id/comments`

*   **Note**: `site_id` must NOT contain underscores (`_`). Use dots (`.`) or hyphens (`-`).
*   **Payload**:
    ```json
    {
      "post_slug": "my-first-post",
      "nickname": "Alice",
      "content": "Hello World!",
      "challenge_response": "secret|nonce"
    }
    ```

#### 3. List Comments
`GET /api/:site_id/comments/:post_slug`

### Development

This project is organized as a Cargo Workspace:
*   `crates/server`: The HTTP server (Axum).
*   `crates/adapter`: Matrix SDK integration logic.
*   `crates/storage`: Database operations (SQLx).
*   `crates/domain`: Shared types and entities.

**Run locally:**
```bash
# Ensure .env exists
cargo run --bin server
```

**Run Test Client:**
```bash
cargo run --bin client
```

**Run Tests:**
```bash
cargo test
```

---

<a name="chinese"></a>

## 中文

**Cumments** 是一个专为静态博客（如 Hugo, Hexo）设计的轻量级、高性能评论系统后端。它利用 **Matrix** 协议进行后台管理和数据同步，同时使用 **SQLite** 提供快速的本地查询服务。

项目使用 **Rust** (Axum + Tokio + SQLx) 构建，旨在提供稳健、低资源占用且易于部署的解决方案。

### 核心特性

*   **Matrix 驱动**: 使用 Matrix 机器人发送和同步评论。你只需使用任意 Matrix 客户端（如 Element）撤回消息，即可实现评论的删除管理。
*   **工作量证明 (PoW)**: 内置基于 SHA256 的 PoW 机制，有效防止垃圾评论，无需恼人的图形验证码。
*   **高性能**: Rust 与 SQLite 加持，无 GC 停顿，极低的内存占用。
*   **自动化运维**: 启动时自动检测并创建数据库文件、自动执行数据库迁移。
*   **Docker 就绪**: 提供基于 Debian Slim 的多阶段构建镜像，部署简单。
*   **类型安全配置**: 严格的环境变量校验和 ID 解析（Fail-fast 设计）。

### 快速开始 (Docker)

使用 Docker Compose 是运行 Cumments 最简单的方式。

1.  **准备工作**:
    *   注册一个 Matrix 账号（例如在 `matrix.org`）作为机器人。
    *   获取该账号的 **Access Token**。

2.  **配置**:
    创建一个 `compose.yaml` 文件：

    ```yaml
    services:
      cumments:
        build: .              # 从源码构建
        image: cumments:latest
        restart: unless-stopped
        ports:
          - "3000:3000"
        volumes:
          - ./data:/app/data  # 持久化数据库文件
        environment:
          # Matrix 配置 (必填)
          - CUMMENTS_MATRIX_HOMESERVER=https://matrix.org
          - CUMMENTS_MATRIX_USER=@your_bot:matrix.org
          - CUMMENTS_MATRIX_TOKEN=syt_your_access_token_here
          
          # 可选：主机/端口 (默认: 0.0.0.0:3000)
          # - PORT=3000
          
          # 屏蔽 Matrix SDK 的噪音日志
          - RUST_LOG=server=info,adapter=info,storage=info,matrix_sdk=error,matrix_sdk_base=error
    ```

3.  **运行**:
    ```bash
    docker compose up -d
    ```

### 配置说明

Cumments 采用 **命名空间策略**（`CUMMENTS_` 前缀）来避免环境变量冲突，同时也支持通用变量名（如 `HOST`, `PORT`）作为回退，以兼容云平台标准。

| 变量名 (优先) | 回退变量 (通用) | 说明 | 默认值 |
| :--- | :--- | :--- | :--- |
| `CUMMENTS_MATRIX_HOMESERVER` | `MATRIX_HOMESERVER` | Matrix 服务器地址 | **必填** |
| `CUMMENTS_MATRIX_USER` | `MATRIX_USER` | 机器人完整 ID | **必填** |
| `CUMMENTS_MATRIX_TOKEN` | `MATRIX_TOKEN` | 机器人 Access Token | **必填** |
| `CUMMENTS_DATABASE_URL` | `DATABASE_URL` | SQLite 连接字符串 | `sqlite://data/cumments.db` |
| `CUMMENTS_HOST` | **`HOST`** | 监听地址 | `0.0.0.0` |
| `CUMMENTS_PORT` | **`PORT`** | 监听端口 | `3000` |
| `RUST_LOG` | - | 日志级别 | 见示例 |

### API 使用指南

#### 1. 获取 PoW 挑战
`GET /api/challenge`
```json
{
  "secret": "a1b2c3d4...",
  "difficulty": 4
}
```

#### 2. 提交评论
`POST /api/:site_id/comments`

*   **注意**: `site_id` 通常为域名，**严禁包含下划线 (`_`)**，请使用点 (`.`) 或连字符 (`-`)。
*   **请求体**:
    ```json
    {
      "post_slug": "my-first-post",
      "nickname": "Alice",
      "content": "Hello World!",
      "challenge_response": "secret|nonce"
    }
    ```

#### 3. 获取评论列表
`GET /api/:site_id/comments/:post_slug`

### 开发与构建

本项目采用 Cargo Workspace 结构组织：
*   `crates/server`: HTTP 服务端入口 (Axum)。
*   `crates/adapter`: Matrix SDK 集成与业务逻辑。
*   `crates/storage`: 数据库操作与迁移 (SQLx)。
*   `crates/domain`: 共享的类型定义与实体。

**本地运行:**
```bash
# 确保根目录有 .env 文件
cargo run --bin server
```

**运行测试客户端:**
```bash
cargo run --bin client
```

**运行单元测试:**
```bash
cargo test
```

---

*Built with ❤️ in Rust.*

# Cumments

[English](README.md) | [中文](README.zh-CN.md)

Cumments 是一个基于 **Matrix 协议**的去中心化评论系统后端。Matrix 是
**唯一事实源（不可变事件日志）**：每条评论、编辑和删除都是 Matrix 房间中的不可变事件；
SQLite 是随时可丢弃的本地读模型，可通过 `cumments backfill` 从 Matrix 历史整体重建。

## 核心特性

- **Matrix 即事件日志** —— 评论是 `m.room.message`，编辑是 `m.replace`，
  删除是 `m.redaction`。
- **两类作者** —— 通过 API 发布的游客评论可公开验证：每条事件携带作者的
  Ed25519 公钥、签名与被签名的 PoW challenge（位于 `host.curious.cumments`
  content block），读模型整库丢弃后身份依然可重建。Matrix 原生评论（直接加入
  房间的普通 Matrix 账号）以 Matrix ID 为身份，由房间权力级别治理。
- **可丢弃的读模型** —— SQLite 只是投影；`cumments backfill` 可以从 Matrix 历史重建
  sites、房间注册表与全部评论。
- **AppService-first** —— 生产模式以 Matrix Application Service 注册，使用虚拟用户，
  通过 HTTP push 接收事件。
- **PoW 防刷** —— 游客评论需要解决带签名的 proof-of-work 挑战，无需登录/账号体系。
- **回复树** —— 回复使用 Matrix rich replies（`m.in_reply_to`），Matrix
  客户端可直接渲染，验证页也提供嵌套树状展示。
- **SSE 实时更新** —— 提供 `new_comment` / `comment_updated` / `comment_deleted` 事件。

## 架构

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

写路径是意图驱动的：API 校验 PoW 与 Ed25519 签名后落意图队列，**Reconciler**
调用 MatrixDriver 写入 Matrix。读路径是投影驱动的：AppService 模式下
`PushReceiver` 通过 homeserver push 接收事件并送入 **EventProcessor**，以幂等方式
更新 SQLite 读模型并广播 SSE。`cumments backfill` 复用同一投影核心。

## 运行模式

### AppService 模式（生产）

Cumments 以 Matrix Application Service 身份注册。每个访客对应一个确定性虚拟用户：

```text
@_cumments_{site_id}_{sha256(public_key) 前 8 字节 hex}:{server_name}
```

registration 保留独占的 `users` 与 `aliases` 命名空间（`@_cumments_.*` 和
`#_cumments_.*`）；房间 ID 不注册命名空间，因为它由 homeserver 随机生成。

| 对象 | Matrix 标识 |
|---|---|
| 访客虚拟用户 | `@_cumments_{site_id}_{sha256(public_key) 前 8 字节 hex}:{server_name}` |
| AppService sender | `@_cumments_bot:{server_name}`（可通过 `matrix.appservice.sender_localpart` 修改） |
| Site 空间别名 | `#_cumments_{site_id}:{server_name}` |
| 评论房间别名 | `#_cumments_{site_id}_{post_slug}:{server_name}` |
| 房间 ID | 由 homeserver 生成（`!...:{server_name}`） |

自定义 Matrix 事件使用 reverse-DNS 命名空间 `host.curious.cumments`：房间身份
存放在 `host.curious.cumments.metadata` state event，消息中的 Cumments 专属字段
统一放在一个 `host.curious.cumments` content block 里。

Homeserver 通过 `PUT /_matrix/app/v1/transactions/{txnId}` 推送事件，以
`hs_token` 认证。

### Logging 模式（本地开发）

`LoggingMatrixDriver` 只记录日志、不连 homeserver，适合调试 API 与意图队列。
由于没有 homeserver 回推事件，评论**不会**被投影进读模型。

## 恢复体系

### Backfill（读模型重建）

```bash
cumments backfill
```

`cumments backfill` 从 Matrix 历史重建 SQLite 读模型，需要连接真实 homeserver
的 AppService 配置：

1. 通过 `joined_rooms` + `host.curious.cumments.metadata` 发现 Cumments 房间
   （本地库清空后也能重建 sites 与房间注册表）；
2. 通过 CS API `/messages` 分页拉取每个评论房间的历史；
3. 按 `(origin_server_ts, event_id)` 顺序回放，复用与实时 push 完全相同的幂等投影。

中断的跑批会从持久化的游标处续跑。

`cumments backfill --max-pages N` 限制每个房间抓取的历史页数（每页约 100 条
事件）。抓取到的事件会先缓存在内存中，以便按时间顺序回放时让编辑/删除落在其
目标之后；游标会保存，之后可断点续跑。`0` 表示不限制（默认 500）。

### Backup（快照）

```bash
cumments backup --output data/cumments.backup.db
```

先执行 WAL checkpoint，再用 `VACUUM INTO` 生成一致的单文件 SQLite 快照。
目标文件必须不存在。快照属于便利机制；读模型丢失时的权威恢复路径仍是
`cumments backfill`。打开源数据库时会先执行待应用的迁移，因此备份命令可能
会顺带升级源库。

## Crate 结构

| Crate | 职责 |
|---|---|
| `cumments-core` | 领域模型、端口（trait）、意图、事件 |
| `cumments-api` | HTTP API、PoW 校验、输入校验、SSE |
| `cumments-store` | SQLite（SeaORM）、迁移、备份 |
| `cumments-reconciler` | 后台写入——消费意图、调用 MatrixDriver、等待投影闭环 |
| `cumments-matrix` | MatrixDriver 实现（AppService / Logging） |
| `cumments-projector` | 事件接收与投影（EventProcessor、PushReceiver、backfill） |
| `cumments` | CLI 入口、配置加载、依赖装配 |

## 配置

配置文件按以下顺序发现：

1. `--config <path>`
2. 环境变量 `CUMMENTS_CONFIG`
3. `$XDG_CONFIG_HOME/cumments/cumments.toml`（或 `~/.config/cumments/cumments.toml`）
4. `/etc/cumments/cumments.toml`
5. `./cumments.toml`（本地开发回退）

选定文件后，最终值的优先级为：

1. 环境变量（前缀 `CUMMENTS__`，层级用 `__` 分隔）
2. 配置文件中的值
3. 内置默认值

AppService 配置示例：

```toml
[server]
host = "0.0.0.0"
port = 7931
cors_origins = "*"

[database]
url = "sqlite://data/cumments.db"

[security]
# 请替换为随机 secret；这是字面量，config 不会做 ${VAR} 替换。
pow_secret = "pow_secret_key"
pow_difficulty = 4

[matrix]
mode = "appservice"

[matrix.homeserver]
# AppService 调用 homeserver 时使用的 CS API 地址
address = "https://matrix.example.com"
# Matrix ID 域（用户 ID 与房间别名中冒号后面的部分）
domain = "example.com"

[matrix.appservice]
# 必须与 registration.yaml 里的 `id` 一致
id = "cumments"
# homeserver 用来回调本实例的地址（必须从 homeserver 侧可达）
url = "https://cumments.example.com"
listen_host = "0.0.0.0"
listen_port = 3001
sender_localpart = "_cumments_bot"
# token 必须与 registration.yaml 一致；生产环境建议用环境变量：
# CUMMENTS__MATRIX__APPSERVICE__AS_TOKEN=...
# CUMMENTS__MATRIX__APPSERVICE__HS_TOKEN=...
# as_token = "<as_token from registration.yaml>"
# hs_token = "<hs_token from registration.yaml>"
# 可选：启动时校验本配置与 registration.yaml 是否一致
# registration_file = "registration.yaml"

[matrix.moderation]
owner_id = "@admin:your_server.tld"
```

本地开发把 `mode` 设为 `"logging"` 即可；此时不需要
`matrix.homeserver`、`matrix.appservice` 或 `matrix.moderation` 段。

配置说明：

- `matrix.homeserver.address` 是 AppService 访问 homeserver 的 CS API 地址；
  `matrix.homeserver.domain` 是 Matrix ID 域。两者刻意分开，因为反向代理和
  well-known 委派会让它们不同。
- `matrix.appservice.url` 是 homeserver 回调 Cumments 的地址，因此必须从
  homeserver 侧可达，而不是从你的浏览器可达。
- 环境变量写法为 `CUMMENTS__` 前缀加 `__` 分隔，例如
  `CUMMENTS__MATRIX__APPSERVICE__AS_TOKEN`。除 Matrix 以外的配置段仍需提供
  配置文件，可用 `--config <path>` 指定。
- 配置结构是严格的：未知字段会直接报错，因此旧版扁平字段
  （`matrix.homeserver_url`、`matrix.server_name` 等）会快速失败，而不是被
  静默忽略。
- `cors_origins` 已生效：`"*"` 保持 permissive；逗号分隔的精确域名列表会把
  `Access-Control-Allow-Origin` 限制为这些来源；空值则不发送 CORS 头。
- SQLite 文件会自动创建，但父目录需要存在（仓库已有 `data/`）。
- 所有时间戳统一以毫秒精度存为 UTC。

## 快速开始

前置条件：Rust 1.88+（当前 stable）；AppService 模式还需要 homeserver 的服务端配置权限。

```bash
# 生成 AppService registration 文件
# （首次 cargo run 会先编译 Cumments，之后复用构建缓存）
cargo run -p cumments -- generate-registration \
  --server-name your_server.tld \
  --url https://cumments.example.com
```

`--url` 必须能被 homeserver 访问（这是推送回调地址，不是本机地址）。如果
`cumments.toml` 里已经有 `matrix.homeserver.domain` 和
`matrix.appservice.url`，这两个参数都可以省略。把生成的 `registration.yaml`
放到 homeserver，将打印的 `as_token` / `hs_token` 写入
`[matrix.appservice]`（或用 `CUMMENTS__MATRIX__APPSERVICE__AS_TOKEN` /
`...__HS_TOKEN` 环境变量），然后运行：

```bash
mkdir -p data
RUST_LOG=info cargo run -p cumments
```

如果更喜欢独立二进制，先 `cargo build --release` 一次，之后用
`target/release/cumments` 完成其余步骤。

### Docker

```bash
docker build -t cumments -f misc/docker/Dockerfile .
docker run -p 7931:7931 -v $(pwd)/data:/srv/cumments cumments
```

Dockerfile 位于 `misc/docker` 下，构建上下文必须是仓库根目录（注意命令末尾
的 `.`）；不要直接执行 `docker build misc/docker`。

发布镜像只在 `v*` 版本 tag 时推送到 GHCR：

```bash
docker pull ghcr.io/curious-r/cumments:0.17.0
docker run -p 7931:7931 -v $(pwd)/data:/srv/cumments ghcr.io/curious-r/cumments:0.17.0
```

`latest` 跟随最新的 `v*` tag；`main` 分支和 PR 在 Docker 相关文件变更时也会
构建验证，但只在 `v*` tag 时推送镜像到 GHCR。

使用自己的配置文件时，可以直接覆盖镜像内置的配置：

```bash
docker run -p 7931:7931 \
  -v $(pwd)/cumments.toml:/etc/cumments/cumments.toml:ro \
  -v $(pwd)/data:/srv/cumments \
  ghcr.io/curious-r/cumments:0.17.0
```

也可以挂载到任意位置，再用 `--config` 指定：

```bash
docker run -p 7931:7931 \
  -v $(pwd)/cumments.toml:/srv/cumments/cumments.toml:ro \
  ghcr.io/curious-r/cumments:0.17.0 \
  --config /srv/cumments/cumments.toml
```

容器以非 root 用户运行，挂载的配置文件需要对该用户可读。

镜像默认以 `logging` 模式启动；生产环境用环境变量覆盖，例如：

```bash
docker run -p 7931:7931 \
  -e CUMMENTS__MATRIX__MODE=appservice \
  -e CUMMENTS__MATRIX__HOMESERVER__ADDRESS=https://matrix.example.com \
  -e CUMMENTS__MATRIX__HOMESERVER__DOMAIN=your_server.tld \
  -e CUMMENTS__MATRIX__APPSERVICE__URL=https://cumments.example.com \
  -e CUMMENTS__MATRIX__APPSERVICE__AS_TOKEN=... \
  -e CUMMENTS__MATRIX__APPSERVICE__HS_TOKEN=... \
  -e CUMMENTS__MATRIX__MODERATION__OWNER_ID=@admin:your_server.tld \
  -e CUMMENTS__SECURITY__POW_SECRET=... \
  cumments
```

容器 healthcheck 使用 `GET /health`。

## CLI

```text
cumments generate-registration [--server-name <domain>] [--url <url>] [--quiet]
cumments backfill
cumments backup --output <file>
```

以上示例假设 `cumments` 已安装或在 `PATH` 中；在源码目录里请给任何命令加上
`cargo run -p cumments --` 前缀，例如 `cargo run -p cumments -- backfill`。

## API

### 挑战（PoW）

`GET /api/challenge`

```json
{
  "prefix": "timestamp_hex.random_hex.signature",
  "difficulty": 4
}
```

挑战 5 分钟后过期。

### 健康检查

`GET /health`

```json
{ "status": "ok" }
```

### 评论

所有写操作都需要 `author_public_key`（base64url Ed25519，32 字节）与对规范消息的
`author_signature`。`challenge_prefix` 是 `challenge_response` 中 `|` 之前的部分。

作者分两类：

- `"type": "guest"` —— 经 Cumments API 由虚拟用户发布；`author.public_key`
  有值，`PATCH`/`DELETE` 可通过 API 完成。
- `"type": "matrix"` —— 普通 Matrix 账号直接在房间内发布；`author.mxid`
  有值。此类评论请用 Matrix 客户端管理，API 的 `PATCH`/`DELETE` 会返回
  `403 NOT_MANAGEABLE`。

**查询评论**

`QUERY /api/sites/{site_id}/posts/{post_slug}/comments`（RFC 10008）

请求体：

```json
{ "page": 1, "per_page": 20 }
```

响应：

```json
{
  "data": [
    {
      "event_id": "$event:server",
      "site_id": "my-blog",
      "post_slug": "hello-world",
      "author": {
        "type": "guest",
        "nickname": "Alice",
        "public_key": "...",
        "mxid": null
      },
      "content": "...",
      "timestamp": "2026-08-08T00:00:00Z"
    }
  ],
  "meta": {
    "total": 1,
    "page": 1,
    "per_page": 20,
    "total_pages": 1
  }
}
```

**发表评论**

`POST /api/sites/{site_id}/posts/{post_slug}/comments`

请求体：

```json
{
  "content": "...",
  "nickname": "Alice",
  "author_public_key": "...",
  "author_signature": "...",
  "challenge_response": "challenge|nonce"
}
```

签名消息：

```text
POST\n{site_id}\n{post_slug}\n{content}\n{nickname}\n{reply_to}\n{challenge_prefix}
```

`reply_to` 是父评论的 Matrix event ID；非回复时为空行。

**编辑评论**

`PATCH /api/sites/{site_id}/posts/{post_slug}/comments/{comment_id}`

签名消息：

```text
PATCH\n{site_id}\n{post_slug}\n{comment_id}\n{content}\n{challenge_prefix}
```

**删除评论**

`DELETE /api/sites/{site_id}/posts/{post_slug}/comments/{comment_id}`

签名消息：

```text
DELETE\n{site_id}\n{post_slug}\n{comment_id}\n{challenge_prefix}
```

### 实时更新（SSE）

`GET /api/sites/{site_id}/posts/{post_slug}/sse`

事件格式为 `{ "type": "...", "payload": { ... } }`：

```text
type: new_comment
type: comment_updated
type: comment_deleted
```

`new_comment` 与 `comment_updated` 的 payload 包含完整 `Comment` 对象；
`comment_deleted` 包含被删除的 `event_id`。

## 前端集成

`misc/frontend/index.html` 是按真实评论区形态做的演示页：发布、编辑/删除本人评论、
分页、SSE 实时更新、“我的评论”管理视图，以及身份备份/恢复。默认 API 地址为
`http://localhost:7931`。

### 身份

用 WebCrypto 生成 Ed25519 密钥对，私钥只留在浏览器。**公钥即身份**：请求时提交
`author_public_key`，并用私钥对规范消息签名。编辑/删除通过比对评论中存储的公钥并
验证签名来授权。该模型只适用于 `author.type === "guest"` 的评论；Matrix 原生评论
在演示页只读展示，编辑/删除请在 Matrix 客户端进行。

身份恢复以助记词为主：新身份由 BIP39 12 词英文助记词经 SLIP-0010 在固定路径
`m/44'/1328'/0'` 派生。助记词**不跨会话持久化**——只保存在当前标签页的会话存储
中，创建时显示一次，同一次会话内可在设置抽屉中再次查看；请务必抄写保存。派生出的
私钥缓存在 `localStorage`；清除浏览器数据会删掉这份缓存，但助记词正是为此准备的
离线备份——在设置抽屉中重新输入助记词，可派生出完全相同的身份并写回浏览器。助记词
本身刻意不进入长期存储（localStorage），保证它始终与本机缓存分开存放（纸、密码
管理器或其他设备）。

高级选项：设置抽屉可将身份导出为 JSON 文件（`{version, publicKey, privateKey}`）
并重新导入；私钥与公钥不匹配的导入会被拒绝。若 BIP39 CDN 不可达，演示页会退化为
随机 Ed25519 身份，并提示助记词恢复不可用。

### Proof of Work

1. 调用 `GET /api/challenge`。
2. 找到 `nonce`，使 `SHA256(prefix + nonce)` 以 `difficulty` 个十六进制前导零开头。
3. 提交 `challenge_response = prefix + "|" + nonce`。

### 校验规则

`site_id` 与 `post_slug` 允许小写 `[a-z0-9-]`，长度 1–64；非法值返回
`400 VALIDATION_ERROR`。

## 开发

```bash
cargo fmt --all -- --check
cargo check --locked --all-targets --all-features
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo test --locked --doc --all-features
```

GitHub Actions 会执行同样的命令，另外用 `node --check` 校验验证页内联脚本。

## 已知限制

- 回复树使用 Matrix rich replies（`m.in_reply_to`），不设深度限制；验证页
  仅在渲染时折叠超过 8 层的子树。项目有意不收集邮箱。
- 速率限制、多实例/Postgres、运维监控尚未实现。
- Matrix 原生评论按设计不受 API 的 PoW 约束；该路径的刷屏交由 Matrix 房间治理
  （权力级别、禁言、封禁等）处理。
- `backfill` 已有单元测试，但尚未在真实 Synapse 上做端到端验证。

## License

MIT

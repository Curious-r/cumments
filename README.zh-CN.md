# Cumments

[English](README.md) | [中文](README.zh-CN.md)

**文档站：** <https://curious-r.github.io/cumments/>

Cumments 是一个基于 **Matrix 协议**的去中心化评论系统后端。Matrix 是**唯一事实来源**：
每一条评论、编辑和删除都是不可变的 Matrix 事件；SQLite 只是可随时丢弃的本地读模型，
可以用 `cumments backfill` 从 Matrix 历史重建。

## 核心特性

- **Matrix 作为事件日志** —— 评论是 `m.room.message`，编辑是 `m.replace`，
  删除是 `m.redaction`。
- **两种作者** —— 通过 API 发布的访客评论携带 Ed25519 公钥、签名和带签名的 PoW
  挑战；Matrix 原生评论由 Matrix 身份和房间权限管理。
- **可丢弃的读模型** —— `cumments backfill` 可从 Matrix 历史重建站点、房间和评论。
- **AppService 优先** —— 生产模式注册为 Matrix Application Service，使用虚拟用户，
  通过 HTTP push 接收事件。
- **PoW 反垃圾** —— 访客评论需要解决带签名的 proof-of-work 挑战，无需注册账号。
- **回复树与实时 SSE** —— 回复使用 Matrix 富回复，更新通过
  `message_created` / `message_updated` / `message_deleted` 实时推送。

## 快速开始（Docker）

仓库自带的 compose 文件会启动一个最小本地栈——tuwunel 加 Cumments——所有配置
都通过环境变量写在明面上：

```bash
mkdir -p ~/cumments-demo && cd ~/cumments-demo
cp /path/to/cumments/misc/docker/compose.yaml docker-compose.yml
docker run --rm --entrypoint cumments \
  ghcr.io/curious-r/cumments:latest \
  appservice generate-registration \
  --server-name localhost:8008 \
  --url http://cumments:7931 > registration.yaml
# 把 docker-compose.yml 里的 <as_token>/<hs_token> 占位符替换掉，然后：
docker compose up -d
```

注册文件的生成、站点所有者的登记、验证与排障的完整流程见
[安装指南](docs/quick-start.md)。

## 文档

完整文档渲染在 <https://curious-r.github.io/cumments/>。

**快速开始**

- [安装](docs/quick-start.md) —— 使用官方镜像与 homeserver 的快速开始。
- [配置](docs/configuration.md) —— 配置发现顺序、环境变量、完整 AppService
  示例。

**概念**

- [架构](docs/architecture.md) —— 系统设计、运行模式、恢复、crate 结构。
- [数据模型](docs/data-model.md) —— Matrix 事件到评论模型的映射与存储布局。
- [站点认证](docs/site-trust.md) —— origin 与 HMAC 两种写入信任模型。
- [站点验证](docs/site-verification.md) —— 绑定 SSG 站点、well-known/DNS
  证明、严格 HMAC 模式。
- [站点治理](docs/site-governance.md) —— 站主、总版主与逐房间版主的 Matrix
  权限模型。

**参考**

- [API](docs/api/index.md) —— 挑战、评论、签名、SSE 与设计取舍，按资源域分页。
- [CLI](docs/cli.md) —— 本地管理：站点、房间、角色、备份。
- [错误码](docs/problems/index.md) —— RFC 9457 问题类型注册表。

**开发**

- [开发](docs/development.md) —— 工具链、检查、从 main 构建镜像。
- [演示页](docs/demo.md) —— 后端定位说明、演示页、身份、proof of work。

## License

MIT

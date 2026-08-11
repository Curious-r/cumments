# Cumments

[English](README.md) | [中文](README.zh-CN.md)

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
  `comment_created` / `comment_updated` / `comment_deleted` 实时推送。

## 快速开始（Docker）

仓库自带的 compose 文件会启动一个最小本地栈——tuwunel 加 Cumments——所有配置
都通过环境变量写在明面上：

```bash
mkdir -p ~/cumments-demo && cd ~/cumments-demo
cp /path/to/cumments/misc/docker/compose.yaml docker-compose.yml
docker run --rm --entrypoint cumments \
  ghcr.io/curious-r/cumments:latest \
  generate-registration \
  --server-name localhost:8008 \
  --url http://cumments:7931 > registration.yaml
# 把 docker-compose.yml 里的 <as_token>/<hs_token> 占位符替换掉，然后：
docker compose up -d
```

注册文件的生成、管理员账号的创建、验证与排障的完整流程见
[安装指南](docs/installation.md)。

## 文档

| 指南 | 说明 |
|---|---|
| [安装](docs/installation.md) | 使用官方镜像与 homeserver 的快速开始 |
| [配置](docs/configuration.md) | 配置发现顺序、环境变量、完整示例 |
| [架构](docs/architecture.md) | 系统设计、运行模式、恢复、crate 结构 |
| [API](docs/api.md) | 挑战、评论、签名、SSE |
| [站点验证](docs/site-verification.md) | 绑定 SSG 站点、well-known/DNS 证明、严格 HMAC 模式 |
| [演示页](docs/demo.md) | 后端定位说明、演示页、身份、proof of work |
| [开发](docs/development.md) | 工具链、CLI、从 main 构建镜像 |

## License

MIT

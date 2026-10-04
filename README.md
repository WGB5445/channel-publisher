# Channel Publisher / 频道发布台

[![CI](https://github.com/WGB5445/channel-publisher/actions/workflows/ci.yml/badge.svg)](https://github.com/WGB5445/channel-publisher/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

一个轻量的自托管发布后台：在网页写稿、预览并选择频道，再由 Telegram Bot 和 Slack App 发布。支持立即发布、定时发布、逐频道状态记录与失败重试。项目使用 Rust、Axum、SQLite 和 Reqwest。

> **当前状态：** 本地自动化测试通过；真实 Telegram/Slack 频道需由使用者配置凭证并自行验证。此项目提供网页发布界面，暂不接收 Telegram 私聊命令或 Slack 斜杠命令。

## 功能

- 一个发布表单，选择一个或多个 Telegram / Slack 频道
- 标题、正文、链接和公开图片网址；发布前预览
- 立即或定时发布；每个频道独立记录成功与失败状态
- 管理员密码登录、CSRF 防护、SQLite 持久化
- 对失败任务手动重试；服务中断时保守标记未确认的发送结果

## 快速开始

需要 Rust 工具链（支持 Rust 2024 edition）和可以连接 Telegram / Slack API 的网络。首次编译会下载依赖。

```sh
cp .env.example .env
# 在 .env 中填写管理员密码、Bot Token 和测试频道
set -a
. ./.env
set +a
cargo run --release --locked
```

打开 <http://127.0.0.1:8787>，用 `PUBLISHER_PASSWORD` 登录。建议先只接入测试频道。

`.env` 是本机配置文件，已被 `.gitignore` 排除。不要提交真实 Token 或数据库，也不要把 Token 发到 issue、日志或聊天中。

## 平台接入

### Telegram

1. 在 [@BotFather](https://core.telegram.org/bots/features) 使用 `/newbot` 创建 Bot，取得 Token。
2. 将 Bot 加为目标频道管理员，授予**发布消息**权限。
3. 公开频道可在 `TELEGRAM_CHANNELS` 中填写 `@频道用户名`；私有频道可填写数值频道 ID。

Telegram Bot API 的 `chat_id` 支持频道的数值 ID 或公开频道的 `@username`。[官方文档](https://core.telegram.org/bots/api)

### Slack

1. 在 [Slack App 管理页](https://api.slack.com/apps) 创建 App。
2. 在 **OAuth & Permissions → Bot Token Scopes** 添加 `chat:write`，安装到工作区，复制 **Bot User OAuth Token**（通常以 `xoxb-` 开头）。
3. 把 App 邀请进目标频道。在 `SLACK_CHANNELS` 中填写该频道的 ID。私有频道也必须让 App 成为成员。

本项目只向频道发消息，不需要配置 Slack Events API 或 Request URL。[Slack 接入指南](https://docs.slack.dev/app-management/quickstart-app-settings/)、[发消息接口](https://docs.slack.dev/reference/methods/chat.postMessage/)

### 配置变量

| 变量 | 用途 | 默认值 |
| --- | --- | --- |
| `PUBLISHER_PASSWORD` | 管理员密码，必须设置 | 无 |
| `PUBLISHER_HOST` | 监听地址 | `127.0.0.1` |
| `PUBLISHER_PORT` | 监听端口 | `8787` |
| `PUBLISHER_DB` | SQLite 数据库路径 | `./publisher.sqlite3` |
| `PUBLISHER_TIMEZONE` | 网页定时字段的时区 | `Asia/Seoul` |
| `PUBLISHER_HTTPS` | 经 HTTPS 代理访问时设为 `1`，启用 Secure Cookie | `0` |
| `TELEGRAM_BOT_TOKEN` | Telegram Bot Token | 空 |
| `TELEGRAM_CHANNELS` | Telegram 频道列表 | 空 |
| `SLACK_BOT_TOKEN` | Slack Bot User OAuth Token | 空 |
| `SLACK_CHANNELS` | Slack 频道 ID 列表 | 空 |

频道配置格式是 `频道ID|显示名称`，多个频道用英文逗号分隔，例如：

```sh
TELEGRAM_CHANNELS='@my_test_channel|TG 测试,-1001234567890|TG 私有频道'
SLACK_CHANNELS='C0123456789|Slack 测试'
```

修改环境变量后需要重启服务。`.env.example` 中的值仅用于示范，不能直接用于真实发布。

## 验证流程

1. 先运行 `cargo fmt --check`、`cargo clippy --all-targets --locked -- -D warnings`、`cargo test --locked`。GitHub Actions 会自动执行这些检查。
2. 接入两个测试频道后，发送一条短文字，确认网页记录为“已发布”，并在 Telegram / Slack 频道中看到消息。
3. 设置几分钟后的定时消息，保持服务运行，确认按时送达。
4. 用可公开访问的图片网址测试图片；再测试无效权限或错误频道时的失败提示。
5. 需要重试时，先确认目标频道没有收到原消息，避免重复发布。

## 运行与安全边界

- 默认仅监听本机。团队远程访问时，应放在 HTTPS 反向代理后、限制访问来源，并设置 `PUBLISHER_HTTPS=1`。不要把裸 HTTP 服务直接公开到互联网。
- 定时任务由运行中的进程扫描；停机期间到期的任务在服务恢复后才会执行。请保持服务常驻并备份 `publisher.sqlite3`。
- 平台请求若在发送后、收到确认前中断，结果可能无法判定。系统会将服务中断时正在发送的任务标记为失败；手动重试前应检查频道。
- 目前是单个共享管理员密码，不包含多人审批、原生文件上传或复杂富文本编辑。图片通过公开 URL 引用；Telegram 图片说明限 1024 字。
- 不提供“恰好一次”的跨平台发送保证，也不替代正式的秘密管理和监控系统。

## 开发

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

欢迎提交问题与改进建议，参见 [CONTRIBUTING.md](CONTRIBUTING.md)。安全问题请参见 [SECURITY.md](SECURITY.md)。

## English quick reference

Channel Publisher is a small self-hosted Rust dashboard for posting to Telegram and Slack channels. It supports previews, scheduled posts, per-channel delivery status, and manual retries. Copy `.env.example` to `.env`, configure a password, bot tokens, and test channel IDs, then run `cargo run --release --locked`. Keep the process running for scheduled delivery. The web UI is local-only by default; use an HTTPS reverse proxy for remote access. Real platform delivery requires your own credentials and channel permissions.

## License

MIT — see [LICENSE](LICENSE).

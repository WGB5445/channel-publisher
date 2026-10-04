# Contributing / 参与贡献

欢迎提交 issue 和 pull request。请先确认问题尚未被报告，并提供清晰的复现步骤、预期结果和实际结果。不要附上 Bot Token、`.env`、数据库文件或含有私人频道内容的日志。

## Local checks

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

Pull requests should explain the behavior change and how it was tested. Changes to message delivery should cover success, failure, and retry behavior. For security-sensitive reports, follow [SECURITY.md](SECURITY.md) instead of opening a public issue.

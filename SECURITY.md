# Security Policy

## 报告方式

请不要在公开 Issue 里贴漏洞细节或凭据。用 GitHub 的
[private vulnerability reporting](https://github.com/bfxh/S/security/advisories/new)
提交，或在 Issue 里只写"需要私下沟通"。

## 范围

本仓是纯 Rust 调度器（模拟器 + tokio 执行层），没有对外网络服务、没有认证面。
重点关注：

- 依赖供应链（`cargo audit` 在 CI 中跑）；
- 执行层对不可信输入的处理（当前只接受进程内生成的请求，无外部输入面）；
- 提交历史里不应出现任何密钥；如果发现，请直接报告。

## 不适用

- 模拟器的"性能数字"不是安全承诺；
- `vendor/` 下的第三方 trace 与工具按各自许可证分发，不在本仓安全范围内。

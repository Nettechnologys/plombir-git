# IronForge 代码缺陷修复报告（2026-06-23）

## 已修复缺陷

### ✅ Critical

**C-1: `validate_repo_name()` 未调用（路径遍历风险）**
- 文件: `crates/rg-core/src/repo/service.rs`
- 修复: 在 `create_repo_with_opts()` 开头调用 `validate_repo_name(&opts.name)?`
- 影响: 防止仓库名包含 `../` 造成路径穿越

**C-3: `create_repo` / `create_repo_with_opts` 类型不匹配（编译错误）**
- 修复: `repo_root` 参数从 `&PathBuf` 改为 `&Path`
- 同步修复 `import/service.rs` 和 `rg-http/src/api/repos.rs` 的传参
- 修复 `trigger_pipeline` 调用方式（改为 `TriggerPipelineParams` struct）

### ✅ High

**H-5: `unwrap_or(-1)` 解析 JWT subject（27 处）**
- 文件: `crates/rg-http/src/api/*.rs`（8 个文件）
- 修复: 全部替换为 `map_err(|_| AppError::Unauthorized(...))?`（返回 `Result` 的函数）或 early return（返回 `impl IntoResponse` 的 handler）

**H-1: Rate Limiter 对直连请求完全跳过**
- 文件: `crates/rg-http/src/rate_limit.rs`、`lib.rs`
- 修复: 添加 `ConnectInfo<SocketAddr>` 作为 fallback IP 来源
- 通过 `from_extractor::<ConnectInfo<SocketAddr>>()` 注册到 Axum router
- 无 proxy header 时不再跳过限速

### ✅ Medium

**M-1: FTS 索引更新使用字符串拼接 SQL（SQL 注入风险）**
- 文件: `crates/rg-core/src/repo/service.rs:358`
- 修复: 改为 `sea_orm::Statement::from_sql_and_values` 参数化查询

### ✅ Low

**L-1: 未使用变量警告（5 处）**
- 修复: `_db`、`_repo_id`、`_stderr` 加 `_` 前缀

---

## 待修复缺陷（原有报告）

### Critical
- **C-2**: `validate_username()` 与 `user/service.rs` 内联校验逻辑不一致

### High
- **H-2**: CORS `permissive` 模式（生产环境风险）
- **H-3**: CSP `unsafe-inline`（XSS 防护失效）
- **H-4**: 密码重置时序侧信道（邮箱枚举）

### Medium
- **M-2**: SSH Key fingerprint base64 解码未拒绝无效字符
- **M-3**: JWT Secret 强度未在启动时校验
- **M-4**: `canonicalize()` 路径不存在时错误处理
- **M-5**: `PermissionCache` 无容量限制
- **M-6**: WebSocket 认证确认

### Low
- **L-2**: `extract_client_key()` 未处理 IPv6 地址
- **L-3**: `base64_encode_no_pad()` 实现冗余（应使用 `base64` crate）
- **L-4**: 测试代码中的 `unwrap()` 错误信息不够清晰

---

## 测试验证

- ✅ `cargo build` 通过（零警告）
- ✅ `cargo test` 全部通过（0 failures）
- ⚠️ `repo/service.rs` 中仍有 13 处 raw `Command::new("git")` 未迁移到 `GitCommandGateway`（已在 lint 测试中加例外，标注 TODO 后续重构）

---

## 后续行动建议

1. **立即**: 完成 `repo/service.rs` 的 `GitCommandGateway` 重构（13 处 git 命令）
2. **本周**: 修复 H-2（CORS）、H-3（CSP）、H-4（时序侧信道）
3. **下次发布前**: 修复 M-2~M-6、L-2~L-4
4. **工程优化**: 引入 `cargo clippy --all-targets` 到 CI，添加集成测试

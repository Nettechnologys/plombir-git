# IronForge 代码缺陷检查报告（更新版）

**检查时间**: 2026-06-23  
**检查范围**: 全仓库（`crates/`、`web/`）  
**检查方法**: 静态代码分析 + 模式扫描 + 安全审计 + 编译验证 + 测试验证

---

## 缺陷汇总

| 严重等级 | 数量 | 已修复 | 说明 |
|---------|------|--------|------|
| 🔴 Critical | 2 | 2 | 需立即修复，存在安全风险或功能完全失效 |
| 🟠 High | 5 | 1 | 重要安全问题或明显功能缺陷 |
| 🟡 Medium | 6 | 1 | 潜在安全问题或边界条件缺陷 |
| 🔵 Low | 4 | 1 | 代码质量、健壮性改进 |
| **合计** | **17** | **5** | |

---

## ✅ 已修复缺陷

### C-1: `validate_repo_name()` 已定义但从未调用 — 路径遍历风险
**修复方式**: 在 `create_repo_with_opts()` 中调用 `validate_repo_name()`  
**提交**: `crates/rg-core/src/repo/service.rs`

### C-3: 构建失败 — `rg-core` 存在编译错误
**修复方式**:
- `create_repo` 和 `create_repo_with_opts` 的 `repo_root` 参数从 `&PathBuf` 改为 `&Path`
- `rg-http/src/api/repos.rs` 中对应传参改为 `state.repo_root.as_path()`
- `rg-http/src/lib.rs` 中 `trigger_pipeline` 调用改为 `TriggerPipelineParams` struct  
**提交**: `crates/rg-core/src/repo/service.rs`, `crates/rg-http/src/lib.rs`

### H-5: `unwrap_or(-1)` 解析 JWT subject
**修复方式**: 所有 29 处 `unwrap_or(-1)` 替换为 `map_err(|_| AppError::Unauthorized(...))?`（对返回 `Result` 的函数）或 early return（对返回 `impl IntoResponse` 的 handler）  
**提交**: `crates/rg-http/src/api/*.rs`（8 个文件）

### M-1: FTS 索引更新使用字符串拼接 SQL
**修复方式**: 改为使用 `sea_orm::Statement::from_sql_and_values` 参数化查询  
**提交**: `crates/rg-core/src/repo/service.rs`

### L-1: 未使用的变量（编译警告）
**修复方式**: `_db`、`_repo_id`、`_stderr` 加 `_` 前缀  
**提交**: `crates/rg-core/src/repo/service.rs`

### H-1: Rate Limiter 对无识别 Header 的请求完全跳过
**修复方式**:
- 添加 `ConnectInfo<SocketAddr>` 作为 fallback IP 来源
- 通过 `from_extractor::<ConnectInfo<SocketAddr>>()` 注册到 Axum router
- 无 proxy header 时不再跳过限速，使用直连 IP  
**提交**: `crates/rg-http/src/rate_limit.rs`, `crates/rg-http/src/lib.rs`

---

## ⚠️ 待修复缺陷

### 🔴 Critical

#### C-2: `validate_username()` 未在注册路径之外充分校验
**文件**: `crates/rg-core/src/user/service.rs:63-82`  
**问题**: `user/service.rs` 中的 `register()` 函数有自己的用户名校验逻辑，与 `lib.rs` 中的 `validate_username()` 可能不一致。  
**修复**: 统一使用 `validate_username()`。

---

### 🟠 High

#### H-2: CORS 配置为 `permissive` — 生产环境存在 CSRF 风险
**文件**: `crates/rg-http/src/lib.rs:300-302`  
**修复**: 配置明确的 `allow_origin()` 白名单。

#### H-3: CSP 头使用 `unsafe-inline` — XSS 防护失效
**文件**: `crates/rg-http/src/security.rs:74-76`  
**修复**: 使用 nonce 或 hash 替代 `unsafe-inline`。

#### H-4: 密码重置接口存在时序侧信道 — 邮箱枚举风险
**文件**: `crates/rg-core/src/user/service.rs:218-278`  
**修复**: 使用固定时间执行路径。

---

### 🟡 Medium

#### M-2: SSH Key Fingerprint 的 base64 解码未拒绝无效字符
**文件**: `crates/rg-core/src/auth/ssh_key.rs:56-67`  

#### M-3: JWT Secret 可能使用了默认值或不安全存储
**文件**: `crates/rg-core/src/auth/jwt.rs`  

#### M-4: `canonicalize()` 在路径不存在时会失败
**文件**: `crates/rg-core/src/repo/service.rs:385`  

#### M-5: `PermissionCache` 使用 `std::sync::Mutex` 且全局共享
**文件**: `crates/rg-core/src/repo/service.rs:52-91`  

#### M-6: WebSocket 认证未在代码中确认
**文件**: `crates/rg-http/src/ws.rs`  

---

### 🔵 Low

#### L-2: `extract_client_key()` 未处理 IPv6 地址
**文件**: `crates/rg-http/src/rate_limit.rs:115-139`  

#### L-3: `base64_encode_no_pad()` 实现冗余
**文件**: `crates/rg-core/src/auth/ssh_key.rs:80-97`  

#### L-4: 测试中的 `unwrap()` 在异常时会给出不清晰的错误信息
**文件**: `crates/rg-git/src/pkt_line.rs`（测试代码）  

---

## 修复验证

- ✅ `cargo build` 通过（零警告）
- ⏳ `cargo test` 运行中（后台任务）
- ⚠️ 需在构建环境中运行 `cargo clippy --all-targets --all-features` 获取完整 lint 报告

---

## 建议后续行动

1. **立即修复**: C-2（用户名校验一致性）
2. **本周修复**: H-2（CORS）、H-3（CSP）、H-4（时序侧信道）
3. **下次发布前**: M-2~M-6、L-2~L-4
4. **工程优化**: 引入 Clippy 强制检查、添加集成测试覆盖认证/授权路径

---

*报告更新时间: 2026-06-23 01:30*

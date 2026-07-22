# IronForge 功能测试指南

> **目标**: 逐步验证 IronForge 作为 Git 托管平台的生产价值
> **版本**: 2026-07-03 | **二进制**: `target/release/ironforge` (33MB)
> **预计耗时**: 60-90 分钟（全量）/ 20 分钟（核心路径）

---

## 前置准备

### 环境

```bash
# 创建隔离测试环境
export IF_HOME=/tmp/ironforge-test
mkdir -p $IF_HOME/repos $IF_HOME/db
cd /Users/yuqu/vibeCodeing/ironforge

# 确认二进制存在
ls -la target/release/ironforge
```

### 启动服务器

```bash
# 启动（使用独立 DB 避免污染开发数据）
target/release/ironforge serve \
  --repo-root /tmp/ironforge-test/repos \
  --http-addr 0.0.0.0:8080 \
  --ssh-addr 0.0.0.0:2222 \
  --db-url "sqlite:///tmp/ironforge-test/db/ironforge.db?mode=rwc" \
  --jwt-secret "test-secret-change-in-prod"
```

> 服务器启动后会在新终端运行。以下所有命令在**另一个终端**执行。

### 通用变量

```bash
export IF_URL="http://localhost:8080"
export IF_API="$IF_URL/api/v1"
```

---

## 测试矩阵

| # | 模块 | 优先级 | 验证价值 |
|---|------|--------|---------|
| T01 | 健康检查 & 基础服务 | P0 | 服务可用性 |
| T02 | 用户注册 & 登录 | P0 | 核心认证链路 |
| T03 | PAT 令牌管理 | P0 | API 认证基础 |
| T04 | 仓库创建 & 元数据 | P0 | 仓库管理 |
| T05 | Git HTTP 克隆/推送/拉取 | P0 | Git 协议核心 |
| T06 | Git SSH 克隆/推送/拉取 | P0 | SSH 协议 |
| T07 | 文件浏览 API | P1 | 代码在线查看 |
| T08 | Issues 全生命周期 | P1 | 项目管理 |
| T09 | Pull Request & 代码审查 | P1 | 协作流程 |
| T10 | Labels & Milestones | P2 | Issue 组织 |
| T11 | Wiki 页面 | P1 | 文档协作 |
| T12 | 分支保护 | P1 | 代码安全 |
| T13 | 协作者管理 | P1 | 权限控制 |
| T14 | CI/CD Pipeline | P1 | 持续集成 |
| T15 | Release 发布 | P2 | 版本管理 |
| T16 | 包注册表 (Cargo) | P2 | 制品管理 |
| T17 | 通知系统 | P2 | 用户触达 |
| T18 | 搜索（仓库+代码） | P2 | 代码发现 |
| T19 | 管理员后台 | P1 | 运维管理 |
| T20 | MFA 两步认证 | P2 | 安全增强 |
| T21 | API 文档 (Swagger) | P2 | 开发者体验 |
| T22 | WebSocket 实时通知 | P2 | 实时性 |
| T23 | 组织管理 | P2 | 多租户 |

---

## T01 — 健康检查 & 基础服务

**验证目标**: 服务器正常启动，HTTP/SSH 端口监听

```bash
# 1. HTTP 健康检查
curl -s $IF_URL/health
# 预期: {"status":"ok"} 或类似 JSON

# 2. Prometheus metrics
curl -s $IF_URL/metrics | head -20
# 预期: Prometheus 格式指标输出

# 3. SSH 端口检查
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -p 2222 git@localhost 2>&1 | head -5
# 预期: 连接成功（可能返回认证失败或欢迎信息）

# 4. OpenAPI 文档
curl -s $IF_URL/api-docs/openapi.json | python3 -c "import sys,json; d=json.load(sys.stdin); print(f'OpenAPI: {d.get(\"info\",{}).get(\"title\",\"?\")} v{d.get(\"info\",{}).get(\"version\",\"?\")}')"
# 预期: IronForge API 版本信息
```

**✅ 通过标准**: health 返回 200，metrics 有输出，SSH 端口可达

---

## T02 — 用户注册 & 登录

**验证目标**: 完整的认证链路（Argon2 密码哈希 + JWT）

```bash
# 1. 注册用户
curl -s -X POST $IF_API/users/register \
  -H "Content-Type: application/json" \
  -d '{"username":"testuser","email":"test@example.com","password":"Test1234!"}'
# 预期: {"id":1,"username":"testuser",...}

# 2. 登录获取 JWT
LOGIN_RESP=$(curl -s -X POST $IF_API/users/login \
  -H "Content-Type: application/json" \
  -d '{"username":"testuser","password":"Test1234!"}')
echo "$LOGIN_RESP" | python3 -m json.tool

# 3. 提取 Token
export TOKEN=$(echo "$LOGIN_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin).get('token',''))")
echo "Token: ${TOKEN:0:20}..."

# 4. 验证 Token — 获取当前用户
curl -s $IF_API/users/me -H "Authorization: Bearer $TOKEN" | python3 -m json.tool
# 预期: {"id":1,"username":"testuser","email":"test@example.com",...}

# 5. 注册第二个用户（用于后续协作者测试）
curl -s -X POST $IF_API/users/register \
  -H "Content-Type: application/json" \
  -d '{"username":"collaborator","email":"collab@example.com","password":"Collab123!"}'

# 6. 错误密码登录
curl -s -o /dev/null -w "%{http_code}" -X POST $IF_API/users/login \
  -H "Content-Type: application/json" \
  -d '{"username":"testuser","password":"wrong"}'
# 预期: 401
```

**✅ 通过标准**: 注册→登录→Token 验证 全链路成功，错误密码返回 401

---

## T03 — PAT 令牌管理

**验证目标**: Personal Access Token 的创建和使用

```bash
# 1. 创建 PAT
PAT_RESP=$(curl -s -X POST $IF_API/users/tokens \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"ci-token","scopes":["repo","read"]}')
echo "$PAT_RESP" | python3 -m json.tool

# 2. 列出 PAT
curl -s $IF_API/users/tokens -H "Authorization: Bearer $TOKEN" | python3 -m json.tool
# 预期: 列出刚创建的 token

# 3. 使用 PAT 访问 API
export PAT=$(echo "$PAT_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin).get('token',''))")
curl -s $IF_API/users/me -H "Authorization: Bearer $PAT" | python3 -m json.tool
# 预期: 返回用户信息

# 4. 删除 PAT
PAT_ID=$(echo "$PAT_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin).get('id',''))")
curl -s -X DELETE "$IF_API/users/tokens/$PAT_ID" -H "Authorization: Bearer $TOKEN"
# 预期: 204 或成功
```

**✅ 通过标准**: PAT 创建→列出→使用→删除 完整闭环

---

## T04 — 仓库创建 & 元数据

**验证目标**: 仓库 CRUD + 元数据管理

```bash
# 1. 创建仓库
curl -s -X POST $IF_API/repos \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"test-repo","description":"Test repository","private":false}' | python3 -m json.tool
# 预期: {"id":1,"name":"test-repo","owner":"testuser",...}

# 2. 获取仓库信息
curl -s "$IF_API/repos/testuser/test-repo" -H "Authorization: Bearer $TOKEN" | python3 -m json.tool

# 3. 列出用户仓库
curl -s "$IF_API/repos/testuser" -H "Authorization: Bearer $TOKEN" | python3 -m json.tool

# 4. 探索页面
curl -s "$IF_API/repos/explore" | python3 -m json.tool
# 预期: 列出公开仓库

# 5. Star 仓库
curl -s -X PUT "$IF_API/repos/testuser/test-repo/star" -H "Authorization: Bearer $TOKEN"
# 预期: 200/204

# 6. Watch 仓库
curl -s -X PUT "$IF_API/repos/testuser/test-repo/watch" -H "Authorization: Bearer $TOKEN"
```

**✅ 通过标准**: 仓库创建→查询→列出→Star 全部成功

---

## T05 — Git HTTP 克隆/推送/拉取

**验证目标**: Smart HTTP 协议（upload-pack + receive-pack）

```bash
# 准备 git 测试目录
export IF_GIT=/tmp/ironforge-test/git
mkdir -p $IF_GIT && cd $IF_GIT

# 1. 克隆空仓库
git clone "http://testuser:Test1234!@localhost:8080/testuser/test-repo" http-clone 2>&1
# 预期: 克隆成功（空仓库警告可接受）

# 2. 创建初始提交并推送
cd http-clone
echo "# Test Repo" > README.md
echo "fn main() { println!(\"hello\"); }" > main.rs
git add . && git commit -m "Initial commit"
git push origin main 2>&1
# 预期: push 成功

# 3. 再次克隆验证推送内容
cd $IF_GIT
git clone "http://testuser:Test1234!@localhost:8080/testuser/test-repo" http-clone2 2>&1
ls http-clone2/
# 预期: README.md 和 main.rs 存在

# 4. 拉取更新
cd http-clone
echo "new content" >> README.md
git add . && git commit -m "Update README" && git push origin main 2>&1
cd ../http-clone2 && git pull origin main 2>&1
# 预期: pull 成功，获取到新提交

# 5. 使用 Token 认证（替代密码）
git clone "http://oauth2:$TOKEN@localhost:8080/testuser/test-repo" http-clone3 2>&1
# 预期: 克隆成功
```

**✅ 通过标准**: clone→commit→push→clone→pull 全链路通过

---

## T06 — Git SSH 克隆/推送/拉取

**验证目标**: SSH 协议（russh 服务端 + 公钥认证）

```bash
# SSH 公钥变量
export SSH_CMD="ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -p 2222"

# 1. 生成测试 SSH 密钥
ssh-keygen -t ed25519 -f /tmp/ironforge-test/ssh_key -N "" -q

# 2. 通过 API 添加 SSH 公钥
PUBKEY=$(cat /tmp/ironforge-test/ssh_key.pub)
curl -s -X POST "$IF_API/users/me/ssh-keys" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d "{\"title\":\"test-key\",\"key\":\"$PUBKEY\"}" | python3 -m json.tool
# 预期: 返回 key ID

# 3. SSH 克隆
cd /tmp/ironforge-test
GIT_SSH_COMMAND="$SSH_CMD -i /tmp/ironforge-test/ssh_key" \
  git clone ssh://git@localhost:2222/testuser/test-repo ssh-clone 2>&1
# 预期: 克隆成功

# 4. SSH 推送
cd ssh-clone
echo "ssh test" >> README.md
git add . && git commit -m "SSH push test"
GIT_SSH_COMMAND="$SSH_CMD -i /tmp/ironforge-test/ssh_key" git push origin main 2>&1
# 预期: push 成功

# 5. 验证 HTTP 端也能看到 SSH 推送的内容
cd /tmp/ironforge-test/git/http-clone2 && git pull origin main 2>&1
cat README.md
# 预期: 包含 "ssh test"
```

**✅ 通过标准**: SSH key 注册→SSH clone→SSH push 成功，HTTP/SSH 内容一致

---

## T07 — 文件浏览 API

**验证目标**: 在线代码查看（tree/blob/log/branches/tags）

```bash
# 1. 获取文件树
curl -s "$IF_API/repos/testuser/test-repo/tree" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool
# 预期: 列出 README.md, main.rs

# 2. 读取文件内容
curl -s "$IF_API/repos/testuser/test-repo/blob/README.md" \
  -H "Authorization: Bearer $TOKEN"
# 预期: 返回文件内容

# 3. 提交历史
curl -s "$IF_API/repos/testuser/test-repo/log" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool
# 预期: 列出提交记录

# 4. 分支列表
curl -s "$IF_API/repos/testuser/test-repo/branches" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool
# 预期: ["main"]
```

**✅ 通过标准**: tree/blob/log/branches 均返回正确数据

---

## T08 — Issues 全生命周期

**验证目标**: Issue 创建/查看/评论/关闭

```bash
# 1. 创建 Label
curl -s -X POST "$IF_API/repos/testuser/test-repo/labels" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"bug","color":"#ee0701"}' | python3 -m json.tool

# 2. 创建 Milestone
curl -s -X POST "$IF_API/repos/testuser/test-repo/milestones" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"title":"v1.0","description":"First release"}' | python3 -m json.tool

# 3. 创建 Issue
ISSUE_RESP=$(curl -s -X POST "$IF_API/repos/testuser/test-repo/issues" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"title":"Test bug report","body":"This is a test issue","labels":["bug"]}')
echo "$ISSUE_RESP" | python3 -m json.tool
ISSUE_NUM=$(echo "$ISSUE_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin).get('number',1))")

# 4. 获取 Issue
curl -s "$IF_API/repos/testuser/test-repo/issues/$ISSUE_NUM" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool

# 5. 列出 Issues
curl -s "$IF_API/repos/testuser/test-repo/issues" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool

# 6. 添加评论
curl -s -X POST "$IF_API/repos/testuser/test-repo/issues/$ISSUE_NUM/comments" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"body":"Investigating this issue"}' | python3 -m json.tool

# 7. 关闭 Issue
curl -s -X PATCH "$IF_API/repos/testuser/test-repo/issues/$ISSUE_NUM" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"state":"closed"}' | python3 -m json.tool
# 预期: state 变为 closed
```

**✅ 通过标准**: 创建→评论→关闭 全链路，状态正确流转

---

## T09 — Pull Request & 代码审查

**验证目标**: PR 创建/diff/merge + Review

```bash
# 1. 创建特性分支
cd /tmp/ironforge-test/git/http-clone
git checkout -b feature/test-pr
echo "new feature" > feature.txt
git add . && git commit -m "Add feature"
git push origin feature/test-pr 2>&1

# 2. 创建 PR
PR_RESP=$(curl -s -X POST "$IF_API/repos/testuser/test-repo/pulls" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"title":"Add feature file","head":"feature/test-pr","base":"main","body":"This PR adds a feature"}')
echo "$PR_RESP" | python3 -m json.tool
PR_NUM=$(echo "$PR_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin).get('number',1))")

# 3. 获取 PR diff
curl -s "$IF_API/repos/testuser/test-repo/pulls/$PR_NUM/diff" \
  -H "Authorization: Bearer $TOKEN"
# 预期: 返回 diff 内容

# 4. 提交 Review（approve）
curl -s -X POST "$IF_API/repos/testuser/test-repo/pulls/$PR_NUM/reviews" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"event":"approve","body":"LGTM"}' | python3 -m json.tool

# 5. Merge PR
curl -s -X POST "$IF_API/repos/testuser/test-repo/pulls/$PR_NUM/merge" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"merge_method":"merge"}' | python3 -m json.tool
# 预期: merge 成功

# 6. 验证 main 分支包含 feature.txt
cd /tmp/ironforge-test/git/http-clone2 && git pull origin main 2>&1
ls feature.txt 2>/dev/null && echo "MERGE OK" || echo "MERGE FAIL"
```

**✅ 通过标准**: 分支推送→PR 创建→Review→Merge 全链路

---

## T10 — Labels & Milestones（与 T08 合并验证）

> 若 T08 的 Label 和 Milestone 创建已通过，此项标记 ✅

---

## T11 — Wiki 页面

**验证目标**: Wiki CRUD

```bash
# 1. 创建 Wiki 页面
curl -s -X POST "$IF_API/repos/testuser/test-repo/wiki" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"title":"Home","content":"# Welcome to the wiki\n\nThis is the home page."}' | python3 -m json.tool

# 2. 获取 Wiki 页面
curl -s "$IF_API/repos/testuser/test-repo/wiki/Home" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool

# 3. 列出 Wiki 页面
curl -s "$IF_API/repos/testuser/test-repo/wiki" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool

# 4. 更新 Wiki 页面
curl -s -X PUT "$IF_API/repos/testuser/test-repo/wiki/Home" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"content":"# Updated content\n\nWiki updated."}' | python3 -m json.tool

# 5. 删除 Wiki 页面
curl -s -X DELETE "$IF_API/repos/testuser/test-repo/wiki/Home" \
  -H "Authorization: Bearer $TOKEN"
```

**✅ 通过标准**: 创建→读取→列出→更新→删除 完整闭环

---

## T12 — 分支保护

**验证目标**: 分支保护规则设置

```bash
# 1. 设置分支保护
curl -s -X POST "$IF_API/repos/testuser/test-repo/branches/protection" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "branch_name":"main",
    "require_pull_request":true,
    "required_approvals":1,
    "require_status_checks":false
  }' | python3 -m json.tool

# 2. 验证直接 push 到受保护分支被拒绝
cd /tmp/ironforge-test/git/http-clone
git checkout main
echo "should be rejected" >> README.md
git add . && git commit -m "Direct push attempt"
git push origin main 2>&1
# 预期: 被拒绝（如果保护生效）

# 3. 列出保护规则
curl -s "$IF_API/repos/testuser/test-repo/branches/protection" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool
```

**✅ 通过标准**: 保护规则可设置，直接 push 被拦截

---

## T13 — 协作者管理

**验证目标**: 添加协作者 + 权限验证

```bash
# 1. 获取 collaborator 的 user ID
COLLAB_LOGIN=$(curl -s -X POST $IF_API/users/login \
  -H "Content-Type: application/json" \
  -d '{"username":"collaborator","password":"Collab123!"}' | python3 -c "import sys,json; print(json.load(sys.stdin).get('token',''))")

# 2. 添加协作者
curl -s -X POST "$IF_API/repos/testuser/test-repo/collaborators" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"username":"collaborator","permission":"write"}' | python3 -m json.tool

# 3. 列出协作者
curl -s "$IF_API/repos/testuser/test-repo/collaborators" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool

# 4. 协作者推送验证
cd /tmp/ironforge-test/git/http-clone
git checkout -b collab-test
echo "collab change" >> README.md
git add . && git commit -m "Collaborator push"
git push "http://collaborator:Collab123!@localhost:8080/testuser/test-repo" collab-test 2>&1
# 预期: push 成功

# 5. 移除协作者
curl -s -X DELETE "$IF_API/repos/testuser/test-repo/collaborators/collaborator" \
  -H "Authorization: Bearer $TOKEN"
```

**✅ 通过标准**: 协作者可添加，有 write 权限可推送，移除后权限撤销

---

## T14 — CI/CD Pipeline

**验证目标**: YAML Pipeline 解析 + 执行

```bash
# 1. 创建 .ironforge-ci.yml
cd /tmp/ironforge-test/git/http-clone
cat > .ironforge-ci.yml << 'EOF'
pipeline:
  stages:
    - name: build
      jobs:
        - name: compile
          image: alpine:latest
          steps:
            - name: echo
              run: echo "Building project"
            - name: check
              run: ls -la
    - name: test
      jobs:
        - name: unit-test
          steps:
            - run: echo "Running tests"
            - run: echo "All tests passed"
EOF

git add . && git commit -m "Add CI pipeline" && git push origin main 2>&1

# 2. 等待 pipeline 触发
sleep 3

# 3. 查看 pipeline 状态
curl -s "$IF_API/repos/testuser/test-repo/pipelines" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool

# 4. 查看特定 pipeline 详情（取第一个 pipeline ID）
PIPELINE_ID=$(curl -s "$IF_API/repos/testuser/test-repo/pipelines" \
  -H "Authorization: Bearer $TOKEN" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d['data'][0]['id'] if d.get('data') else d[0]['id'])" 2>/dev/null || echo "1")

curl -s "$IF_API/repos/testuser/test-repo/pipelines/$PIPELINE_ID" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool

# 5. 查看 pipeline 日志（如果有 job ID）
# curl -s "$IF_API/repos/testuser/test-repo/pipelines/$PIPELINE_ID/jobs" \
#   -H "Authorization: Bearer $TOKEN" | python3 -m json.tool
```

**✅ 通过标准**: Pipeline 被 push 触发，状态可见

---

## T15 — Release 发布

**验证目标**: Release + 资产上传

```bash
# 1. 创建 Tag
cd /tmp/ironforge-test/git/http-clone
git tag v1.0.0 && git push origin v1.0.0 2>&1

# 2. 创建 Release
RELEASE_RESP=$(curl -s -X POST "$IF_API/repos/testuser/test-repo/releases" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"tag_name":"v1.0.0","title":"First Release","body":"Initial release","draft":false,"prerelease":false}')
echo "$RELEASE_RESP" | python3 -m json.tool
RELEASE_ID=$(echo "$RELEASE_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin).get('id',1))")

# 3. 列出 Releases
curl -s "$IF_API/repos/testuser/test-repo/releases" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool

# 4. 上传 Release 资产
echo "binary content" > /tmp/test-asset.tar.gz
curl -s -X POST "$IF_API/repos/testuser/test-repo/releases/$RELEASE_ID/assets" \
  -H "Authorization: Bearer $TOKEN" \
  -F "file=@/tmp/test-asset.tar.gz" \
  -F "name=test-asset.tar.gz" | python3 -m json.tool
```

**✅ 通过标准**: Release 创建→列出→资产上传 全链路

---

## T16 — 包注册表（Cargo）

**验证目标**: Cargo registry 协议兼容

```bash
# 1. 创建包上传 API 测试
# Cargo registry 的 announce.json 端点
curl -s -o /dev/null -w "%{http_code}" \
  "$IF_URL/cargo/api/v1/crates/new" \
  -H "Authorization: Bearer $TOKEN"
# 预期: 200（或 400 如果缺少 body，但不应 404）

# 2. 查询包
curl -s "$IF_API/repos/testuser/test-repo/packages" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool

# 3. 使用 cargo publish（需要配置 .cargo/config.toml）
mkdir -p /tmp/ironforge-test/cargo-test
cd /tmp/ironforge-test/cargo-test
cat > Cargo.toml << 'EOF'
[package]
name = "test-pkg"
version = "0.1.0"
edition = "2021"

[dependencies]
EOF

mkdir src && echo 'pub fn hello() { println!("hello"); }' > src/lib.rs

# 配置 cargo registry 指向 IronForge
mkdir -p ~/.cargo
cat >> ~/.cargo/config.toml << 'EOF'
[registries.ironforge]
index = "http://localhost:8080/cargo/testuser/test-repo"
token = "placeholder"
EOF

# 尝试 publish（可能需要正确的 token 格式）
# cargo publish --registry ironforge 2>&1 || echo "Publish requires proper token format"

# 4. 验证 API 端点可达
curl -s -o /dev/null -w "%{http_code}" "$IF_URL/cargo/api/v1/crates" 
echo " - crates API"
```

**✅ 通过标准**: API 端点可达，不返回 404

---

## T17 — 通知系统

**验证目标**: 通知生成和获取

```bash
# 1. 获取通知列表
curl -s "$IF_API/notifications" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool
# 预期: 之前操作（Issue/PR/Star）生成的通知

# 2. 标记通知为已读
NOTIF_ID=$(curl -s "$IF_API/notifications" \
  -H "Authorization: Bearer $TOKEN" | python3 -c "import sys,json; d=json.load(sys.stdin); items=d.get('data',d); print(items[0]['id'] if items else '1')" 2>/dev/null || echo "1")

curl -s -X PATCH "$IF_API/notifications/$NOTIF_ID/read" \
  -H "Authorization: Bearer $TOKEN"

# 3. 未读计数
curl -s "$IF_API/notifications/unread/count" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool
```

**✅ 通过标准**: 通知存在，可标记已读

---

## T18 — 搜索（仓库 + 代码）

**验证目标**: 全文搜索功能

```bash
# 1. 仓库搜索
curl -s "$IF_API/search?q=test" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool
# 预期: 找到 test-repo

# 2. 代码搜索（需要先 index-repo）
target/release/ironforge index-repo testuser test-repo \
  --repo-root /tmp/ironforge-test/repos 2>&1

# 3. 搜索代码
curl -s "$IF_API/search?q=hello&type=code" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool
# 预期: 找到 main.rs 中的 hello
```

**✅ 通过标准**: 仓库搜索返回结果，代码索引+搜索可用

---

## T19 — 管理员后台

**验证目标**: 管理功能

```bash
# 先将 testuser 设为 admin（需要直接操作 DB 或通过首次注册自动设为 admin）
# IronForge 的第一个用户通常自动成为 admin

# 1. 列出所有用户
curl -s "$IF_API/admin/users" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool

# 2. 列出组织
curl -s "$IF_API/admin/orgs" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool

# 3. 审计日志
curl -s "$IF_API/admin/audit/logs" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool
# 预期: 返回操作审计记录

# 4. 系统设置
curl -s "$IF_API/admin/settings" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool

# 5. Runner 列表
curl -s "$IF_API/admin/runners" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool
```

**✅ 通过标准**: Admin API 可访问，返回管理数据

---

## T20 — MFA 两步认证

**验证目标**: TOTP 设置和验证

```bash
# 1. 设置 MFA（获取 secret + QR code）
MFA_RESP=$(curl -s -X POST "$IF_API/users/mfa/setup" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json")
echo "$MFA_RESP" | python3 -m json.tool
# 预期: 返回 secret 和 otpauth URL

# 2. 需要 TOTP app 生成验证码（如 Google Authenticator）
# 提取 secret
MFA_SECRET=$(echo "$MFA_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin).get('secret',''))")

# 3. 生成 TOTP 验证码（需要 oathtool）
# brew install oath-toolkit
if command -v oathtool &>/dev/null; then
  TOTP_CODE=$(oathtool --totp -b "$MFA_SECRET")
  
  # 启用 MFA
  curl -s -X POST "$IF_API/users/mfa/enable" \
    -H "Authorization: Bearer $TOKEN" \
    -H "Content-Type: application/json" \
    -d "{\"code\":\"$TOTP_CODE\"}" | python3 -m json.tool
  
  # 获取备份码
  curl -s "$IF_API/users/mfa/backup" \
    -H "Authorization: Bearer $TOKEN" | python3 -m json.tool
else
  echo "oathtool not installed - skipping TOTP verification"
  echo "Install with: brew install oath-toolkit"
fi
```

**✅ 通过标准**: MFA setup 返回 secret，enable 验证成功（需要 oathtool）

---

## T21 — API 文档 (Swagger)

**验证目标**: OpenAPI 文档完整性

```bash
# 1. OpenAPI JSON
curl -s "$IF_URL/api-docs/openapi.json" | python3 -c "
import sys, json
d = json.load(sys.stdin)
paths = d.get('paths', {})
print(f'Title: {d.get(\"info\",{}).get(\"title\",\"?\")}')
print(f'Version: {d.get(\"info\",{}).get(\"version\",\"?\")}')
print(f'Paths: {len(paths)}')
methods = sum(len(v) for v in paths.values())
print(f'Endpoints: {methods}')
# 列出前 10 个路径
for i, (path, ops) in enumerate(sorted(paths.items())):
    if i >= 10: break
    methods_list = list(ops.keys())
    print(f'  {methods_list} {path}')
"

# 2. Swagger UI
curl -s -o /dev/null -w "%{http_code}" "$IF_URL/api-docs/"
# 预期: 200 (HTML)
```

**✅ 通过标准**: OpenAPI JSON 有效，Swagger UI 可访问

---

## T22 — WebSocket 实时通知

**验证目标**: WebSocket 连接和推送

```bash
# 使用 websocat 或 wscat 测试
# brew install websocat

if command -v websocat &>/dev/null; then
  # 连接 WebSocket（30秒超时）
  timeout 15 websocat "ws://localhost:8080/ws/notifications?token=$TOKEN" 2>&1 &
  WS_PID=$!
  
  # 触发一个通知（创建 Issue）
  sleep 1
  curl -s -X POST "$IF_API/repos/testuser/test-repo/issues" \
    -H "Authorization: Bearer $TOKEN" \
    -H "Content-Type: application/json" \
    -d '{"title":"WS test issue","body":"Testing WebSocket"}' > /dev/null
  
  # 等待 WebSocket 接收
  sleep 3
  kill $WS_PID 2>/dev/null
  echo "WebSocket test done"
else
  echo "websocat not installed - install with: brew install websocat"
  echo "Or test via browser console:"
  echo "  const ws = new WebSocket('ws://localhost:8080/ws/notifications?token=$TOKEN');"
  echo "  ws.onmessage = e => console.log('Notification:', e.data);"
fi
```

**✅ 通过标准**: WebSocket 连接成功，事件触发时收到推送

---

## T23 — 组织管理

**验证目标**: 组织创建 + 成员管理

```bash
# 1. 创建组织
curl -s -X POST "$IF_API/orgs" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"test-org","description":"Test organization"}' | python3 -m json.tool

# 2. 获取组织信息
curl -s "$IF_API/orgs/test-org" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool

# 3. 列出组织
curl -s "$IF_API/orgs" \
  -H "Authorization: Bearer $TOKEN" | python3 -m json.tool

# 4. 组织下创建仓库
curl -s -X POST "$IF_API/repos" \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"name":"org-repo","description":"Org repo","owner":"test-org"}' | python3 -m json.tool
```

**✅ 通过标准**: 组织创建→查询→组织仓库 全链路

---

## 前端 UI 验证

> 在浏览器中打开 `http://localhost:8080` 进行以下验证

| # | 页面 | 验证内容 |
|---|------|---------|
| F01 | 登录页 | 使用 testuser/Test1234! 登录 |
| F02 | Dashboard | 显示仓库列表 |
| F03 | 仓库首页 | 显示 README.md 渲染 |
| F04 | 代码浏览 | 导航文件树，查看文件内容 |
| F05 | Issues 列表 | 显示已创建的 Issue |
| F06 | PR 列表 | 显示已创建的 PR |
| F07 | Wiki | 显示 Wiki 页面 |
| F08 | Pipeline | 显示 CI/CD 状态 |
| F09 | 设置-分支保护 | 显示保护规则 |
| F10 | 设置-协作者 | 显示协作者列表 |
| F11 | 管理员后台 | 用户/组织/审计日志页面 |
| F12 | 通知页面 | 显示通知列表 |
| F13 | 语言切换 | 中英文切换正常 |
| F14 | 个人设置-Tokens | PAT 管理页面 |
| F15 | 个人设置-安全 | MFA 设置页面 |

---

## 测试结果汇总模板

```markdown
## 测试结果

| 编号 | 模块 | 状态 | 备注 |
|------|------|------|------|
| T01 | 健康检查 | ✅/❌ | |
| T02 | 用户认证 | ✅/❌ | |
| T03 | PAT 令牌 | ✅/❌ | |
| T04 | 仓库管理 | ✅/❌ | |
| T05 | Git HTTP | ✅/❌ | |
| T06 | Git SSH | ✅/❌ | |
| T07 | 文件浏览 | ✅/❌ | |
| T08 | Issues | ✅/❌ | |
| T09 | PR & Review | ✅/❌ | |
| T11 | Wiki | ✅/❌ | |
| T12 | 分支保护 | ✅/❌ | |
| T13 | 协作者 | ✅/❌ | |
| T14 | CI/CD | ✅/❌ | |
| T15 | Release | ✅/❌ | |
| T16 | 包注册表 | ✅/❌ | |
| T17 | 通知 | ✅/❌ | |
| T18 | 搜索 | ✅/❌ | |
| T19 | 管理员 | ✅/❌ | |
| T20 | MFA | ✅/❌ | |
| T21 | API 文档 | ✅/❌ | |
| T22 | WebSocket | ✅/❌ | |
| T23 | 组织 | ✅/❌ | |

**通过率**: XX/23
**核心路径 (P0)**: T01-T06 全部通过 = 生产可用基线
```

---

## 清理

```bash
# 停止服务器（Ctrl+C 在服务器终端）

# 清理测试数据
rm -rf /tmp/ironforge-test
rm -f ~/.cargo/config.toml  # 如果添加了 ironforge registry 配置
```

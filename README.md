# GitHub Issue Reporter for BroKnowMyQQBot

BKMQB 官方维护的 BPP/WASM 插件，用聊天命令绑定 GitHub 仓库并创建 Issue。

查询当前 QQ 官方 OpenID、OneBot QQ 号和来源会话：

```text
/whoami
```

```text
/aliasRepo CDC https://github.com/Jasons-impart/Create-Delight-Core

/issue CDC 标题内容
正文内容，可以有多行。
```

普通连通性测试使用保留别名 `TEST`，任何用户都可以执行，且不会向 GitHub 创建
真实 Issue。回复包含测试结果、标题、正文、发送者 ID 和来源 ID：

```text
/issue TEST 测试标题
测试正文
```

插件会在 Issue 正文末尾自动附加发送者昵称、sender ID、群聊/私聊/频道 ID、
Adapter ID 和原消息 ID。创建成功后回复 Issue 编号、标题和 GitHub URL。

`/aliasRepo` 仅允许插件配置中的管理员调用。默认也只有管理员可以创建 Issue；将
`allow_all_submitters` 设置为 `true` 后，所有能够向 Bot 发送命令的用户都可以提交。

## Token

Token 只配置在 Bot Host 的 `config/secrets.env`，不得写入本仓库或插件配置：

```dotenv
BKMQB_PLUGIN_HTTP_BEARER_GITHUB_ISSUE=github_pat_xxx
```

建议使用 fine-grained personal access token，只授权目标仓库的 Issues `Read and write`。

安装时必须显式授予插件请求的能力：

```bash
bkmqb plugin install io.github.bkmqb-community.github-issue-0.2.4.bkm-plugin \
  --grant message.reply \
  --grant storage.private \
  --grant http.request \
  --grant http.host.api.github.com:443 \
  --grant http.credential.github_issue \
  --config admins='["你的平台用户ID"]' \
  -y
```

安装或更新后重启 `bkmqb`。

## 开发

```bash
rustup target add wasm32-unknown-unknown
cargo fmt --all -- --check
cargo test
cargo check --target wasm32-unknown-unknown
```

版本标签 `v*` 会通过 GitHub Actions 构建并上传 `.bkm-plugin` 和 SHA-256 文件到
GitHub Release。

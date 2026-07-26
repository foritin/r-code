# agent-core 子模块提交手册

这一轮给 `hermes-config` 的 `ProviderConfig` 加了 `protocol: Option<String>` 字段。
改动在子模块 `vendor/agent-core` 的工作区里，**还没提交**。本机 `cargo build` 能过
（用的是工作区文件），但 CI 会 checkout 到父仓 pin 住的提交，那里没有这个字段，
6 处 `E0560` 全线失败。

推送必须由你来（沙箱没有凭据）。以下命令在 `D:\project\rust\r-code` 下按顺序执行。

## 1. 确认要提交的只有一个文件

子模块工作区里 `.github/workflows/ci.yml`、`Cargo.toml`、`LICENSE`、`README.md` 等
也显示为已修改，但那些全是 CRLF 行尾噪声，不是真实改动。用 `--ignore-cr-at-eol`
可以看清：

```powershell
cd vendor\agent-core
git diff --ignore-cr-at-eol --stat
# 预期只有：crates/hermes-config/src/lib.rs | 16 +++++++++
```

`contract-lock.json` 也有真实改动（见第 3 步），其余不要碰。

## 2. 提交并推送 agent-core

```powershell
cd D:\project\rust\r-code\vendor\agent-core

git add crates/hermes-config/src/lib.rs contract-lock.json

git commit -m "feat(config): ProviderConfig 增加可选的 protocol 字段

线路协议改为由用户在设置页显式选择并持久化，不再由消费者按 provider 名或
base_url 推断。同一个 base_url 常常同时支持 Anthropic Messages / OpenAI Chat /
OpenAI Responses，计费和能力都不同，只能由用户决定。

None = 升级前保存的旧配置，回退策略交给消费者。R-Code 的做法是按目录推断但
绝不自动选中 Responses，避免静默改变用户的账单。

契约版本 v0.1.0 -> v0.2.0（新增可选字段，按 versionPolicy 属 minor）。"

git push origin HEAD
```

> 仓里没有配置 git 身份。如果 commit 报 `Author identity unknown`，先跑：
> `git config user.name "你的名字"` 和 `git config user.email "你的邮箱"`
> （加 `--global` 就是全局配置）。

## 3. 把 contract-lock.json 的 commit 刷成新 HEAD

`publicContract` 已经改成 `v0.2.0` 并加了 changelog 条目。`commit` 字段现在写的是
提交**之前**的 HEAD，刷一下：

```powershell
cd D:\project\rust\r-code\vendor\agent-core
$sha = git rev-parse HEAD
(Get-Content contract-lock.json -Raw) -replace '"commit": "[a-f0-9]{40}"', ('"commit": "' + $sha + '"') | Set-Content contract-lock.json -NoNewline
git commit -am "chore: contract-lock 指向 v0.2.0 的提交"
git push origin HEAD
```

这一步不做也不会让 CI 变红——新的 pin 检查不看这个字段（见下）。它只是文档，
记录"合同最后一次被验证时的 agent-core 提交"。

## 4. 更新父仓的 submodule 指针

**这步不能漏。** 少了它，CI 仍然会 checkout 到旧提交。

```powershell
cd D:\project\rust\r-code
git add vendor/agent-core
git status --short vendor/agent-core   # 应显示 M vendor/agent-core
git commit -m "chore(vendor): bump agent-core to v0.2.0 契约"
```

## 5. submodule-pin 检查已改

`.github/workflows/ci.yml` 里那个 job 原来是死循环：`contract-lock.json` 存在子模块
内部，却要求它的 `commit` 字段等于自己所在提交的 SHA——写 lock 会产生新提交，新
提交的 SHA 又不等于 lock 里的值，永远不可能相等。所以它长期是红的
（lock 写 `b6ac5d43`，实际 HEAD 是 `0a8a5ef2`）。

现在改成比对**父仓记录的 gitlink**与**实际 checkout 出来的 HEAD**：

```bash
EXPECTED=$(git ls-tree HEAD vendor/agent-core | awk '{print $3}')
ACTUAL=$(git -C vendor/agent-core rev-parse HEAD)
```

这才是 pin 检查真正该验的东西（子模块有没有被 checkout 到父仓指针以外的位置），
而且总是可满足。漏了第 4 步就会被它抓住。

## 顺带一提：CRLF 噪声

父仓的 `tauri.conf.json`、`Cargo.toml`、`index.html`、`README.md`、`deny.toml` 在工作
区是 CRLF 而 HEAD 是 LF，导致这些文件整文件显示为改动（`Cargo.toml` 109 行全变，而
根本没人动过它）。这会把真实 diff 淹没。要治的话：

```powershell
cd D:\project\rust\r-code
Set-Content .gitattributes "* text=auto eol=lf`n"
git add --renormalize .
git commit -m "chore: 统一行尾为 LF"
```

子模块里有同样的问题，同样处理。

# RV-04 安全审查证据（2026-08-29）

说明：命令在 `D:\project\rust\r-code` 下执行；`rg` 输出经 rtk 代理压缩，关键结论用 `rtk proxy`（原始输出）复核，二者分别标注。行号为当时工作树状态。

## E1 IPC 面清点

```
$ rg -c '#\[tauri::command\]' src-tauri/src crates
src-tauri/src\tauri_commands.rs:190
src-tauri/src\lifecycle_commands.rs:4
src-tauri/src\main.rs:1
src-tauri/src\commands.rs:1
（合计 196 个 tauri command；commands.rs 为库函数层，#[tauri::command] 包装集中在 tauri_commands.rs）

$ rg -n 'pub (async )?fn ' src-tauri/src/tauri_commands.rs | wc -l
196（全部命令签名逐条清点，路径/字符串参数命令重点追踪）
```

## E2 路径穿越

```
$ rg -n --no-heading 'struct PathGuard|impl PathGuard|fn open_file|fn list_directory|fn attached_workspace_root' crates src-tauri/src --type rust
src-tauri/src\commands.rs:1857:fn attached_workspace_root(state: &CommandState, workspace_path: &str) -> Result<PathBuf, String> {
crates\r-code-core\src\security.rs:71:pub struct PathGuard {
crates\r-code-core\src\security.rs:257:impl PathGuard {
crates\r-code-core\src\security.rs:421:    pub fn open_file(
crates\r-code-core\src\security.rs:476:    pub fn list_directory(

# workspace 必须已注册（commands.rs:1828-1840）：
$ rg -n 'fn workspace_root' -A 12 src-tauri/src/commands.rs
1828:fn workspace_root(state: &CommandState, workspace_path: &str) -> Result<PathBuf, String> {
1829-    let workspace = WorkspaceService::new(&state.db)
1830-        .get(workspace_path)          # DB 注册校验
1833-    let root = PathBuf::from(&workspace.canonical_path).canonicalize() ...

# 直连 std::fs 写/删点核对（非测试）：
$ rg -n --no-heading 'std::fs::(write|create_dir|copy|remove|rename|read_to_string|read)|fs::(write|create_dir|copy|remove|rename)' src-tauri/src/commands.rs | grep -v '^.*test'
1048/1060（sessions_dir 读） 1340-1342（appdata 目录） 5307（audit_dir） 6791-6793（附件：is_safe_path_segment(task_id)+valid_attachment_preview_id 校验）
12235/12508/15026/15694/17327/17517/17536/17752-17753（codex_home/config 定值路径） 18058-18079（codex host staging）
18433/18438/18488（macOS 登录临时脚本，uuid 文件名+0700） 20423（附件 blob，appdata） 26287+（测试）

# zip/tar：
$ rg -n --no-heading 'use (zip|tar|flate2)|zip::|tar::|\.unpack\(' src-tauri/src crates --type rust
src-tauri/src\rtk.rs:779:    let mut zip = zip::ZipArchive::new(reader)
src-tauri/src\rtk.rs:799:    let mut tar = tar::Archive::new(decoder)
src-tauri/src\rtk.rs:1157/1160（测试）
（仅 RTK 安装链；extract_rtk_binary 只按 entry 名匹配 rtk(.exe) 并把字节读入内存，不解压到文件系统路径 → 无 zip-slip）
```

## E3 命令注入面

```
$ rg -n --no-heading 'Command::new' src-tauri/src crates --type rust | grep -v 'test\|mock\|fixture'
（33 处，分类：codex_mcp.rs:416/425/435、codex_app_server.rs:782/790/800/872/930/1129、
 commands.rs:17030/17037/17043/17069/17077（npm）、18254/18267（codex mcp add）、
 18468（codex login，windows）、18482（macOS open Terminal）、18497、20531/20545/20557/20566、
 20745（taskkill）、22198/22205、29109/29221/29315/29445/33452/33473/33481（git，args 数组）、
 system_integration.rs:75（explorer.exe）、91（open -R）、legacy_memory.rs:54/90（git））

$ rg -n 'Command::new\("cmd.exe"\)|TokioCommand::new\("cmd.exe"\)' src-tauri/src --type rust
codex_mcp.rs:425 / codex_app_server.rs:790 / commands.rs:17030 / 17069 / 18267 / 18468 / 22205（生产）
commands.rs:37563 / 37576 / 37604（测试）

# cmd.exe 调用模式（commands.rs:17030-17035 为代表）：
.args(["/D", "/S", "/C", "call"]).arg(cli_path).args(args)   # args 为模块内字面量
# 路径守卫（codex_app_server.rs:845-858 / commands.rs:18303-18315）：
matches!(character, '\0'|'\r'|'\n'|'"'|'&'|'|'|'<'|'>'|'^'|'%'|'!') → 拒绝
$ rg -c "windows_cmd_safe_path" src-tauri/src/commands.rs src-tauri/src/codex_app_server.rs src-tauri/src/codex_mcp.rs
codex_app_server.rs:2  commands.rs:11

# MCP Windows 启动器拒绝（crates/r-code-mcp/src/client.rs:281-311）：
reject_unsafe_windows_launcher: 扩展名 bat|cmd|ps1 或文件名 cmd/cmd.exe/powershell(.exe)/pwsh(.exe)/wscript/cscript/npx/npm → Err(UnsafeWindowsLauncher)
# 进程构造：tokio::process::Command::new(command); process.args(args);（args 数组，无 shell 拼接）

# macOS 登录脚本转义（commands.rs:18384-18392）：
posix_shell_quote: 拒绝 \0 \r \n；单引号包裹 + '\''→"'\"'\"' 替换
```

## E4 凭据安全

```
$ rg -n --no-heading 'zeroize|Zeroize' src-tauri/src crates Cargo.toml src-tauri/Cargo.toml
Cargo.toml:83:zeroize = "1"
crates/r-code-core/Cargo.toml:54:zeroize = { workspace = true }
crates/r-code-core/src/secret.rs:38:    zeroize::Zeroizing,   # macOS AEAD 解密缓冲使用

# keyring：
$ rg -n --no-heading 'keyring|Keyring' src-tauri/src crates --type rust | grep -v test
crates\r-code-core\src\secret.rs:16/86/95/106（Entry::new/set_password/get_password/delete_credential + 写后回读验证）

# TOML 剥离（settings.rs:626-644 save_global）：
for provider in sanitized.providers.values_mut() { provider.api_key.clear(); }
self.write_global(&sanitized)
# settings_set 拦截（commands.rs:26077-26088）：
parts.len()==3 && parts[0]=="providers" && parts[2]=="api_key" → set_provider_secret(keyring) + TOML 写空串
# settings_get 返回前清空（commands.rs:15175）：provider.api_key.clear();

# 日志脱敏：
$ rg -c 'redact_text' src-tauri/src crates --type rust
log_buffer.rs:3  support_bundle.rs:7  commands.rs:12  codex_interaction.rs:2  memory_store.rs:2  secret.rs:30
log_buffer.rs:89（落盘前）+ 198（回读二次脱敏）+ 270-299（字段名白名单 17+ 项）
support_bundle.rs:169（导出再脱敏）+ 66-76（MCP 摘要白名单 DTO，无命令/参数/URL/头）

# F-sec-01 证据（console 层无脱敏）：
logging.rs:66-72: let console_layer = fmt::layer().json()...;
logging.rs:74-78: tracing_subscriber::registry().with(env_filter).with(console_layer).with(crate::log_buffer::BufferLayer::new(writer))
（脱敏逻辑 MessageVisitor/redact_text 全部位于 log_buffer.rs 的 BufferLayer 内）
```

## E5 SQL 注入

```
$ rg -n --no-heading 'format!\(' src-tauri/src crates --type rust | rg -i 'select |insert |update |delete |create table|where'
src-tauri/src\support_bundle.rs:234:  format!("SELECT COUNT(*) FROM {table}")            # table ∈ 本地白名单 "tasks"/"agent_runs"/"tool_calls"
src-tauri/src\migration.rs:624/686:     本地表名常量
crates\r-code-store\src\repositories.rs:699: format!("UPDATE tasks SET {} WHERE id = ?", sets.join(", "))  # sets 全部为硬编码列名字面量
crates\r-code-store\src\plan_review.rs:1646/1995: format!("SELECT {EVENT_COLUMNS} ...")  # EVENT_COLUMNS 为 const

$ rg -n 'query\(&format|execute\(&format|prepare\(&format|query_row\(&format' src-tauri/src crates --type rust
（生产代码无用户输入进入上述动态 SQL；plan_schema.rs 命中为测试）
```

## E6 前端 XSS

```
$ rg -n --no-heading 'dangerouslySetInnerHTML' src-tauri/frontend/src
src-tauri/frontend/src\components\room\Markdown.tsx:5:  # 仅注释声明"没有 dangerouslySetInnerHTML"（实际用点 0）
$ rg -n '\.innerHTML|insertAdjacentHTML|document.write' src-tauri/frontend/src   # 0 命中
$ rg -n '\beval\(|new Function\(' src-tauri/frontend/src                          # 0 命中

# scheme 白名单（frontend/src/lib/markdown.ts:546/568-572）：
const SAFE_SCHEME = /^(?:https?|mailto|file):/i;
# 去控制字符后判 scheme（挡 "java\nscript:"）；被拒 scheme 降级为链接文字（:886）
# 外链（Markdown.tsx:362-372/388-399）：target="_blank" rel="noopener noreferrer"；远程图片渲染为惰性链接不加载
# localStorage 仅存 UI 偏好（SEND_MODE/UNREAD/POSITION/TERMINAL_SIDEBAR），无凭据
```

## E7 IPC/Tauri 配置

```
tauri.conf.json:32: "csp": "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self' ipc: http://ipc.localhost"
tauri.conf.json:33: "devCsp": null
tauri.conf.json:77-84: updater endpoints=https://github.com/foritin/r-code/... + minisign pubkey
capabilities/default.json: core:default + 8 个窗口操作 + opener:default + updater:default（无 fs/shell 权限）
capabilities/companion.json: 仅事件/窗口只读类权限
main.rs:324-334: 自定义 navigation-guard 插件挂载 on_navigation → should_block_navigation（javascript/file/vbscript/data 拒绝）
security_config.rs:37: 生产 blocked_schemes 含 javascript:/file:/vbscript:
```

## E8 URL/SSRF

```
# web.rs（模型侧 fetch，crates/r-code-mcp/src/web.rs）：
:476-487  scheme ∈ {http,https}；拒绝内嵌凭据；拒绝 localhost/.localhost/.local
:489-504  DNS 解析全部地址过 is_blocked_ip
:638-660  私网/环回/链路本地/组播/0.0.0.0/8/≥224/100.64-127（CGNAT）/192.0.0.x/192.0.2/198.51.100/203.0.113
:435-444  resolved addresses 作为 approved_addresses 传入 HTTP 层（DNS rebinding 防护）
:397-415  每跳重定向重新 validate_and_resolve；:534-547 跨域重定向剥离 authorization/proxy-authorization/cookie/x-subscription-token
:461-468  Jina Reader fallback 不转发任何目标站凭据

# provider baseURL（用户自配，属产品功能）：
provider_models.rs:242-248  scheme 白名单 http/https + 拒绝 URL 内嵌用户名密码
provider_models.rs:48-53  DeepSeek 余额查询硬编码 host == api.deepseek.com（防自定义网关收 key）

# F-sec-03 证据链：
settings.rs:592-603  合并 {workspace}/.r-code/config.toml（merge_toml）
settings.rs:779-788  hydrate_secrets 按 provider 名从凭据库回填 api_key
$ rtk proxy grep -rn "load_with_workspace" src-tauri/src crates --include="*.rs"
→ 仅 settings.rs:577（定义）与 settings.rs:1491/1513/1563（测试）→ 生产无调用方
```

## E9 权限门与安装链

```
codex_permissions.rs:282-315  from_config：未知字符串 → Custom + sandbox=read-only + approval=never（fail-closed）
codex_permissions.rs:103-109  config_override() 仅返回固定 TOML 字面量
commands.rs:17690-17702        Codex config.toml 用 toml_edit 结构化编辑（无拼接）
r-code-gateway/src/permission.rs  R0/R1 自动放行、R2/R3 standing rule/待审批、R4 前置拒绝；R3 不可持久化
mcp_manager.rs:948-991         mcp_toggle 需 confirmation_token；新启动形态默认 disabled
rtk.rs:298-383  下载 sha256 pin（313-317）→ staging 激活 → 版本探针 → 失败回滚
control_door.rs  Unix-only，socket 0600 + 每次启动随机 token（Windows 不编译）
```

## E10 F-sec-02 / F-sec-04 / F-sec-08 直接证据

```
tauri_commands.rs:1941-1943:
pub async fn cmd_reveal_local_path(path: String) -> Result<(), String> {
    r_code_host::system_integration::reveal_in_file_manager(std::path::Path::new(&path))
}
system_integration.rs:65-71: 仅 path.exists() 检查后交 explorer.exe / open / xdg-open

commands.rs:14329-14341（F-sec-04）:
let canonical = candidate.canonicalize()...;
let metadata = std::fs::metadata(&canonical)...;
scope = if relative_path.is_some() { Workspace } else { External }
（External 返回 absolute_path/size_bytes/mime_type；内容读取另被 14396-14418 限制在 generated_images）

installer-hooks.nsh:5-13（F-sec-08）:
${GetOptions} $CMDLINE "/BRANDED_PROGRESS=" $R8 → FileOpen $R9 "$R8" w → FileWrite 固定字符串
```

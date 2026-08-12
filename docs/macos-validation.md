# macOS 真机验证清单

本文只记录无法在 Windows 开发机或静态代码审查中闭环的 macOS 运行验证。签名、公证
和最低系统版本不属于本清单范围。

## 已在代码层完成

- Apple 目标启用 `keyring/apple-native`，Provider 和 MCP 凭据使用 macOS Keychain。
- `SecretStore::store` 会用新的 Keychain `Entry` 回读确认；回读失败时保存失败，旧明文
  Provider 密钥不会被迁移逻辑清空。
- macOS Bundle ID 保持 `com.rcode.desktop`；启动期日志、无参数 MCP server、托管 RTK
  和 Tauri 桌面进程统一使用其数据目录：
  `~/Library/Application Support/com.rcode.desktop/r-code`。
- Finder/Dock 启动会导入登录 Shell PATH；终端默认使用 zsh，并保留 bash/sh 回退。
- Apple Silicon 与 Intel 均有 Release 构建配置。

## 真机必须验证

在一台日常可用的 Mac 上安装当前构建，建议用一份临时工作区完成以下 smoke test。

### 1. Provider Keychain

1. 在设置页保存一个可撤销的测试 Provider Key。
2. 不重启应用，立即发起一次模型请求。
3. 完全退出并重新打开 R-Code，再发起一次请求。
4. 更新 Key 后重复请求，随后删除 Key，确认请求明确提示缺少凭据。
5. 在“钥匙串访问”中确认只存在预期的 R-Code 项，值不会写入
   `config/config.toml` 或诊断日志。

验收：保存、跨调用读取、跨重启读取、更新和删除均正常；失败不会清空唯一凭据副本。

### 2. MCP 凭据

1. 添加一个需要环境变量或 Header 凭据的测试 MCP 服务。
2. 保存后测试连接并调用一个只读工具。
3. 重启 R-Code，再次测试连接和调用。
4. 删除凭据，确认服务显示未配置且不会携带旧值启动。

验收：凭据跨重启可用，删除后立即失效，配置文件中只保存凭据引用。

### 3. 数据目录一致性

1. 启用托管 RTK，确认文件位于应用数据目录的 `bin/`。
2. 从 R-Code 启动 Codex 子进程，确认执行 `rtk --version` 能找到同一二进制。
3. 产生一条诊断日志并导出支持包，确认支持包包含该日志。
4. 使用不带 `--data-dir` 的 `r-code-host mcp-server`，确认读取桌面应用已有的工作区和数据库，
   没有在 `com.r-code.app` 下创建第二套 macOS 数据。

验收：RTK、日志、桌面状态和独立 MCP 全部落在
`~/Library/Application Support/com.rcode.desktop/r-code`。

### 4. Finder、Shell 与进程生命周期

1. 从 Finder 或 Dock 启动 R-Code，而不是从终端启动。
2. 确认 Homebrew、npm/nvm 安装的 Codex 和 Node 能被探测。
3. 打开内置终端，确认 zsh 可交互、当前目录正确、中文输入输出正常。
4. 运行会产生子进程的长命令后取消，确认父进程和子进程均退出。
5. 执行“在 Finder 中显示”，确认目标文件被选中。

验收：GUI 启动与终端启动的工具发现一致，PTY 和进程回收无残留。

### 5. 安装包基本运行

1. 按 CPU 架构选择 Apple Silicon 或 Intel 包，拖入 Applications 后启动。
2. 创建/打开工作区，完成一轮对话、工具读取和终端命令。
3. 退出并重启，确认任务、设置和工作区状态仍存在。

验收：常用桌面主链路无崩溃或平台专属错误。

## 结果记录模板

```text
日期：
macOS 版本：
CPU：Apple Silicon / Intel
R-Code commit 或版本：
安装方式：DMG / .app / cargo tauri dev

Provider Keychain：通过 / 失败（说明）
MCP 凭据：通过 / 失败（说明）
托管 RTK：通过 / 失败（说明）
日志与支持包：通过 / 失败（说明）
无参数 MCP server：通过 / 失败（说明）
Finder PATH：通过 / 失败（说明）
zsh / PTY / 取消：通过 / 失败（说明）
安装与重启持久化：通过 / 失败（说明）
```

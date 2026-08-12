# R-Code 安装、备份与恢复手册

本文面向安装、升级、备份、恢复或卸载已发布 R-Code 桌面应用的用户与运维人员，说明当前 `0.x` 的真实数据布局。发布维护者的版本、tag、签名与 CI 操作请看[发布手册](./RELEASING.md)。

## 先遵守这些安全边界

- 只从项目的 [GitHub Releases](https://github.com/foritin/r-code/releases) 下载安装包。安装前核对操作系统、CPU 架构，以及 Release 正文声明的签名状态。
- 复制、移动或替换本地数据前，退出 R-Code，并关闭任何正在使用 R-Code 受管 MCP server 的 Codex 客户端。应用运行时不要只复制 `r-code.db`：SQLite 可能仍有活动的 `-wal`、`-shm` 旁车文件。
- 完整的应用数据备份可能含对话、工作区引用、诊断输出和非敏感配置，应加密保存，并采用与所用工作区相同的访问控制。
- Provider 与 MCP 凭据位于操作系统凭据库，不在本文的数据目录中。文件备份不会导出这些凭据；在另一台机器恢复后，可能需要重新输入凭据。

## 安装

| 平台 | 选择的安装包 | 说明 |
| --- | --- | --- |
| Windows x64 | 品牌 `.exe`、NSIS `.exe` 或 WiX `.msi` | `.msi` 适合受管软件分发。升级前先关闭旧版 R-Code。 |
| macOS Apple Silicon / Intel | 与芯片架构匹配的 `.dmg` | M 系列 Mac 选 Apple Silicon，Intel Mac 选 Intel。出现 Gatekeeper 警告时不要用绕过命令，先确认 Release 中公开的签名/公证状态。 |
| Linux x86_64 | `.deb` 或 `.AppImage` | 用发行版的软件包工具安装 `.deb`，或先执行 `chmod +x` 后运行 AppImage。 |

普通升级时，直接安装新版本覆盖现有应用，不要先删除应用数据目录。当前受支持的升级路径是安装 Release 页面发布的新包；updater manifest 与签名是 Release 资产，不代替操作系统对安装包和签名的校验。

如果系统提示安装包未签名或不可信，请停止安装，并将提示与 Release 明示的签名状态核对。不要为了完成安装而关闭系统安全机制。Windows/macOS 的平台签名凭据属于发布运维要求；缺失时应在 Release 中显式标注，而不是对用户隐藏。

## 应用数据位置

正常桌面应用的数据根目录按平台 Bundle ID 派生：

| 平台 | 默认路径 |
| --- | --- |
| Windows | `%APPDATA%\\com.r-code.app\\r-code` |
| macOS | `~/Library/Application Support/com.rcode.desktop/r-code` |
| Linux | `${XDG_DATA_HOME:-~/.local/share}/com.r-code.app/r-code` |

目录通常包含：

```text
r-code/
├─ db/r-code.db     # SQLite 产品状态
├─ blobs/           # 内容寻址的基线和大输出
├─ sessions/        # JSONL 对话和 Agent 事件
├─ config/          # 非敏感设置与凭据引用
├─ logs/            # 脱敏诊断 JSONL，固定保留七天
├─ plans/           # 使用 Plan 模式时生成的 Markdown 投影
└─ mcp-host/        # 启用 Codex 集成后部署的本地 MCP host
```

独立运行的 `mcp-server` 可以通过 `--data-dir` 或 `R_CODE_DATA_DIR` 指定整个数据根目录。该设置只影响 MCP 进程，不会迁移已安装桌面应用的数据；不要把它指到源代码工作区或不完整的 `db` 子目录。

## 升级、重装或恢复前的备份

应备份整个 profile，而不是只复制数据库：

1. 退出 R-Code，并关闭可能让 R-Code MCP host 持续运行的 Codex 或其他客户端。
2. 确认没有 R-Code 进程仍在使用数据目录。
3. 将完整的 `r-code` 目录复制到加密备份位置，保留目录结构，并记录 R-Code 版本与备份时间。
4. 直到新版本确认能打开预期任务、对话、设置和工作区前，不要删除备份。

应用本身也会保护 schema 升级。已有数据库需要迁移时，桌面应用和独立 MCP 启动入口都会先做完整性校验，并在 `db/` 中创建类似 `r-code-pre-migration-<timestamp>-<uuid>.db` 的可校验、WAL 安全 SQLite 快照；随后才执行迁移和第二次完整性校验。任一步失败时，R-Code 会恢复该快照并中止启动，不会继续打开部分迁移的数据。全新 profile 没有旧数据，因此不会生成升级前快照。

请把已验证的快照保留到该版本验收完成。不要因为安装器清理就删除旧快照，除非这符合你自己的保留策略。

## 恢复

### 恢复完整 profile

1. 停止 R-Code 和所有受管 MCP host 进程。
2. 即使当前目录看起来已损坏，也先再复制一份，再做替换。
3. 使用完整 profile 备份替换数据根目录，`db`、`blobs`、`sessions` 与 `config` 必须来自同一备份点。
4. 启动 R-Code，确认任务列表、至少一段代表性对话和 Provider 设置正确，再删除失败目录的副本。

不要把一个备份点的数据库和另一个备份点的 `blobs` 或 `sessions` 混用，除非正在有意识地进行事故恢复；这些存储之间存在引用关系。

### schema 升级失败时

先阅读启动错误。正常的迁移失败已经尝试恢复可校验的升级前快照，并会主动停止启动。保留错误文本及其中指明的快照路径；如果再次启动仍失败，优先恢复最近的完整 profile 备份。只恢复数据库属于最后手段：保留当前 profile 副本，确认所有 R-Code/MCP 进程均已停止，用已验证快照替换 `db/r-code.db`，并在启动前移除陈旧的 `r-code.db-wal` 与 `r-code.db-shm`。任何进程运行时都不要执行这些替换步骤。

若自动和手动恢复都失败，尽可能收集脱敏日志或支持包，并通过[支持渠道](../SUPPORT.md)提供版本、系统、安装来源和完整报错。不要把数据库本身附到公开 issue。

## 卸载与数据保留

卸载应用和删除用户数据是两件独立的事：

- Windows NSIS 卸载器提供删除应用数据的选项。普通重装或需要保留历史时不要勾选。勾选后，它会先尝试停止 R-Code 自己的 MCP host，再清理产品的 Roaming/Local AppData 根目录；文件被占用时，删除可能被安排到下次重启。
- macOS 把应用移到废纸篓只会移除应用包，未必移除 profile；Linux 卸载包同样不保证删除用户数据目录。
- 需要彻底清除本地历史时，先按组织策略创建并验证备份，然后再删除上面的 profile 路径。删除后，本地对话和任务历史不可恢复。
- 删除应用数据不保证删除操作系统凭据库条目。只有在确认条目确实属于 R-Code 后，才在 Credential Manager、Keychain 或 secret-service UI 中单独处理；不要靠猜测名称删除无关凭据。

## 诊断和支持包

在 **设置 → 诊断** 查看本地日志尾部。应用固定保留最近七个自然日的诊断日志。先选择 **生成预览**，它不会写出文件；检查内容后，再选择 **选择目录并导出**。

支持包只在本地生成，不会自动上传。它包含应用版本、平台、本地统计、受限 MCP 摘要及最近的脱敏 warning/error。脱敏只能降低风险，不能保证完全清除敏感信息，所以发送前必须打开生成的 JSON 再检查一遍。不要在公开 issue 中附上原始 profile、Provider key、MCP 凭据、私有源码或真实对话；安全问题必须按 [SECURITY.md](../SECURITY.md) 私密报告。

## 运维验收清单

每次安装或升级后，至少确认：应用可以启动；已知任务和对话仍存在；可以配置 Provider 而不暴露密钥；支持包预览能成功生成。正式推广前，应在每个实际分发平台的干净机器或虚拟机上完成安装、升级、保留数据卸载和恢复测试。

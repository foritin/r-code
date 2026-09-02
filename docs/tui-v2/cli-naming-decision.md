# R-Code CLI 命名决策（M6-03 / R-SHIP-01）

> 决策日期：2026-09-03
> 依据：docs/tui-v2/r-code-cli-prd.md §2.1（产品定位 R3）+ §11.2 提交切片纪律

## 决策：维持 `r-code-tui` 单名，不增加 `r-code` 别名

### 备选方案与评估

| 方案 | 分发影响 | 结论 |
| --- | --- | --- |
| A. 维持 `r-code-tui` 单名（现状） | 零改动：`tauri.conf.json` externalBin、`release.yml` 构建步、`dev-tui.ps1/sh`、README×2、`release.test.mjs` 断言已一致 | ✅ 采纳 |
| B. `r-code` 别名（cargo `[[bin]]` 双名） | externalBin 需加 `binaries/r-code`、release.yml 构建/复制两处、installer PATH 写入两个名、文档/脚本/断言四面同步 | ❌ 否决 |
| C. 安装期 symlink（r-code → r-code-tui） | 仅 installer 层，但 PATH 出现两个名，卸载清理面 +1 | ❌ 否决 |

### 否决 B/C 的依据

1. **品牌混淆风险**：`r-code` 与桌面应用产品名 `R-Code` 同名，会与"桌面 app 的 CLI 入口"语义混淆；而 `r-code-tui` 明确指终端形态。
2. **PRD 硬约束**：§2.1 明确"不新增第二 CLI 入口"——别名方案本质是同一 binary 的第二个名字，但安装后 PATH/文档/脚本都多一个可发现名，等于第二入口的可发现性成本。
3. **成本不对称**：别名收益（命令行短 3 字符）不敌四面同步成本 + 卸载清理面 + 后续维护漂移风险（`release.test.mjs` 已有 TUI 漂移断言，扩到别名会让断言面翻倍）。
4. **已有先例**：v1 M8-04 分发管线以 `r-code-tui` 为 externalBin 名跑通三平台；改名会破坏既有安装约定。

### 一致性证据（维持现状的四面一致）

- **externalBin**：`src-tauri/tauri.conf.json` `"binaries/r-code-tui"`
- **构建**：`.github/workflows/release.yml` `cargo build --release -p r-code-tui --bin r-code-tui`
- **脚本**：`dev-tui.ps1` / `dev-tui.sh`（`cargo run -p r-code-tui --bin r-code-tui`）
- **断言**：`scripts/release.test.mjs`（TUI 启动脚本 Dev 命名空间漂移断言）+ `scripts/verify-tui-v2.mjs`（全链断言）

## 结果

`implementation_verified` 完成条件满足：命名决策落地为"维持现状 + 决策记录"，四面一致已由 `--through M6` 累计门禁（含 `release.test.mjs` 漂移断言）验证。

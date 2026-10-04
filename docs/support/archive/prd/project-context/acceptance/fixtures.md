# 验收 fixture 定义（附录 A.1）

fixture 一律是**固定 commit 的快照克隆**，落 `sandbox/project-context-acceptance/fixtures/`（不入库）。
`prepare-fixtures.mjs` 负责克隆、检出固定版本、校验指令文件存在性，并把解析出的 commit 写进 `fixtures.json` 清单。

## A 组 · r-code 本仓库（有完整指令体系）

| 项 | 值 |
| --- | --- |
| 来源 | 本地工作区克隆（`--source` 可指其他 r-code 检出） |
| 固定 commit | `2b894c21feddee5efba079ae38009b9b38a5ed02`（main，2026-09-30 基线） |
| 克隆方式 | `git clone --no-hardlinks --recurse-submodules <source> fixtures/a-r-code` 后 `git checkout <commit>` |
| 校验 | 根存在 `AGENTS.md`（A 组前提：外来指令文件在场） |

注意：A 组**不是**开发工作区本身——基线任务会写文件、跑命令，必须用快照克隆隔离。

## B 组 · jq（无任何指令文件的中型仓库）

| 项 | 值 |
| --- | --- |
| 来源 | `https://github.com/jqlang/jq.git` |
| 固定 tag | `jq-1.7.1`（首次 prepare 后把解析 commit 固化进 `fixtures.json`，之后按 commit 校验） |
| 克隆方式 | `git clone --branch jq-1.7.1` |
| 校验 | 根与全树**不存在** `AGENTS.md` / `CLAUDE.md` / `GEMINI.md` / `.cursorrules` / `.windsurfrules`；C 代码行数落在约 5–10 万区间（含 src/ 与内置测试） |

选型说明：jq 是单仓 C 项目（约 6 万行），生态内无 AGENTS.md/CLAUDE.md 惯例，规模落在 PRD 区间；
若未来校验发现其新增了指令文件，换库重钉 commit 并在 `fixtures.json` 与 verdict 中记录变更。

## C 组（仅 M3，>40 万行）

M3 开工时再定仓库与 commit，此处仅占位（PRD 附录 A.1）。

## fixtures.json（prepare 产物，机器可读）

```json
{
  "a": { "repo": "<source>", "commit": "2b894c2…", "verifiedAt": "…" },
  "b": { "repo": "https://github.com/jqlang/jq.git", "tag": "jq-1.7.1", "commit": "<resolved>", "verifiedAt": "…" }
}
```

`fixtures.json` 落 `sandbox/project-context-acceptance/fixtures/fixtures.json`（含绝对路径，不入库）；
其 commit 锚点回填到本文件的"固定 commit"格中作为入库记录。

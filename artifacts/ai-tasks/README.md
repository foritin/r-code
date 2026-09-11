# AI 长任务执行产物

远程控制（docs/prd/remote-control/worklist.md）的连续执行状态：

- `current.yaml` —— 当前任务包（唯一单项恢复状态），从 skill 模板生成
- `evidence/<task-id>.yaml` —— 任务通过后的证据归档
- `verification/<profile>/*.json` —— verify-remote.mjs 机器可读报告

本目录是运行产物，除本 README 与首个 current.yaml 模板外不提交报告。

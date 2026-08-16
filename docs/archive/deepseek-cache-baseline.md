# DeepSeek 前缀缓存真实 API 基线验证

> 归档状态：验证已完成，本文件只保留历史测量证据，不是当前待办。

> 验证时间：2026-08-07（分支 `feat/deepseek-prefix-cache`，P0-A + P0-B vendor 层已实施后）
> 方式：非流式 `/chat/completions`，模型 `deepseek-chat`，相同 system+user 前缀连续两轮，追加 assistant+新 user 消息
> 对应 PRD：`docs/archive/deepseek-prefix-cache.md` §6 发布门槛②

## 结果

| 轮次 | prompt_tokens | prompt_cache_hit_tokens | prompt_cache_miss_tokens | 说明 |
| --- | --- | --- | --- | --- |
| 请求 1 | 155 | 0 | 155 | 冷启动，全 miss |
| 请求 2 | 318 | **256** | 62 | 前缀（system+user1）完全命中；62 miss = 新增的 assistant 回复 + user2 |

**第二轮命中率：256/318 = 80.5%**（命中部分为固定前缀，随轮次增长命中比例趋近 90%+，与 Reasonix 宣称一致）

## 结论

1. DeepSeek 字节级自动前缀缓存**真实存在且工作**——无需任何 API 开关，客户端保持前缀字节稳定即可命中。
2. P0-B 解析的字段名 `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens` 与真实 API 返回一致（响应中同时含 OpenAI 风格 `prompt_tokens_details.cached_tokens`，可两套都解析）。
3. 前缀需明显超过缓存块粒度（~64 tokens）才可观测命中；短对话前缀（<64 tokens）不命中属正常。
4. 命中 token 计费为 miss 的 ~1/5（DeepSeek 定价），长会话成本收益显著。

## 备注

- 基线①（P0-A 前 ≈0% 对照）未采集：P0-A 已实施，历史对照由上述"请求 1 冷启动全 miss"数据替代。
- 正式探针测试（`#[ignore]`，走 agent-llm 流式链路 + usage 解析）待补：见 PRD §6 真实 API 层级。

## 探针复测（2026-08-07，`deepseek_cache_probe.rs`，真实 API 14 轮）

| 轮次 | prompt | hit | miss | 命中率 | | 轮次 | prompt | hit | miss | 命中率 |
| --- | ---: | ---: | ---: | ---: | --- | --- | ---: | ---: | ---: | ---: |
| 0 | 169 | 128 | 41 | 75.7% | | 7 | 1542 | 1280 | 262 | 83.0% |
| 1 | 357 | 128 | 229 | 35.9% | | 8 | 1746 | 1536 | 210 | 88.0% |
| 2 | 533 | 256 | 277 | 48.0% | | 9 | 1955 | 1664 | 291 | 85.1% |
| 3 | 712 | 512 | 200 | 71.9% | | 10 | 2159 | 2048 | 111 | 94.9% |
| 4 | 918 | 640 | 278 | 69.7% | | 11 | 2361 | 2048 | 313 | 86.7% |
| 5 | 1131 | 1024 | 107 | 90.5% | | 12 | 2550 | 2432 | 118 | 95.4% |
| 6 | 1339 | 1280 | 59 | 95.6% | | 13 | 2771 | 2688 | 83 | 97.0% |

**tail_avg(3) = 93.0% — PRD §6 发布门槛②（≥85%）达成，且超过 90% 守卫阈值。**

观测：

1. 命中按 ~128 token 块量化（128/256/512/…/2688），与缓存块粒度行为一致；不足一块的尾部必然 miss。
2. round 0 即命中 128：同一 system+首条 user 前缀在探针多次运行间仍存活于服务端缓存（冷启动全 miss 仅首次出现）。
3. 早期轮命中率低是结构性稀释：每轮追加的 assistant 回复（max_tokens=256）占短 prompt 比例高；轮次增加后稀释收敛，命中率趋近 95%+。真实 agent 长会话历史远大于每轮追加，同构——10 轮内的 tail_avg（82.2%）不代表稳态。
4. 此前 3 轮实测 tail_avg=79.7%（vendor commit `d26f02e`）与本曲线早期段一致；字节前缀 mock 守卫 tail_avg=96.5%（commit `69c49e9`）与稳态段一致。

复跑命令（vendor/agent-core 目录下，key 从环境变量读取、不落盘）：

```bash
DEEPSEEK_API_KEY=sk-... cargo test -p agent-llm --test deepseek_cache_probe -- --ignored --nocapture
```

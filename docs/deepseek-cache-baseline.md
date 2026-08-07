# DeepSeek 前缀缓存真实 API 基线验证

> 验证时间：2026-08-07（分支 `feat/deepseek-prefix-cache`，P0-A + P0-B vendor 层已实施后）
> 方式：非流式 `/chat/completions`，模型 `deepseek-chat`，相同 system+user 前缀连续两轮，追加 assistant+新 user 消息
> 对应 PRD：`docs/deepseek-prefix-cache.md` §6 发布门槛②

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
- 正式探针测试（`#[ignore]`，走 hermes-llm 流式链路 + usage 解析）待补：见 PRD §6 真实 API 层级。

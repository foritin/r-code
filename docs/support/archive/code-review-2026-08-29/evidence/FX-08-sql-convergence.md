# FX-08 evidence（host 裸 SQL 收敛）

## 生产 SQL 残留检查（rg 'SELECT |INSERT INTO|UPDATE x SET|DELETE FROM' src-tauri/src）
- migration.rs / attachment_migration.rs：schema DDL 与一次性数据迁移的合法 SQL 拥有者（保留）
- bin/plan_eval.rs：独立开发工具二进制（保留，非宿主命令路径）
- 其余命中均为 #[cfg(test)] 区域 fixture（commands.rs:26000+、support_bundle.rs tests、recovery.rs 测试 helper、mcp_server.rs:636 schema 断言）

## store 侧新增：crates/r-code-store/src/host_support.rs（17 个查询/事务函数 + 10 单测）
## 测试：store 274/274；host lib 707/707（曾 2 失败=事件名大小写，已修）

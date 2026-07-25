//! R-Code Host 二进制入口。
//!
//! P0 host binary [doc-19 §2 退出条件]：
//! 1. 初始化结构化日志
//! 2. 打开 SQLite 数据库（内存）
//! 3. 启动 IPC server，注册 `ping` 与 `task.create` 处理器
//! 4. `ping` 响应 `{ "pong": true }`
//!
//! 运行：`cargo run -p r-code-host`

use std::sync::Arc;

use hermes_ipc::IpcServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日志
    r_code_host::init_logging();
    tracing::info!("R-Code Host starting...");

    // 打开数据库（P0 阶段使用内存库；生产应使用文件库）
    let _db = r_code_store::Database::open_in_memory()?;
    tracing::info!("Database initialized");

    // 创建 IPC server
    let socket_path = std::env::temp_dir().join(format!("r-code-{}.sock", std::process::id()));
    let mut server = IpcServer::bind(socket_path.clone())?;

    // 注册 ping 处理器
    server.register("ping", Arc::new(r_code_host::ipc::PingHandler));

    // 注册 task.create 处理器
    server.register("task.create", Arc::new(r_code_host::ipc::TaskCreateHandler));

    tracing::info!("IPC server listening on {}", socket_path.display());

    // 服务（阻塞，接受连接直到进程被终止）
    server.serve().await?;

    Ok(())
}

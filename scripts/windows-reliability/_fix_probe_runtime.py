import io

NL = chr(10)

path = "src-tauri/src/commands.rs"
with io.open(path, encoding="utf-8") as f:
    lines = f.readlines()

start = next(i for i, l in enumerate(lines) if "pub struct ExecutionEnvProbe {" in l)
end = next(i for i in range(start, start + 60) if lines[i].rstrip() == "}")
# end 是结构体闭合；probe 函数紧随其后，找函数闭合（第二个 "}" 顶格）
fn_start = next(i for i in range(end + 1, end + 10) if "pub fn execution_env_probe()" in lines[i])
depth = 0
fn_end = None
for j in range(fn_start, len(lines)):
    depth += lines[j].count("{") - lines[j].count("}")
    if depth == 0 and j > fn_start:
        fn_end = j
        break
assert fn_end is not None

DQ = chr(34)
new_block = (
    "pub struct ExecutionEnvProbe {" + NL
    + "    pub dialect: String," + NL
    + "    pub program: String," + NL
    + "    pub git_bash_detected: bool," + NL
    + "    /// 已保存的 execution.bash_shell_path（None=自动探测；Some(" + DQ + DQ + ")=强制回落）。" + NL
    + "    pub configured_override: Option<String>," + NL
    + "}" + NL
    + NL
    + "pub fn execution_env_probe(config_dir: &Path) -> ExecutionEnvProbe {" + NL
    + "    let configured_override = SettingsService::new(config_dir.to_path_buf())" + NL
    + "        .load_execution_settings()" + NL
    + "        .ok()" + NL
    + "        .and_then(|settings| settings.execution.bash_shell_path);" + NL
    + "    #[cfg(windows)]" + NL
    + "    {" + NL
    + "        match r_code_gateway::resolve_windows_shell(configured_override.as_deref()) {" + NL
    + "            Ok(resolved) => ExecutionEnvProbe {" + NL
    + "                dialect: resolved.dialect.label().to_string()," + NL
    + "                program: resolved.program.to_string_lossy().into_owned()," + NL
    + "                git_bash_detected: matches!(" + NL
    + "                    resolved.dialect," + NL
    + "                    r_code_gateway::ShellDialect::GitBash" + NL
    + "                )," + NL
    + "                configured_override," + NL
    + "            }," + NL
    + "            Err(error) => ExecutionEnvProbe {" + NL
    + "                // 覆盖路径缺失等解析错误：如实呈现（卡片显示探测失败与原因）。" + NL
    + '                dialect: "unknown".to_string(),' + NL
    + "                program: error.to_string()," + NL
    + "                git_bash_detected: false," + NL
    + "                configured_override," + NL
    + "            }," + NL
    + "        }" + NL
    + "    }" + NL
    + "    #[cfg(not(windows))]" + NL
    + "    {" + NL
    + "        ExecutionEnvProbe {" + NL
    + '            dialect: r_code_gateway::ShellDialect::PosixSh.label().to_string(),' + NL
    + '            program: "/bin/sh".to_string(),' + NL
    + "            git_bash_detected: false," + NL
    + "            configured_override," + NL
    + "        }" + NL
    + "    }" + NL
    + "}" + NL
)
lines[start : fn_end + 1] = [new_block]

text = "".join(lines)

# settings_set 分支：保存后即时生效（此前补丁未落盘）
old = '''        settings.save_execution_settings(&execution).map_err(err_str)?;
        return Ok(());
    }'''
new = '''        settings.save_execution_settings(&execution).map_err(err_str)?;
        // 即时生效：gateway 的 override 快照更新 + Windows shell 解析缓存失效
        //（否则最长 5 分钟内仍按旧档执行）。
        state
            .tool_gateway
            .update_shell_override(execution.execution.bash_shell_path.clone());
        #[cfg(windows)]
        r_code_gateway::win_shell::invalidate_shell_cache();
        return Ok(());
    }'''
assert text.count(old) == 1, f"settings_set anchor: {text.count(old)}"
text = text.replace(old, new)

with io.open(path, "w", encoding="utf-8", newline="") as f:
    f.write(text)
print("commands.rs probe + settings_set patched")

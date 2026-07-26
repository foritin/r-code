//! Shell Integration — Shell 集成注入 [doc-03 §3]
//!
//! 纯函数 `shell_integration_spawn` 根据 shell 类型生成集成所需的参数和环境变量。
//!
//! - **zsh**: `ZDOTDIR` shim，`.zshrc` 注册 `precmd`/`preexec` hooks 发射 OSC 133 序列
//! - **bash**: `--init-file` shim，安装 `PROMPT_COMMAND` + `DEBUG` trap
//! - **fish**: `XDG_DATA_DIRS` prepend，使用 fish 事件处理器
//! - 其他 shell 或 `enabled: false`: 降级为纯 scrollback，无错误

use std::fs;
use std::path::{Path, PathBuf};

/// Shell 集成配置。
#[derive(Debug, Clone)]
pub struct ShellIntegrationConfig {
    pub shell: String,
    pub working_dir: PathBuf,
    pub enabled: bool,
}

/// Shell 集成设置结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellIntegrationResult {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// 生成 shell 集成 spawn 参数。
///
/// 根据 shell 类型返回对应的 args 和 env。
/// 对于不支持的 shell 或 `enabled: false`，返回空结果（降级模式，无错误）。
pub fn shell_integration_spawn(config: &ShellIntegrationConfig) -> ShellIntegrationResult {
    if !config.enabled {
        return ShellIntegrationResult {
            args: vec![],
            env: vec![],
        };
    }

    let shell_name = Path::new(&config.shell)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    match shell_name {
        "zsh" => spawn_zsh(),
        "bash" => spawn_bash(),
        "fish" => spawn_fish(),
        "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe" => spawn_powershell(),
        _ => ShellIntegrationResult {
            args: vec![],
            env: vec![],
        },
    }
}

/// zsh 集成：设置 ZDOTDIR 到临时目录，`.zshrc` 注册 OSC 133 hooks。
fn spawn_zsh() -> ShellIntegrationResult {
    let zdotdir = std::env::temp_dir().join(format!("r-code-zsh-{}", uuid::Uuid::new_v4()));

    if fs::create_dir_all(&zdotdir).is_err() {
        return ShellIntegrationResult {
            args: vec![],
            env: vec![],
        };
    }

    let zshrc = r#"# R-Code shell integration for zsh
# Source user's real zshrc
if [[ -n "$R_CODE_USER_ZDOTDIR" && -f "$R_CODE_USER_ZDOTDIR/.zshrc" ]]; then
    source "$R_CODE_USER_ZDOTDIR/.zshrc"
elif [[ -f "$HOME/.zshrc" ]]; then
    source "$HOME/.zshrc"
fi

# OSC 133 shell integration
_r_code_precmd() {
    local _exit_code=$?
    print -nP "\e]133;D;$_exit_code\a"
    print -nP "\e]133;A\a"
    print -nP "\e]133;B\a"
}

_r_code_preexec() {
    print -nP "\e]133;C\a"
}

precmd_functions=(_r_code_precmd $precmd_functions)
preexec_functions=(_r_code_preexec $preexec_functions)
"#;

    if fs::write(zdotdir.join(".zshrc"), zshrc).is_err() {
        return ShellIntegrationResult {
            args: vec![],
            env: vec![],
        };
    }

    // 保存用户原始 ZDOTDIR（如果有）
    let mut env = Vec::new();
    if let Ok(user_zdotdir) = std::env::var("ZDOTDIR") {
        if !user_zdotdir.is_empty() {
            env.push(("R_CODE_USER_ZDOTDIR".to_string(), user_zdotdir));
        }
    }
    env.push(("ZDOTDIR".to_string(), zdotdir.to_string_lossy().to_string()));

    ShellIntegrationResult { args: vec![], env }
}

/// bash 集成：使用 --init-file 指向脚本，安装 PROMPT_COMMAND + DEBUG trap。
fn spawn_bash() -> ShellIntegrationResult {
    let script_dir = std::env::temp_dir().join(format!("r-code-bash-{}", uuid::Uuid::new_v4()));

    if fs::create_dir_all(&script_dir).is_err() {
        return ShellIntegrationResult {
            args: vec![],
            env: vec![],
        };
    }

    let script_path = script_dir.join("r-code-init.bash");

    let script = r#"# R-Code shell integration for bash
# Source user's bashrc
if [[ -f "$HOME/.bashrc" ]]; then
    source "$HOME/.bashrc"
fi

# OSC 133 shell integration
__r_code_precmd() {
    local __exit_code=$?
    printf '\e]133;D;%s\a' "$__exit_code"
    printf '\e]133;A\a'
    printf '\e]133;B\a'
}

__r_code_preexec() {
    printf '\e]133;C\a'
}

# Prepend to PROMPT_COMMAND (preserve existing)
if [[ -z "$PROMPT_COMMAND" ]]; then
    PROMPT_COMMAND="__r_code_precmd"
else
    PROMPT_COMMAND="__r_code_precmd; $PROMPT_COMMAND"
fi

# DEBUG trap fires before each command - emit C marker
trap '__r_code_preexec' DEBUG
"#;

    if fs::write(&script_path, script).is_err() {
        return ShellIntegrationResult {
            args: vec![],
            env: vec![],
        };
    }

    ShellIntegrationResult {
        args: vec![
            "--init-file".to_string(),
            script_path.to_string_lossy().to_string(),
        ],
        env: vec![],
    }
}

/// fish 集成：prepend XDG_DATA_DIRS，使用 fish 事件处理器。
fn spawn_fish() -> ShellIntegrationResult {
    let data_dir = std::env::temp_dir().join(format!("r-code-fish-{}", uuid::Uuid::new_v4()));
    let conf_dir = data_dir.join("fish").join("vendor_conf.d");

    if fs::create_dir_all(&conf_dir).is_err() {
        return ShellIntegrationResult {
            args: vec![],
            env: vec![],
        };
    }

    let script = r#"# R-Code shell integration for fish

function _r_code_postexec --on-event fish_postexec
    printf '\e]133;D;%s\a' $status
end

function _r_code_prompt --on-event fish_prompt
    printf '\e]133;A\a'
    printf '\e]133;B\a'
end

function _r_code_preexec --on-event fish_preexec
    printf '\e]133;C\a'
end
"#;

    if fs::write(conf_dir.join("r-code.fish"), script).is_err() {
        return ShellIntegrationResult {
            args: vec![],
            env: vec![],
        };
    }

    // Prepend 到 XDG_DATA_DIRS（保留原始值）
    // Windows 使用 ; 作为路径分隔符，Unix 使用 :
    #[cfg(windows)]
    let separator = ";";
    #[cfg(not(windows))]
    let separator = ":";

    let xdg = match std::env::var("XDG_DATA_DIRS") {
        Ok(v) if !v.is_empty() => format!("{}{}{}", data_dir.to_string_lossy(), separator, v),
        _ => data_dir.to_string_lossy().to_string(),
    };

    ShellIntegrationResult {
        args: vec![],
        env: vec![("XDG_DATA_DIRS".to_string(), xdg)],
    }
}

/// PowerShell 集成：使用 -NoProfile + 自定义 profile 脚本安装 OSC 133 hooks。
///
/// 兼容 Windows PowerShell 5.x 和 PowerShell Core 7.x (pwsh)。
/// 通过自定义 prompt 函数发射 A/B 标记，通过 PSReadLine OutBuffer 处理 C 标记。
fn spawn_powershell() -> ShellIntegrationResult {
    let script_dir = std::env::temp_dir().join(format!("r-code-pwsh-{}", uuid::Uuid::new_v4()));

    if fs::create_dir_all(&script_dir).is_err() {
        return ShellIntegrationResult {
            args: vec![],
            env: vec![],
        };
    }

    let script_path = script_dir.join("r-code-init.ps1");

    let script = r#"# R-Code shell integration for PowerShell
# Source user's profile first
if (Test-Path $PROFILE) { . $PROFILE }

# OSC 133 shell integration
$_r_code_exit_code = 0

# Override prompt to emit OSC 133 A/B markers
function global:prompt {
    # Emit exit code of last command (D marker)
    [Console]::Write("`e]133;D;$_r_code_exit_code`a")
    # Emit A (prompt start) and B (command input start)
    [Console]::Write("`e]133;A`a")
    [Console]::Write("`e]133;B`a")
    # Reset exit code tracker
    $_r_code_exit_code = 0
    # Return original prompt text (simplified)
    "PS $($executionContext.SessionState.Path.CurrentLocation)> "
}

# Use PSReadLine to detect command execution (C marker)
# This fires when the user presses Enter to execute a command
if (Get-Module -ListAvailable -Name PSReadLine) {
    Import-Module PSReadLine
    Set-PSReadLineKeyHandler -Key Enter -ScriptBlock {
        # Emit C marker before executing the command
        [Console]::Write("`e]133;C`a")
        # Save the exit code after the command runs
        $_r_code_exit_code = $LASTEXITCODE
    }
}
"#;

    if fs::write(&script_path, script).is_err() {
        return ShellIntegrationResult {
            args: vec![],
            env: vec![],
        };
    }

    ShellIntegrationResult {
        args: vec![
            "-NoExit".to_string(),
            "-NoProfile".to_string(),
            "-File".to_string(),
            script_path.to_string_lossy().to_string(),
        ],
        env: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_returns_empty() {
        let config = ShellIntegrationConfig {
            shell: "/bin/zsh".to_string(),
            working_dir: PathBuf::from("/tmp"),
            enabled: false,
        };
        let result = shell_integration_spawn(&config);
        assert!(result.args.is_empty());
        assert!(result.env.is_empty());
    }

    #[test]
    fn unknown_shell_returns_empty() {
        let config = ShellIntegrationConfig {
            shell: "/bin/dash".to_string(),
            working_dir: PathBuf::from("/tmp"),
            enabled: true,
        };
        let result = shell_integration_spawn(&config);
        assert!(result.args.is_empty());
        assert!(result.env.is_empty());
    }

    #[test]
    fn bare_shell_name_resolved() {
        // 无路径前缀的 shell 名也应正确识别
        let config = ShellIntegrationConfig {
            shell: "zsh".to_string(),
            working_dir: PathBuf::from("/tmp"),
            enabled: true,
        };
        let result = shell_integration_spawn(&config);
        assert!(result.env.iter().any(|(k, _)| k == "ZDOTDIR"));
    }

    #[test]
    fn zsh_sets_zdotdir_and_creates_zshrc() {
        let config = ShellIntegrationConfig {
            shell: "/bin/zsh".to_string(),
            working_dir: PathBuf::from("/tmp"),
            enabled: true,
        };
        let result = shell_integration_spawn(&config);

        // ZDOTDIR 应在 env 中
        let zdotdir = result
            .env
            .iter()
            .find(|(k, _)| k == "ZDOTDIR")
            .map(|(_, v)| v.clone())
            .expect("ZDOTDIR should be set");
        assert!(!zdotdir.is_empty());

        // .zshrc 文件应存在
        let zshrc_path = Path::new(&zdotdir).join(".zshrc");
        assert!(zshrc_path.exists(), ".zshrc should exist at {zshrc_path:?}");

        // .zshrc 应包含 OSC 133 hooks
        let content = fs::read_to_string(&zshrc_path).expect("should read .zshrc");
        assert!(content.contains("133;A"), "should contain OSC 133;A");
        assert!(content.contains("133;B"), "should contain OSC 133;B");
        assert!(content.contains("133;C"), "should contain OSC 133;C");
        assert!(content.contains("133;D"), "should contain OSC 133;D");
        assert!(
            content.contains("precmd_functions"),
            "should register precmd hook"
        );
        assert!(
            content.contains("preexec_functions"),
            "should register preexec hook"
        );

        // 清理
        let _ = fs::remove_dir_all(&zdotdir);
    }

    #[test]
    fn zsh_preserves_user_zdotdir() {
        // 临时设置 ZDOTDIR
        let original = std::env::var("ZDOTDIR").ok();
        // SAFETY: 单线程测试，环境变量修改是安全的
        std::env::set_var("ZDOTDIR", "/custom/user/zdotdir");

        let config = ShellIntegrationConfig {
            shell: "/bin/zsh".to_string(),
            working_dir: PathBuf::from("/tmp"),
            enabled: true,
        };
        let result = shell_integration_spawn(&config);

        let user_zdotdir = result
            .env
            .iter()
            .find(|(k, _)| k == "R_CODE_USER_ZDOTDIR")
            .map(|(_, v)| v.clone());

        // 恢复原始值
        match original {
            Some(v) => std::env::set_var("ZDOTDIR", v),
            None => std::env::remove_var("ZDOTDIR"),
        }

        assert_eq!(
            user_zdotdir.as_deref(),
            Some("/custom/user/zdotdir"),
            "should preserve user ZDOTDIR"
        );

        // 清理临时文件
        if let Some((_, zdotdir)) = result.env.iter().find(|(k, _)| k == "ZDOTDIR") {
            let _ = fs::remove_dir_all(zdotdir);
        }
    }

    #[test]
    fn bash_sets_init_file_arg_and_creates_script() {
        let config = ShellIntegrationConfig {
            shell: "/bin/bash".to_string(),
            working_dir: PathBuf::from("/tmp"),
            enabled: true,
        };
        let result = shell_integration_spawn(&config);

        // args 应包含 --init-file
        assert!(
            result.args.iter().any(|a| a == "--init-file"),
            "args should contain --init-file"
        );

        let script_path = result
            .args
            .iter()
            .skip_while(|a| *a != "--init-file")
            .nth(1)
            .cloned()
            .expect("should have script path after --init-file");

        // 脚本文件应存在
        assert!(
            Path::new(&script_path).exists(),
            "init script should exist at {script_path:?}"
        );

        // 脚本应包含 OSC 133 和 PROMPT_COMMAND
        let content = fs::read_to_string(&script_path).expect("should read script");
        assert!(content.contains("133;A"), "should contain OSC 133;A");
        assert!(content.contains("133;C"), "should contain OSC 133;C");
        assert!(
            content.contains("PROMPT_COMMAND"),
            "should install PROMPT_COMMAND"
        );
        assert!(content.contains("trap"), "should set DEBUG trap");

        // 清理
        if let Some(parent) = Path::new(&script_path).parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    #[cfg(unix)] // fish 是 Unix shell；XDG_DATA_DIRS 语义在 Windows 不存在
    #[test]
    fn fish_sets_xdg_data_dirs_and_creates_script() {
        let original = std::env::var("XDG_DATA_DIRS").ok();

        let config = ShellIntegrationConfig {
            shell: "/usr/bin/fish".to_string(),
            working_dir: PathBuf::from("/tmp"),
            enabled: true,
        };
        let result = shell_integration_spawn(&config);

        // XDG_DATA_DIRS 应在 env 中
        let xdg = result
            .env
            .iter()
            .find(|(k, _)| k == "XDG_DATA_DIRS")
            .map(|(_, v)| v.clone())
            .expect("XDG_DATA_DIRS should be set");
        assert!(!xdg.is_empty());

        // fish 脚本应存在（只取第一个路径组件）
        let first_dir = xdg.split(':').next().unwrap_or("");
        let script_path = Path::new(first_dir)
            .join("fish")
            .join("vendor_conf.d")
            .join("r-code.fish");
        assert!(
            script_path.exists(),
            "fish script should exist at {script_path:?}"
        );

        // 脚本应包含 OSC 133 和 fish 事件处理器
        let content = fs::read_to_string(&script_path).expect("should read fish script");
        assert!(content.contains("133;A"), "should contain OSC 133;A");
        assert!(content.contains("133;C"), "should contain OSC 133;C");
        assert!(
            content.contains("fish_prompt"),
            "should use fish_prompt event"
        );
        assert!(
            content.contains("fish_preexec"),
            "should use fish_preexec event"
        );
        assert!(
            content.contains("fish_postexec"),
            "should use fish_postexec event"
        );

        // 清理
        let data_dir = xdg.split(':').next().unwrap_or("");
        let _ = fs::remove_dir_all(data_dir);

        // 恢复原始值
        match original {
            Some(v) => std::env::set_var("XDG_DATA_DIRS", v),
            None => std::env::remove_var("XDG_DATA_DIRS"),
        }
    }

    #[cfg(unix)] // fish 是 Unix shell；XDG_DATA_DIRS 语义在 Windows 不存在
    #[test]
    fn fish_preserves_existing_xdg_data_dirs() {
        let original = std::env::var("XDG_DATA_DIRS").ok();
        std::env::set_var("XDG_DATA_DIRS", "/usr/share:/local/share");

        let config = ShellIntegrationConfig {
            shell: "fish".to_string(),
            working_dir: PathBuf::from("/tmp"),
            enabled: true,
        };
        let result = shell_integration_spawn(&config);

        let xdg = result
            .env
            .iter()
            .find(|(k, _)| k == "XDG_DATA_DIRS")
            .map(|(_, v)| v.clone())
            .expect("XDG_DATA_DIRS should be set");

        // 应保留原始值
        assert!(
            xdg.contains("/usr/share:/local/share"),
            "should preserve original XDG_DATA_DIRS: got {xdg}"
        );
        // 原始值应在 prepend 之后
        let prefix = xdg.split(':').next().unwrap_or("");
        assert!(
            prefix.contains("r-code-fish"),
            "should prepend r-code dir: got {prefix}"
        );

        // 清理
        let data_dir = prefix;
        let _ = fs::remove_dir_all(data_dir);
        match original {
            Some(v) => std::env::set_var("XDG_DATA_DIRS", v),
            None => std::env::remove_var("XDG_DATA_DIRS"),
        }
    }

    #[test]
    fn powershell_generates_integration_script() {
        let config = ShellIntegrationConfig {
            shell: "powershell.exe".to_string(),
            working_dir: PathBuf::from("C:\\Users"),
            enabled: true,
        };
        let result = shell_integration_spawn(&config);

        // 应包含 -NoExit -NoProfile -File 参数
        assert!(!result.args.is_empty(), "should have args for PowerShell");
        assert!(
            result.args.iter().any(|a| a == "-NoProfile"),
            "should pass -NoProfile"
        );
        assert!(
            result.args.iter().any(|a| a == "-NoExit"),
            "should pass -NoExit"
        );
        assert!(
            result.args.iter().any(|a| a == "-File"),
            "should pass -File"
        );

        // 脚本文件应存在
        let script_path = result
            .args
            .iter()
            .rev()
            .find(|a| a.ends_with(".ps1"))
            .expect("should have a .ps1 script path");
        assert!(
            PathBuf::from(script_path).exists(),
            "script file should exist"
        );

        // 脚本应包含 OSC 133 序列
        let content = fs::read_to_string(script_path).expect("should read PS script");
        assert!(content.contains("133;A"), "should contain OSC 133;A");
        assert!(content.contains("133;B"), "should contain OSC 133;B");
        assert!(content.contains("133;C"), "should contain OSC 133;C");
        assert!(
            content.contains("133;D"),
            "should contain OSC 133;D (exit code)"
        );
        assert!(
            content.contains("PSReadLine"),
            "should use PSReadLine for C marker"
        );
        assert!(
            content.contains("function global:prompt"),
            "should override prompt function"
        );

        // 清理
        if let Some(parent) = PathBuf::from(script_path).parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn pwsh_core_also_supported() {
        let config = ShellIntegrationConfig {
            shell: "pwsh".to_string(),
            working_dir: PathBuf::from("/tmp"),
            enabled: true,
        };
        let result = shell_integration_spawn(&config);
        assert!(!result.args.is_empty(), "pwsh should also be supported");
        assert!(
            result.args.iter().any(|a| a == "-File"),
            "pwsh should also use -File"
        );

        // 清理
        let script_path = result
            .args
            .iter()
            .rev()
            .find(|a| a.ends_with(".ps1"))
            .expect("should have a .ps1 script path");
        if let Some(parent) = PathBuf::from(script_path).parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn cmd_exe_returns_empty_degraded() {
        // cmd.exe 不支持 shell 集成，应降级为纯 scrollback
        let config = ShellIntegrationConfig {
            shell: "cmd.exe".to_string(),
            working_dir: PathBuf::from("C:\\"),
            enabled: true,
        };
        let result = shell_integration_spawn(&config);
        assert!(result.args.is_empty(), "cmd.exe should degrade to empty");
        assert!(result.env.is_empty(), "cmd.exe should have no env");
    }
}

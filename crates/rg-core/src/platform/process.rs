//! Cross-platform process management utilities.

use std::process::Command;

/// Execute a shell script in a cross-platform way.
///
/// # Unix
/// Uses `sh -c <script>`
///
/// # Windows  
/// Uses `powershell.exe -Command <script>`
///
/// # Errors
/// Returns IoError if the command cannot be executed.
pub fn execute_script(script: &str) -> std::io::Result<std::process::Output> {
    #[cfg(unix)]
    {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script);
        cmd.output()
    }

    #[cfg(windows)]
    {
        // Try PowerShell first, fall back to cmd.exe
        let mut cmd = Command::new("powershell.exe");
        cmd.arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-Command")
            .arg(script);

        match cmd.output() {
            Ok(output) => Ok(output),
            Err(_) => {
                // Fallback to cmd.exe
                let mut cmd = Command::new("cmd.exe");
                cmd.arg("/C").arg(script);
                cmd.output()
            }
        }
    }
}

/// Execute a command with arguments (cross-platform).
///
/// # Examples
/// ```rust,no_run
/// use rg_core::platform::process::execute_command;
/// let output = execute_command("git", &["status"]).unwrap();
/// ```
pub fn execute_command(program: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    cmd.output()
}

/// Terminate a process (cross-platform).
///
/// # Unix
/// Uses `kill -9 <pid>`
///
/// # Windows
/// Uses `taskkill /PID <pid> /F`
pub fn terminate_process(pid: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-9", &pid.to_string()])
            .output()?;
        Ok(())
    }

    #[cfg(windows)]
    {
        Command::new("taskkill")
            .args(&["/PID", &pid.to_string(), "/F"])
            .output()?;
        Ok(())
    }
}

/// Check if a process is running (cross-platform).
///
/// # Unix
/// Uses `kill -0 <pid>` to check if process exists.
///
/// # Windows
/// Uses `tasklist /FI "PID eq <pid>"` to check.
pub fn is_process_running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    #[cfg(windows)]
    {
        let output = Command::new("tasklist")
            .args(&["/FI", &format!("PID eq {}", pid), "/NH"])
            .output();

        match output {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                stdout.contains(&pid.to_string())
            }
            Err(_) => false,
        }
    }
}

/// Get the platform-specific shell command.
///
/// # Returns
/// - Unix: `("sh", "-c")`
/// - Windows: `("cmd.exe", "/C")`
pub fn get_shell() -> (&'static str, &'static str) {
    #[cfg(unix)]
    {
        ("sh", "-c")
    }

    #[cfg(windows)]
    {
        ("cmd.exe", "/C")
    }
}

/// Get the platform-specific shell for scripting.
///
/// # Returns
/// - Unix: `("bash", "-c")`
/// - Windows: `("powershell.exe", "-Command")`
pub fn get_script_shell() -> (&'static str, &'static str) {
    #[cfg(unix)]
    {
        ("bash", "-c")
    }

    #[cfg(windows)]
    {
        ("powershell.exe", "-Command")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_shell() {
        let (shell, flag) = get_shell();
        assert!(!shell.is_empty());
        assert!(!flag.is_empty());
    }

    #[test]
    fn test_get_script_shell() {
        let (shell, flag) = get_script_shell();
        assert!(!shell.is_empty());
        assert!(!flag.is_empty());
    }

    #[test]
    fn test_execute_script_echo() {
        #[cfg(unix)]
        let script = "echo hello";

        #[cfg(windows)]
        let script = "Write-Output 'hello'";

        let result = execute_script(script);
        assert!(result.is_ok());
    }
}

use std::path::PathBuf;
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;

/// Execute a command with timeout and retry.
pub async fn execute_with_timeout(
    cmd: &str,
    args: &[String],
    timeout_dur: Duration,
    max_retries: u32,
) -> Result<String, anyhow::Error> {
    let mut last_error = None;

    for attempt in 0..=max_retries {
        match execute_once(cmd, args, timeout_dur).await {
            Ok(output) => return Ok(output),
            Err(e) => {
                tracing::warn!(
                    "Command failed (attempt {}/{}): {}",
                    attempt + 1,
                    max_retries + 1,
                    e
                );
                last_error = Some(e);
                if attempt < max_retries {
                    tokio::time::sleep(Duration::from_millis(100 * (attempt as u64 + 1))).await;
                }
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("No attempts made")))
}

async fn execute_once(
    cmd: &str,
    args: &[String],
    timeout_dur: Duration,
) -> Result<String, anyhow::Error> {
    let output = timeout(timeout_dur, Command::new(cmd).args(args).output())
        .await
        .map_err(|_| anyhow::anyhow!("Command timed out after {:?}", timeout_dur))?
        .map_err(|e| anyhow::anyhow!("Failed to execute {}: {}", cmd, e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!(
            "Command failed with status {}: {}",
            output.status,
            stderr
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Find a binary in PATH.
pub fn find_binary(name: &str) -> Option<PathBuf> {
    which::which(name).ok()
}

/// Check if a binary exists.
pub fn binary_exists(name: &str) -> bool {
    find_binary(name).is_some()
}

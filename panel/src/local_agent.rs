use crate::runtime::{atomic_write, private_dir};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::{Child, Command};

#[derive(Serialize, Deserialize)]
pub struct Identity {
    pub id: String,
    pub token: String,
}

pub fn identity(dir: &Path) -> Result<Option<Identity>> {
    let path = dir.join("local-agent.json");
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn node_id(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("local-system-node"))
        .ok()
        .map(|id| id.trim().to_owned())
        .or_else(|| identity(dir).ok().flatten().map(|i| i.id))
        .filter(|id| uuid::Uuid::parse_str(id).is_ok())
}

pub fn binary() -> Result<PathBuf> {
    let path = std::env::current_exe()?.with_file_name("chain-agent");
    ensure!(
        path.is_file(),
        "缺少本机运行组件 chain-agent，请同时构建或安装控制台和 Agent"
    );
    Ok(path)
}

pub fn save(dir: &Path, value: &Identity) -> Result<()> {
    atomic_write(&dir.join("local-agent.json"), &serde_json::to_vec(value)?)
}

pub struct LocalAgent {
    child: Option<Child>,
    group: Option<i32>,
}

impl Default for LocalAgent {
    fn default() -> Self {
        Self {
            child: None,
            group: None,
        }
    }
}

impl Drop for LocalAgent {
    fn drop(&mut self) {
        if let Some(group) = self.group {
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
    }
}

impl LocalAgent {
    pub async fn stop(&mut self) {
        if let Some(group) = self.group {
            unsafe {
                libc::kill(-group, libc::SIGTERM);
            }
        }
        if let Some(child) = &mut self.child {
            let _ = tokio::time::timeout(Duration::from_secs(8), child.wait()).await;
        }
        if let Some(group) = self.group.take() {
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.wait().await;
        }
    }

    pub async fn ensure(&mut self, dir: &Path, panel: &str, xray: &Path) -> Result<()> {
        if dir.join("local-system-node").exists() {
            self.stop().await;
            return Ok(());
        }
        let Some(identity) = identity(dir)? else {
            return Ok(());
        };
        if let Some(child) = &mut self.child {
            if child.try_wait()?.is_none() {
                return Ok(());
            }
            self.stop().await;
        }
        let binary = binary()?;
        let data = dir.join("local-agent");
        private_dir(&data, "chain-agent")?;
        let token = data.join("token");
        atomic_write(&token, identity.token.as_bytes())?;
        let mut command = Command::new(binary);
        command
            .env("CHAIN_AGENT_DIR", &data)
            .env("CHAIN_PANEL_URL", panel)
            // This URL comes from our bound listener, never from an API caller.
            .env("CHAIN_ALLOW_HTTP", "1")
            .env("CHAIN_NODE_ID", identity.id)
            .env("CHAIN_NODE_TOKEN_FILE", token)
            .env("CHAIN_XRAY_BIN", xray)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command.process_group(0);
        let parent = unsafe { libc::getpid() };
        // An abruptly terminated controller must not leave its local Agent running.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    return Err(std::io::Error::other("控制台已退出"));
                }
                Ok(())
            });
        }
        let child = command.spawn().context("无法启动本机代理组件")?;
        self.group = child.id().map(|id| id as i32);
        self.child = Some(child);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_system_identity_takes_precedence() {
        let dir = tempfile::tempdir().unwrap();
        assert!(node_id(dir.path()).is_none());
        let managed = uuid::Uuid::new_v4().to_string();
        save(
            dir.path(),
            &Identity {
                id: managed.clone(),
                token: "secret".into(),
            },
        )
        .unwrap();
        assert_eq!(node_id(dir.path()).as_deref(), Some(managed.as_str()));
        let system = uuid::Uuid::new_v4().to_string();
        atomic_write(&dir.path().join("local-system-node"), system.as_bytes()).unwrap();
        assert_eq!(node_id(dir.path()).as_deref(), Some(system.as_str()));
    }
}

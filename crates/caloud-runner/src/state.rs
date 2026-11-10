//! 節點權杖的本機保存（docs/07 §4：檔案權限 0600）。
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeState {
    pub server: String,
    pub node_id: String,
    pub node_token: String,
}

/// 預設目錄：`$HOME/.caloud-agent`
pub fn default_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".caloud-agent")
}

fn file(dir: &Path) -> PathBuf {
    dir.join("node.json")
}

pub fn load(dir: &Path) -> anyhow::Result<Option<NodeState>> {
    match std::fs::read_to_string(file(dir)) {
        Ok(s) => Ok(Some(serde_json::from_str(&s)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn save(dir: &Path, st: &NodeState) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = file(dir);
    let tmp = dir.join("node.json.tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    {
        use std::io::Write;
        let mut f = opts.open(&tmp)?;
        f.write_all(serde_json::to_string_pretty(st)?.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // FR-OPS-04
    #[test]
    fn saves_token_privately_and_loads_it_back() {
        let dir = std::env::temp_dir().join(format!("caloud-agent-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(load(&dir).unwrap(), None);
        let st = NodeState {
            server: "https://caloud.cafeca.io".into(),
            node_id: "n1".into(),
            node_token: "caloud_node_x".into(),
        };
        save(&dir, &st).unwrap();
        assert_eq!(load(&dir).unwrap(), Some(st));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("node.json")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

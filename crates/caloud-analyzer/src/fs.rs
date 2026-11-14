//! 安全讀取不受信任的儲存庫（docs/07 §1「服務解析」）：不跟隨 symlink、只讀根目錄內、限制大小與數量。
use std::cell::Cell;
use std::path::{Component, Path, PathBuf};

/// 單檔上限
pub const MAX_FILE: u64 = 1024 * 1024;
/// 一次解析最多讀取或列出的檔案數
pub const MAX_FILES: usize = 5000;
/// 走訪時略過的目錄
pub const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    ".next",
    "vendor",
    "build",
    "__pycache__",
    ".venv",
    "venv",
];

pub struct RepoFs {
    repo: PathBuf,
    /// 分析根目錄（儲存庫相對路徑，"" 為儲存庫根目錄）
    root: String,
    budget: Cell<usize>,
}

/// 正規化相對路徑；絕對路徑、`..`、反斜線一律拒絕
pub fn clean_rel(rel: &str) -> Option<String> {
    if rel.contains('\\') || rel.contains('\0') {
        return None;
    }
    let mut parts = Vec::new();
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str()?.to_string()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

impl RepoFs {
    /// `root` 不存在、不是目錄或經過 symlink 時回傳 None
    pub fn new(repo: &Path, root: &str) -> Option<Self> {
        let root = clean_rel(root)?;
        let fs = Self {
            repo: repo.to_path_buf(),
            root: String::new(),
            budget: Cell::new(MAX_FILES),
        };
        if !root.is_empty() && !fs.is_dir(&root) {
            return None;
        }
        Some(Self { root, ..fs })
    }

    pub fn root(&self) -> &str {
        &self.root
    }

    /// 根目錄相對路徑 → 儲存庫相對路徑（作為依據顯示）
    pub fn display(&self, rel: &str) -> String {
        match (self.root.is_empty(), rel.is_empty()) {
            (true, _) => rel.to_string(),
            (false, true) => self.root.clone(),
            (false, false) => format!("{}/{rel}", self.root),
        }
    }

    /// 根目錄相對路徑轉實際路徑；每一層都不可為 symlink
    fn resolve(&self, rel: &str) -> Option<PathBuf> {
        let rel = clean_rel(rel)?;
        let mut p = self.repo.clone();
        for part in self.root.split('/').chain(rel.split('/')).filter(|s| !s.is_empty()) {
            p.push(part);
            let m = std::fs::symlink_metadata(&p).ok()?;
            if m.file_type().is_symlink() {
                return None;
            }
        }
        Some(p)
    }

    fn take_budget(&self) -> bool {
        let b = self.budget.get();
        if b == 0 {
            return false;
        }
        self.budget.set(b - 1);
        true
    }

    pub fn is_file(&self, rel: &str) -> bool {
        self.resolve(rel)
            .and_then(|p| std::fs::symlink_metadata(p).ok())
            .is_some_and(|m| m.is_file())
    }

    pub fn is_dir(&self, rel: &str) -> bool {
        self.resolve(rel)
            .and_then(|p| std::fs::symlink_metadata(p).ok())
            .is_some_and(|m| m.is_dir())
    }

    /// 讀文字檔；超過大小、非檔案或 symlink 回傳 None
    pub fn read(&self, rel: &str) -> Option<String> {
        let p = self.resolve(rel)?;
        let m = std::fs::symlink_metadata(&p).ok()?;
        if !m.is_file() || m.len() > MAX_FILE || !self.take_budget() {
            return None;
        }
        let bytes = std::fs::read(&p).ok()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// 列出目錄（略過 symlink）：(名稱, 是否為目錄)，依名稱排序
    pub fn list(&self, rel: &str) -> Vec<(String, bool)> {
        let Some(p) = self.resolve(rel) else { return vec![] };
        let Ok(rd) = std::fs::read_dir(p) else { return vec![] };
        let mut out = Vec::new();
        for e in rd.flatten() {
            if !self.take_budget() {
                break;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if let Some(name) = e.file_name().to_str() {
                out.push((name.to_string(), ft.is_dir()));
            }
        }
        out.sort();
        out
    }

    /// 走訪 `dir` 以下（深度上限 `depth`），回傳副檔名符合的檔案
    pub fn walk(&self, dir: &str, depth: usize, exts: &[&str]) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![(dir.to_string(), 0usize)];
        while let Some((d, level)) = stack.pop() {
            for (name, is_dir) in self.list(&d) {
                let rel = if d.is_empty() { name.clone() } else { format!("{d}/{name}") };
                if is_dir {
                    if level < depth && !SKIP_DIRS.contains(&name.as_str()) && !name.starts_with('.') {
                        stack.push((rel, level + 1));
                    }
                } else if exts.iter().any(|e| name.ends_with(e)) {
                    out.push(rel);
                }
            }
        }
        out.sort();
        out
    }
}

/// 第一個包含 `needle` 的行號（從 1 起算）
pub fn line_of(content: &str, needle: &str) -> Option<u32> {
    content.lines().position(|l| l.contains(needle)).map(|i| i as u32 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_escapes_and_symlinks() {
        assert_eq!(clean_rel("./a/b"), Some("a/b".into()));
        assert_eq!(clean_rel(""), Some(String::new()));
        assert!(clean_rel("../x").is_none());
        assert!(clean_rel("/etc").is_none());
        assert!(clean_rel("a\\b").is_none());

        let dir = std::env::temp_dir().join(format!("caloud-an-fs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("app")).unwrap();
        std::fs::write(dir.join("app/a.txt"), "hello\nworld").unwrap();
        std::fs::write(dir.join("secret"), "x").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("secret"), dir.join("app/link")).unwrap();
            std::os::unix::fs::symlink(dir.join("app"), dir.join("applink")).unwrap();
        }
        let fs = RepoFs::new(&dir, "app").unwrap();
        assert_eq!(fs.read("a.txt").as_deref(), Some("hello\nworld"));
        assert_eq!(fs.display("a.txt"), "app/a.txt");
        assert!(fs.read("../secret").is_none());
        #[cfg(unix)]
        {
            assert!(fs.read("link").is_none());
            assert!(RepoFs::new(&dir, "applink").is_none());
            assert!(!fs.list("").iter().any(|(n, _)| n == "link"));
        }
        assert!(RepoFs::new(&dir, "nope").is_none());
        assert_eq!(line_of("a\nb\nc", "c"), Some(3));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

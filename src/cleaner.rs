use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

const CACHE_WHITELIST: &[&str] = &[
    "node_modules", ".next", ".nuxt", ".svelte-kit", ".turbo",
    ".parcel-cache", "dist", "build",
    "target",
    ".venv", "venv", "__pycache__", ".pytest_cache", ".ruff_cache",
    "vendor", ".gradle",
];

#[derive(Debug, Clone)]
pub struct DeleteResult {
    pub cache_name: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub success: bool,
    pub reason: Option<String>,
}

fn verify_cache_path(path: &Path) -> Result<(), String> {
    let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if !CACHE_WHITELIST.contains(&fname) {
        return Err(format!("Refused to delete '{}': not in the allowed cache whitelist", fname));
    }
    if !path.is_dir() {
        return Err(format!("Expected a directory, found: {}", path.display()));
    }
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let parent = match canonical.parent() {
        Some(p) => p.to_path_buf(),
        None => return Err("Cannot determine parent".into()),
    };
    let parent_has_marker = fs::read_dir(&parent).map_or(false, |rd| {
        rd.flatten().any(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            ["package.json", "Cargo.toml", "pyproject.toml", "go.mod", "composer.json", ".git"]
                .contains(&name.as_str())
        })
    });
    if !parent_has_marker {
        return Err(format!(
            "Refused: '{}' is not a direct cache child of a recognized project folder",
            path.display()
        ));
    }
    Ok(())
}

fn delete_one(path: &Path, size_before: u64) -> DeleteResult {
    let cache_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    match verify_cache_path(path) {
        Ok(()) => match fs::remove_dir_all(path) {
            Ok(()) => DeleteResult {
                cache_name,
                path: path.to_path_buf(),
                size_bytes: size_before,
                success: true,
                reason: None,
            },
            Err(e) => DeleteResult {
                cache_name,
                path: path.to_path_buf(),
                size_bytes: size_before,
                success: false,
                reason: Some(e.to_string()),
            },
        },
        Err(e) => DeleteResult {
            cache_name,
            path: path.to_path_buf(),
            size_bytes: size_before,
            success: false,
            reason: Some(e),
        },
    }
}

pub fn purge_caches_background(
    to_purge: &[PathBuf],
    tx: mpsc::Sender<DeleteResult>,
) -> thread::JoinHandle<()> {
    let paths: Arc<[PathBuf]> = to_purge.iter().cloned().collect();
    let tx = Arc::new(mpsc::Sender::clone(&tx));
    thread::spawn(move || {
        let sizes: Vec<u64> = paths
            .iter()
            .map(|p| {
                fs::metadata(p)
                    .ok()
                    .map(|m| m.len())
                    .unwrap_or_else(|| walk_dir_size(p))
            })
            .collect();
        let results: Vec<DeleteResult> = paths
            .iter()
            .zip(sizes.iter())
            .map(|(p, sz)| delete_one(p, *sz))
            .collect();
        for r in results {
            let _ = tx.send(r);
        }
        let _ = tx.send(DeleteResult {
            cache_name: "__done__".into(),
            path: PathBuf::new(),
            size_bytes: 0,
            success: true,
            reason: None,
        });
    })
}

fn walk_dir_size(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "lemonwatch-clean-{}-{}-{}",
                tag,
                std::process::id(),
                nanos
            ));
            fs::create_dir_all(&path).expect("create temp workspace");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn project_with_cache(tag: &str, marker: &str, cache: &str) -> (TempDir, PathBuf, PathBuf) {
        let root = TempDir::new(tag);
        let project_dir = root.path().join("project");
        let cache_dir = project_dir.join(cache);
        fs::create_dir_all(cache_dir.join("nested")).unwrap();
        fs::write(cache_dir.join("nested/blob.bin"), b"cache payload").unwrap();
        fs::write(project_dir.join(marker), b"marker").unwrap();
        (root, project_dir, cache_dir)
    }

    #[test]
    fn deletes_a_whitelisted_cache_beside_a_project_marker() {
        let (_root, project_dir, cache_dir) = project_with_cache("ok", "package.json", "node_modules");

        let result = delete_one(&cache_dir, 4096);

        assert!(result.success, "expected deletion, got {:?}", result.reason);
        assert_eq!(result.cache_name, "node_modules");
        assert_eq!(result.size_bytes, 4096);
        assert!(!cache_dir.exists(), "cache directory should be gone");
        assert!(
            project_dir.join("package.json").exists(),
            "project sources must survive the purge"
        );
    }

    #[test]
    fn refuses_a_directory_that_is_not_whitelisted() {
        let root = TempDir::new("whitelist");
        let project_dir = root.path().join("project");
        let sources = project_dir.join("src");
        fs::create_dir_all(&sources).unwrap();
        fs::write(project_dir.join("Cargo.toml"), b"[package]").unwrap();
        fs::write(sources.join("main.rs"), b"fn main() {}").unwrap();

        let result = delete_one(&sources, 12);

        assert!(!result.success, "non-whitelisted directory must be refused");
        assert!(sources.exists(), "sources must not be deleted");
        let reason = result.reason.unwrap_or_default();
        assert!(
            reason.contains("whitelist"),
            "reason should name the whitelist, got: {reason}"
        );
    }

    #[test]
    fn refuses_a_whitelisted_name_without_a_project_marker() {
        let root = TempDir::new("nomarker");
        let orphan = root.path().join("target");
        fs::create_dir_all(&orphan).unwrap();
        fs::write(orphan.join("artifact.bin"), b"x").unwrap();

        let result = delete_one(&orphan, 1);

        assert!(!result.success, "a marker-less cache must be refused");
        assert!(orphan.exists(), "directory must survive the refusal");
        let reason = result.reason.unwrap_or_default();
        assert!(
            reason.contains("recognized project"),
            "reason should explain the missing marker, got: {reason}"
        );
    }

    #[test]
    fn refuses_a_file_even_when_named_like_a_cache() {
        let root = TempDir::new("file");
        let project_dir = root.path().join("project");
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(project_dir.join("package.json"), b"{}").unwrap();
        let fake_cache = project_dir.join("node_modules");
        fs::write(&fake_cache, b"not a directory").unwrap();

        let result = delete_one(&fake_cache, 14);

        assert!(!result.success, "files must never be purged");
        assert!(fake_cache.exists());
        let reason = result.reason.unwrap_or_default();
        assert!(
            reason.contains("Expected a directory"),
            "reason should mention the directory check, got: {reason}"
        );
    }

    #[test]
    fn background_purge_reports_every_cache_then_done() {
        let (_root, project_dir, node_cache) =
            project_with_cache("bg", "package.json", "node_modules");
        let target_cache = project_dir.join("target");
        fs::create_dir_all(target_cache.join("debug")).unwrap();
        fs::write(target_cache.join("debug/app"), b"binary").unwrap();

        let (tx, rx) = mpsc::channel();
        let handle = purge_caches_background(&[node_cache.clone(), target_cache.clone(), project_dir.join("missing")], tx);
        handle.join().expect("purge thread finished");
        let results: Vec<DeleteResult> = rx.try_iter().collect();

        assert_eq!(results.len(), 4, "one result per path plus the sentinel");
        assert_eq!(results.last().map(|r| r.cache_name.as_str()), Some("__done__"));

        let node = results.iter().find(|r| r.cache_name == "node_modules").expect("node_modules result");
        assert!(node.success, "whitelisted cache should purge: {:?}", node.reason);
        assert!(!node_cache.exists());

        let target = results.iter().find(|r| r.cache_name == "target").expect("target result");
        assert!(target.success, "target cache should purge: {:?}", target.reason);
        assert!(!target_cache.exists());

        let missing = results.iter().find(|r| r.cache_name == "missing").expect("missing result");
        assert!(!missing.success, "a missing path cannot be purged");
    }
}

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

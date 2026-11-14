use anyhow::Result;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use walkdir::WalkDir;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Ecosystem {
    Node,
    Rust,
    Python,
    Go,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedCache {
    pub name: String,
    pub path: PathBuf,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub path: PathBuf,
    pub ecosystem: Ecosystem,
    pub caches: Vec<DetectedCache>,
    pub total_cache_bytes: u64,
    pub last_active_secs: u64,
    pub last_active_label: String,
    pub source_size_bytes: u64,
}

impl Project {
    pub fn is_dormant(&self, threshold_days: u64) -> bool {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let threshold_secs = threshold_days * 24 * 60 * 60;
        now.saturating_sub(self.last_active_secs) >= threshold_secs
    }
}

const CACHE_WHITELIST: &[&str] = &[
    "node_modules", ".next", ".nuxt", ".svelte-kit", ".turbo",
    ".parcel-cache", "dist", "build",
    "target",
    ".venv", "venv", "__pycache__", ".pytest_cache", ".ruff_cache",
    "vendor", ".gradle",
];

const PROJECT_MARKERS: &[&str] = &[
    "package.json", "Cargo.toml", "pyproject.toml", "go.mod",
    "composer.json", ".git",
];

fn is_cache_dir_name(name: &str) -> bool {
    CACHE_WHITELIST.iter().any(|w| name == *w)
}

fn detect_ecosystem(dir: &Path) -> Ecosystem {
    if dir.join("Cargo.toml").exists() {
        return Ecosystem::Rust;
    }
    if dir.join("package.json").exists() {
        return Ecosystem::Node;
    }
    if dir.join("pyproject.toml").exists() || dir.join("setup.py").exists() {
        return Ecosystem::Python;
    }
    if dir.join("go.mod").exists() {
        return Ecosystem::Go;
    }
    Ecosystem::Other
}

pub fn scan_workspace(root: &Path) -> Result<Vec<Project>> {
    let entries: Vec<PathBuf> = WalkDir::new(root)
        .min_depth(1)
        .max_depth(3)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_dir())
        .filter(|e| PROJECT_MARKERS.iter().any(|m| e.path().join(m).exists()))
        .map(|e| e.path().to_path_buf())
        .collect();

    let project_results: Vec<(PathBuf, Ecosystem, Vec<PathBuf>)> = entries
        .par_iter()
        .map(|dir| {
            let eco = detect_ecosystem(dir);
            let mut cache_paths = Vec::new();
            if let Ok(rd) = std::fs::read_dir(dir) {
                for entry in rd.flatten() {
                    let fname = entry.file_name().to_string_lossy().to_string();
                    if is_cache_dir_name(&fname) {
                        cache_paths.push(entry.path().to_path_buf());
                    }
                }
            }
            (dir.clone(), eco, cache_paths)
        })
        .collect();

    let sized: Vec<(PathBuf, Ecosystem, Vec<(PathBuf, u64)>)> = project_results
        .into_par_iter()
        .map(|(dir, eco, cache_paths)| {
            let sized: Vec<(PathBuf, u64)> = cache_paths
                .par_iter()
                .map(|p| (p.clone(), dir_size(p)))
                .collect();
            (dir, eco, sized)
        })
        .collect();

    let source_sizes: Vec<u64> = sized
        .par_iter()
        .map(|(dir, _eco, _cp)| {
            let mut total = 0u64;
            if let Ok(rd) = std::fs::read_dir(dir) {
                for entry in rd.flatten() {
                    if !is_cache_dir_name(&entry.file_name().to_string_lossy()) {
                        total += dir_size(&entry.path());
                    }
                }
            }
            total
        })
        .collect();

    let last_actives: Vec<u64> = sized
        .par_iter()
        .map(|(dir, _eco, cache_paths)| {
            let mut latest: Option<SystemTime> = None;
            if let Ok(rd) = std::fs::read_dir(dir) {
                for entry in rd.flatten() {
                    let fname = entry.file_name().to_string_lossy().to_string();
                    if is_cache_dir_name(&fname) {
                        continue;
                    }
                    if let Ok(meta) = entry.metadata() {
                        if let Ok(modified) = meta.modified() {
                            latest = Some(latest.unwrap_or(modified).max(modified));
                        }
                    }
                }
            }
            latest
                .map(|t| {
                    t.duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                })
                .unwrap_or(0)
        })
        .collect();

    let mut projects = Vec::new();
    let mut iter = sized.into_iter().zip(source_sizes.iter()).zip(last_actives.iter());
    while let Some((((dir, eco, cache_sizes), &source_size), &last_active_secs)) = iter.next() {
        let caches: Vec<DetectedCache> = cache_sizes
            .into_iter()
            .map(|(path, size)| DetectedCache {
                name: path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
                path,
                size_bytes: size,
            })
            .collect();
        let total_cache_bytes: u64 = caches.iter().map(|c| c.size_bytes).sum();
        let last_active_label = format_human_age(last_active_secs);
        let proj_name = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".into());
        projects.push(Project {
            name: proj_name,
            path: dir.clone(),
            ecosystem: eco,
            caches,
            total_cache_bytes,
            last_active_secs,
            last_active_label,
            source_size_bytes: source_size,
        });
    }

    Ok(projects)
}

fn dir_size(path: &Path) -> u64 {
    WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

fn format_human_age(last_active_secs: u64) -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let ago = now.saturating_sub(last_active_secs);
    if ago < 60 {
        format!("{}s ago", ago)
    } else if ago < 3600 {
        format!("{}m ago", ago / 60)
    } else if ago < 86400 {
        format!("{}h ago", ago / 3600)
    } else if ago < 86400 * 30 {
        format!("{}d ago", ago / 86400)
    } else if ago < 86400 * 365 {
        format!("{}mo ago", ago / (86400 * 30))
    } else {
        format!("{}y ago", ago / (86400 * 365))
    }
}

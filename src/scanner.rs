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

type SizedCache = (PathBuf, u64);
type SizedProjects = Vec<(PathBuf, Ecosystem, Vec<SizedCache>)>;

fn is_cache_dir_name(name: &str) -> bool {
    CACHE_WHITELIST.contains(&name)
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
        // Never look for projects inside a cache: packages under node_modules
        // or crates under target are not workspaces of their own. The walk root
        // is exempt so a workspace named like a cache still gets scanned.
        .filter_entry(|e| {
            e.depth() == 0
                || !(e.file_type().is_dir() && is_cache_dir_name(&e.file_name().to_string_lossy()))
        })
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

    let sized: SizedProjects = project_results
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
        .map(|(dir, _eco, _cache_paths)| {
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
    let combined = sized
        .into_iter()
        .zip(source_sizes.iter())
        .zip(last_actives.iter());
    for (((dir, eco, cache_sizes), &source_size), &last_active_secs) in combined {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "lemonwatch-scan-{}-{}-{}",
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

    fn write_bytes(path: &Path, len: usize) {
        fs::write(path, vec![b'x'; len]).expect("write fixture file");
    }

    fn find<'a>(projects: &'a [Project], name: &str) -> &'a Project {
        projects.iter().find(|p| p.name == name).unwrap_or_else(|| {
            panic!(
                "project '{name}' not found; found {:?}",
                projects.iter().map(|p| p.name.as_str()).collect::<Vec<_>>()
            )
        })
    }

    fn cache_names(project: &Project) -> Vec<&str> {
        project.caches.iter().map(|c| c.name.as_str()).collect()
    }

    fn sample_project(last_active_secs: u64) -> Project {
        Project {
            name: "sample".into(),
            path: PathBuf::from("sample"),
            ecosystem: Ecosystem::Other,
            caches: Vec::new(),
            total_cache_bytes: 0,
            last_active_secs,
            last_active_label: String::new(),
            source_size_bytes: 0,
        }
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    #[test]
    fn detects_node_project_and_its_caches() {
        let root = TempDir::new("node");
        let project_dir = root.path().join("webapp");
        fs::create_dir_all(project_dir.join("node_modules/left-pad")).unwrap();
        fs::create_dir_all(project_dir.join(".next")).unwrap();
        write_bytes(&project_dir.join("package.json"), 10);
        write_bytes(&project_dir.join("index.js"), 16);
        write_bytes(&project_dir.join("node_modules/left-pad/index.js"), 2048);
        write_bytes(&project_dir.join(".next/cache.bin"), 1024);

        let projects = scan_workspace(root.path()).expect("scan workspace");
        let project = find(&projects, "webapp");

        assert_eq!(project.ecosystem, Ecosystem::Node);
        let mut names = cache_names(project);
        names.sort_unstable();
        assert_eq!(names, vec![".next", "node_modules"]);
        assert!(
            project.total_cache_bytes >= 3072,
            "expected both caches to be sized, got {}",
            project.total_cache_bytes
        );
        assert_eq!(
            project.total_cache_bytes,
            project.caches.iter().map(|c| c.size_bytes).sum::<u64>(),
            "total must equal the sum of cache sizes"
        );
        assert!(
            project.source_size_bytes >= 26,
            "source bytes must exclude caches, got {}",
            project.source_size_bytes
        );
    }

    #[test]
    fn detects_rust_project_and_target_cache() {
        let root = TempDir::new("rust");
        let project_dir = root.path().join("core");
        fs::create_dir_all(project_dir.join("target/debug")).unwrap();
        fs::create_dir_all(project_dir.join("src")).unwrap();
        write_bytes(&project_dir.join("Cargo.toml"), 42);
        write_bytes(&project_dir.join("src/main.rs"), 8);
        write_bytes(&project_dir.join("target/debug/app"), 4096);

        let projects = scan_workspace(root.path()).expect("scan workspace");
        let project = find(&projects, "core");

        assert_eq!(project.ecosystem, Ecosystem::Rust);
        assert_eq!(cache_names(project), vec!["target"]);
        assert!(project.total_cache_bytes >= 4096);
        assert_eq!(project.source_size_bytes, 50);
    }

    #[test]
    fn detects_python_project_with_several_caches() {
        let root = TempDir::new("python");
        let project_dir = root.path().join("api");
        fs::create_dir_all(project_dir.join(".venv/lib")).unwrap();
        fs::create_dir_all(project_dir.join("__pycache__")).unwrap();
        write_bytes(&project_dir.join("pyproject.toml"), 12);
        write_bytes(&project_dir.join("app.py"), 20);
        write_bytes(&project_dir.join(".venv/lib/site.py"), 512);
        write_bytes(&project_dir.join("__pycache__/app.pyc"), 64);

        let projects = scan_workspace(root.path()).expect("scan workspace");
        let project = find(&projects, "api");

        assert_eq!(project.ecosystem, Ecosystem::Python);
        let mut names = cache_names(project);
        names.sort_unstable();
        assert_eq!(names, vec![".venv", "__pycache__"]);
    }

    #[test]
    fn ignores_directories_without_project_markers() {
        let root = TempDir::new("bare");
        let nested = root.path().join("scratch");
        fs::create_dir_all(nested.join("node_modules/pkg")).unwrap();
        write_bytes(&nested.join("node_modules/pkg/index.js"), 32);
        write_bytes(&nested.join("notes.txt"), 4);

        let projects = scan_workspace(root.path()).expect("scan workspace");
        assert!(
            projects.is_empty(),
            "marker-less directories must not be reported, got {:?}",
            projects.iter().map(|p| p.name.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn does_not_descend_into_cache_directories() {
        let root = TempDir::new("prune");
        let project_dir = root.path().join("webapp");
        let vendored_package = project_dir.join("node_modules/somepkg");
        fs::create_dir_all(&vendored_package).unwrap();
        write_bytes(&project_dir.join("package.json"), 2);
        write_bytes(&vendored_package.join("package.json"), 2);
        write_bytes(&vendored_package.join("index.js"), 1024);

        let projects = scan_workspace(root.path()).expect("scan workspace");
        let names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();

        assert_eq!(
            names,
            vec!["webapp"],
            "a package inside node_modules must not become a project"
        );
        let project = find(&projects, "webapp");
        assert_eq!(cache_names(project), vec!["node_modules"]);
        assert!(
            project.total_cache_bytes >= 1024,
            "pruning the walk must not stop the cache from being sized, got {}",
            project.total_cache_bytes
        );
    }

    #[test]
    fn still_finds_nested_projects_outside_caches() {
        let root = TempDir::new("nest");
        let monorepo = root.path().join("monorepo");
        let storefront = monorepo.join("apps/storefront");
        fs::create_dir_all(&storefront).unwrap();
        fs::create_dir_all(storefront.join("node_modules")).unwrap();
        write_bytes(&monorepo.join("package.json"), 2);
        write_bytes(&storefront.join("package.json"), 2);
        write_bytes(&storefront.join("node_modules/dep.js"), 8);

        let projects = scan_workspace(root.path()).expect("scan workspace");
        let mut names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
        names.sort_unstable();

        assert_eq!(names, vec!["monorepo", "storefront"]);
    }

    #[test]
    fn scans_a_workspace_root_named_like_a_cache() {
        let base = TempDir::new("rootname");
        let root = base.path().join("target");
        let project_dir = root.join("app");
        fs::create_dir_all(&project_dir).unwrap();
        write_bytes(&project_dir.join("package.json"), 2);

        let projects = scan_workspace(&root).expect("scan workspace");

        assert_eq!(projects.len(), 1, "the walk root itself must never be pruned");
        assert_eq!(projects[0].name, "app");
    }

    #[test]
    fn ignores_projects_deeper_than_the_walk_limit() {
        let root = TempDir::new("deep");
        let shallow = root.path().join("shallow");
        let deep = root.path().join("a/b/c/d");
        fs::create_dir_all(&shallow).unwrap();
        fs::create_dir_all(&deep).unwrap();
        write_bytes(&shallow.join("package.json"), 2);
        write_bytes(&deep.join("package.json"), 2);

        let projects = scan_workspace(root.path()).expect("scan workspace");
        let names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"shallow"), "shallow project missed: {names:?}");
        assert!(!names.contains(&"d"), "project beyond max_depth must be skipped: {names:?}");
    }

    #[test]
    fn dormancy_is_decided_by_last_active_timestamp() {
        let now = now_secs();
        let stale = sample_project(now.saturating_sub(31 * 86_400));
        let fresh = sample_project(now.saturating_sub(86_400));

        assert!(stale.is_dormant(30), "31 days idle must be dormant");
        assert!(!fresh.is_dormant(30), "1 day idle must not be dormant");
        assert!(!stale.is_dormant(60), "31 days idle is not dormant at 60 days");
    }

    #[test]
    fn human_age_labels_cover_each_bucket() {
        let now = now_secs();
        assert!(format_human_age(now).ends_with("s ago"));
        assert!(format_human_age(now.saturating_sub(120)).ends_with("m ago"));
        assert!(format_human_age(now.saturating_sub(2 * 3600)).ends_with("h ago"));
        assert!(format_human_age(now.saturating_sub(3 * 86_400)).ends_with("d ago"));
        assert!(format_human_age(now.saturating_sub(60 * 86_400)).ends_with("mo ago"));
    }
}

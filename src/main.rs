mod app;
mod cleaner;
mod scanner;
mod theme;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "lemon-watch", about = "🍋 LemonWatch — instant workspace cache scanner & purger", version)]
struct Cli {
    #[arg(name = "workspace", index = 1)]
    workspace: Option<String>,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    dry_run: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.json || cli.dry_run {
        run_headless(&cli)?;
    } else {
        run_gui(cli.workspace.as_deref());
    }
    Ok(())
}

fn run_headless(cli: &Cli) -> Result<()> {
    let root = match &cli.workspace {
        Some(p) => std::path::PathBuf::from(p),
        None => anyhow::bail!("No workspace path given. Pass a directory or use the GUI."),
    };
    if !root.is_dir() {
        anyhow::bail!("Workspace path is not a directory: {}", root.display());
    }
    let projects = scanner::scan_workspace(&root)?;
    if cli.json {
        let summary = serde_json::json!({
            "workspace": root.display().to_string(),
            "project_count": projects.len(),
            "total_reclaimable_gb": projects.iter().map(|p| p.total_cache_bytes as f64 / 1_073_741_824.0).sum::<f64>(),
            "projects": projects.iter().map(|p| {
                serde_json::json!({
                    "name": p.name, "path": p.path.display().to_string(),
                    "ecosystem": format!("{:?}", p.ecosystem).to_lowercase(),
                    "total_cache_gb": p.total_cache_bytes as f64 / 1_073_741_824.0,
                    "last_active": p.last_active_label,
                    "caches": p.caches.iter().map(|c| {
                        serde_json::json!({ "name": c.name, "size_gb": c.size_bytes as f64 / 1_073_741_824.0 })
                    }).collect::<Vec<_>>(),
                })
            }).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        let total: u64 = projects.iter().map(|p| p.total_cache_bytes).sum();
        println!("Workspace: {}", root.display());
        println!("Projects found: {}", projects.len());
        println!("Total reclaimable: {:.2} GB", total as f64 / 1_073_741_824.0);
        for p in &projects {
            println!("  - {} [{}] {:>8} GB  last active: {}", p.name, format!("{:?}", p.ecosystem).to_lowercase(), p.total_cache_bytes as f64 / 1_073_741_824.0, p.last_active_label);
            for c in &p.caches {
                println!("      {} {:>8} GB", c.name, c.size_bytes as f64 / 1_073_741_824.0);
            }
        }
    }
    Ok(())
}

fn run_gui(workspace: Option<&str>) {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_min_inner_size(egui::vec2(900.0, 600.0))
            .with_inner_size(egui::vec2(1100.0, 720.0)),
        ..Default::default()
    };
    let seed_path = workspace.and_then(|p| std::path::PathBuf::from(p).canonicalize().ok());
    let result = eframe::run_native(
        "LemonWatch",
        native_options,
        Box::new(move |_cc| {
            let app = app::LemonWatchApp::new(_cc);
            if let Some(ref root) = seed_path {
                app.state.lock().unwrap().workspace = Some(root.clone());
                let (tx, rx) = std::sync::mpsc::channel::<app::Project>();
                app.state.lock().unwrap().rx = Some(rx);
                let tx = tx;
                let root = root.clone();
                std::thread::spawn(move || {
                    if let Ok(projects) = scanner::scan_workspace(&root) {
                        for proj in projects {
                            let _ = tx.send(proj);
                        }
                    }
                    let _ = tx.send(app::Project {
                        name: "__done__".into(),
                        path: std::path::PathBuf::new(),
                        ecosystem: app::Ecosystem::Other,
                        caches: vec![],
                        total_cache_bytes: 0,
                        last_active_secs: 0,
                        last_active_label: String::new(),
                        source_size_bytes: 0,
                    });
                });
            }
            Ok(Box::new(app))
        }),
    );
    if let Err(e) = result {
        eprintln!("LemonWatch error: {}", e);
        std::process::exit(1);
    }
}

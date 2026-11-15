use crate::cleaner::{DeleteResult, purge_caches_background};
use crate::scanner;
use crate::theme;
pub use scanner::{Ecosystem, Project};

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, Context, Ui};
use open::that;

const DORMANCY_DAYS: u64 = 30;

#[derive(Default)]
pub struct FilterState {
    search: String,
    ecosystem: Option<Ecosystem>,
    sort: SortKey,
    sort_idx: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum SortKey {
    #[default]
    LargestSize,
    OldestActivity,
    ProjectName,
}

pub struct AppState {
    pub workspace: Option<PathBuf>,
    pub projects: Vec<Project>,
    pub selected: Arc<Mutex<Vec<usize>>>,
    pub filter: FilterState,
    pub scan_active: bool,
    pub total_reclaimable_gb: f64,
    pub dormant_count: usize,
    pub dormant_bytes: u64,
    pub space_freed_gb: f64,
    pub pending_purge: Option<Vec<(String, PathBuf, u64)>>,
    pub purge_total_gb: f64,
    pub purge_active: bool,
    pub purge_done: usize,
    pub purge_total: usize,
    pub purge_error: Option<String>,
    pub rx: Option<mpsc::Receiver<Project>>,
    pub purge_rx: Option<mpsc::Receiver<DeleteResult>>,
    pub purge_handle: Option<std::thread::JoinHandle<()>>,
    pub toasts: Arc<Mutex<Vec<Toast>>>,
}

#[derive(Clone)]
pub struct Toast {
    text: String,
    color: Color32,
    born: std::time::Instant,
    ttl_ms: u64,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            workspace: None,
            projects: Vec::new(),
            selected: Arc::new(Mutex::new(Vec::new())),
            filter: FilterState {
                sort_idx: 0,
                ..Default::default()
            },
            scan_active: false,
            total_reclaimable_gb: 0.0,
            dormant_count: 0,
            dormant_bytes: 0,
            space_freed_gb: 0.0,
            pending_purge: None,
            purge_total_gb: 0.0,
            purge_active: false,
            purge_done: 0,
            purge_total: 0,
            purge_error: None,
            rx: None,
            purge_rx: None,
            purge_handle: None,
            toasts: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

pub struct LemonWatchApp {
    pub state: Arc<Mutex<AppState>>,
}

impl LemonWatchApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // LemonWatch ships a dark-only palette, so pin the theme instead of
        // following the desktop preference and paint both style slots with it.
        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
        cc.egui_ctx.all_styles_mut(theme::apply_theme);
        LemonWatchApp {
            state: Arc::new(Mutex::new(AppState::default())),
        }
    }
}

impl eframe::App for LemonWatchApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // Drain scanner channel
        {
            let mut st = self.state.lock().unwrap();
            let rx = st.rx.take();
            drop(st);
            if let Some(rx) = rx {
                while let Ok(proj) = rx.try_recv() {
                    if proj.name == "__done__" {
                        break;
                    }
                    self.state.lock().unwrap().projects.push(proj);
                }
            }
        }
        // Drain purge channel
        {
            let mut st = self.state.lock().unwrap();
            let rx = st.purge_rx.take();
            drop(st);
            if let Some(rx) = rx {
                while let Ok(r) = rx.try_recv() {
                    if r.cache_name == "__done__" {
                        self.state.lock().unwrap().purge_active = false;
                        if let Some(h) = self.state.lock().unwrap().purge_handle.take() {
                            let _ = h.join();
                        }
                        if self.state.lock().unwrap().purge_error.is_none() {
                            let total = self.state.lock().unwrap().purge_total_gb;
                            self.state.lock().unwrap().space_freed_gb += total;
                            self.state.lock().unwrap().toasts.lock().unwrap().push(Toast {
                                text: format!("Freed {:.2} GB!", total),
                                color: theme::LIME,
                                born: std::time::Instant::now(),
                                ttl_ms: 4000,
                            });
                        }
                        break;
                    } else if r.success {
                        self.state.lock().unwrap().purge_done += 1;
                        self.state.lock().unwrap().space_freed_gb +=
                            r.size_bytes as f64 / 1_073_741_824.0;
                    } else {
                        self.state.lock().unwrap().purge_error = r.reason.clone();
                        self.state.lock().unwrap().purge_active = false;
                        if let Some(h) = self.state.lock().unwrap().purge_handle.take() {
                            let _ = h.join();
                        }
                        self.state.lock().unwrap().toasts.lock().unwrap().push(Toast {
                            text: format!("Delete failed: {}", r.reason.unwrap_or_default()),
                            color: theme::CORAL,
                            born: std::time::Instant::now(),
                            ttl_ms: 5000,
                        });
                        break;
                    }
                }
            }
        }
        // Update KPIs
        {
            let mut st = self.state.lock().unwrap();
            let total_reclaimable: u64 = st.projects.iter().map(|p| p.total_cache_bytes).sum();
            st.total_reclaimable_gb = total_reclaimable as f64 / 1_073_741_824.0;
            let dormant_count = st.projects.iter().filter(|p| p.is_dormant(30)).count();
            let dormant_bytes: u64 =
                st.projects.iter().filter(|p| p.is_dormant(30)).map(|p| p.total_cache_bytes).sum();
            st.dormant_count = dormant_count;
            st.dormant_bytes = dormant_bytes;
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            let mut st = self.state.lock().unwrap();
            render_header(ui, &mut st, ctx);
            render_kpis(ui, &st);
            render_filter_toolbar(ui, &mut st);
            render_project_list(ui, &mut st);
            render_toasts(ui, &mut st);
        });
        {
            let mut st = self.state.lock().unwrap();
            render_modal(ctx, &mut st);
        }
        ctx.request_repaint();
    }
}

fn render_header(ui: &mut Ui, state: &mut AppState, ctx: &Context) {
    ui.horizontal(|ui| {
        ui.heading("LemonWatch");
        ui.add_space(12.0);
        ui.label(egui::RichText::new("Workspace").small().monospace().color(theme::TEXT_MUTED));
        if let Some(ref ws) = state.workspace {
            ui.label(egui::RichText::new(ws.display().to_string()).small().monospace());
        } else {
            ui.label(egui::RichText::new("No workspace").small().monospace().color(theme::CORAL));
        }
        ui.horizontal(|ui| {
            ui.add_space((ui.available_width() - 160.0).max(0.0));
            if ui.button("Change Folder").clicked() {
                pick_workspace(state, ctx);
            }
            ui.add_space(4.0);
            if ui.button("Rescan").clicked() {
                if let Some(ref ws) = state.workspace {
                    start_scan(state, ws.clone(), ctx);
                }
            }
        });
    });
    ui.add_space(6.0);
    ui.add(egui::Separator::default());
    ui.add_space(10.0);
}

fn render_kpis(ui: &mut Ui, state: &AppState) {
    ui.horizontal(|ui| {
        kpi_card(
            ui,
            "Total Reclaimable",
            format_gb(state.total_reclaimable_gb),
            theme::LEMON,
        );
        kpi_card(
            ui,
            "Dormant Projects (>30d)",
            format!(
                "{} projects - {}",
                state.dormant_count,
                format_gb(state.dormant_bytes as f64 / 1_073_741_824.0)
            ),
            theme::CORAL,
        );
        kpi_card(
            ui,
            "Space Freed This Session",
            format_gb(state.space_freed_gb),
            theme::LIME,
        );
    });
    ui.add_space(12.0);
}

fn kpi_card(ui: &mut Ui, label: &str, value: String, accent: Color32) {
    ui.vertical(|ui| {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(label).small().monospace().color(theme::TEXT_MUTED));
        ui.label(egui::RichText::new(value).strong().size(18.0).color(accent));
        ui.add_space(4.0);
    });
    ui.add_space(8.0);
}

fn render_filter_toolbar(ui: &mut Ui, state: &mut AppState) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Search:").small().color(theme::TEXT_MUTED));
        ui.add(egui::TextEdit::singleline(&mut state.filter.search).hint_text("Filter projects..."));
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            for eco in [
                None,
                Some(Ecosystem::Node),
                Some(Ecosystem::Rust),
                Some(Ecosystem::Python),
                Some(Ecosystem::Other),
            ] {
                let label = match eco {
                    None => "All",
                    Some(Ecosystem::Node) => "Node",
                    Some(Ecosystem::Rust) => "Rust",
                    Some(Ecosystem::Python) => "Python",
                    Some(Ecosystem::Other) => "Other",
                    _ => unreachable!(),
                };
                let active = state.filter.ecosystem == eco;
                let btn = egui::Button::new(egui::RichText::new(label).small().monospace())
                    .fill(if active { theme::BG_CARD } else { theme::BG_DARK })
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        if active { theme::LEMON } else { theme::BORDER_SUBTLE },
                    ));
                if ui.add(btn).clicked() {
                    if active {
                        state.filter.ecosystem = None;
                    } else {
                        state.filter.ecosystem = eco;
                    }
                }
            }
        });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Sort:").small().color(theme::TEXT_MUTED));
            let current_sort = state.filter.sort;
            let current_text = match current_sort {
                SortKey::LargestSize => "Largest Size",
                SortKey::OldestActivity => "Oldest Activity",
                SortKey::ProjectName => "Project Name",
            };
            egui::ComboBox::from_id_salt("sort_combo")
                .selected_text(current_text)
                .show_index(ui, &mut state.filter.sort_idx, 3, |idx| {
                    let _new_sort: SortKey = match idx {
                        0 => SortKey::LargestSize,
                        1 => SortKey::OldestActivity,
                        _ => SortKey::ProjectName,
                    };
                    ""
                });
            state.filter.sort = match state.filter.sort_idx {
                0 => SortKey::LargestSize,
                1 => SortKey::OldestActivity,
                _ => SortKey::ProjectName,
            };
        });
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("Select Dormant (>30d)").clicked() {
            let idxs: Vec<usize> = state
                .projects
                .iter()
                .enumerate()
                .filter(|(_, p)| p.is_dormant(DORMANCY_DAYS))
                .map(|(i, _)| i)
                .collect();
            *state.selected.lock().unwrap() = idxs;
        }
        if ui.button("Select All").clicked() {
            *state.selected.lock().unwrap() = (0..state.projects.len()).collect();
        }
        if ui.button("Clear Selection").clicked() {
            *state.selected.lock().unwrap() = Vec::new();
        }
        ui.horizontal(|ui| {
            ui.add_space((ui.available_width() - 160.0).max(0.0));
            let sel_len = state.selected.lock().unwrap().len();
            let total_sel_bytes: u64 = state
                .selected
                .lock()
                .unwrap()
                .iter()
                .filter_map(|&i| state.projects.get(i).map(|p| p.total_cache_bytes))
                .sum();
            let gb = total_sel_bytes as f64 / 1_073_741_824.0;
            let alert = if sel_len > 0 { theme::CORAL } else { theme::TEXT_MUTED };
            if ui
                .button(egui::RichText::new(format!("Purge Selected ({:.2} GB)", gb)).strong())
                .clicked()
            {
                open_purge_modal(state);
            }
            if sel_len > 0 {
                ui.label(egui::RichText::new(format!("{} selected", sel_len)).small().color(alert));
            }
        });
    });
    ui.add_space(8.0);
    ui.add(egui::Separator::default());
    ui.add_space(8.0);
}

fn render_project_list(ui: &mut Ui, state: &mut AppState) {
    // Build snapshot to avoid borrow conflicts
    let list_snapshot: Vec<(usize, String, Ecosystem, String, String, u64, PathBuf)> =
        state.projects.iter().enumerate().map(|(i, p)| {
            (
                i,
                p.name.clone(),
                p.ecosystem.clone(),
                p.last_active_label.clone(),
                format_gb(p.total_cache_bytes as f64 / 1_073_741_824.0),
                p.total_cache_bytes,
                p.path.clone(),
            )
        }).collect();

    let filtered: Vec<_> = if state.filter.search.is_empty() {
        list_snapshot
    } else {
        list_snapshot
            .into_iter()
            .filter(|(_, name, _, _, _, _, _)| {
                name.to_lowercase().contains(&state.filter.search.to_lowercase())
            })
            .collect()
    };

    let sorted: Vec<_> = match state.filter.sort {
        SortKey::LargestSize => {
            let mut v = filtered;
            v.sort_by_key(|p| std::cmp::Reverse(p.5));
            v
        }
        SortKey::OldestActivity => filtered,
        SortKey::ProjectName => {
            let mut v = filtered;
            v.sort_by_key(|p| p.1.clone());
            v
        }
    };

    if sorted.is_empty() && state.projects.is_empty() {
        ui.label(egui::RichText::new("No projects found. Choose a workspace and rescan.").monospace().color(theme::TEXT_MUTED));
    } else if state.projects.is_empty() && state.scan_active {
        ui.label(egui::RichText::new("Scanning workspace...").monospace().color(theme::LEMON));
    } else {
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (real_idx, name, eco, last_active, size_str, _, path) in sorted.iter() {
                let prev = state.selected.lock().unwrap().contains(real_idx);
                let mut is_sel = prev;
                ui.horizontal(|ui| {
                    ui.add(egui::Checkbox::new(&mut is_sel, ""));
                    if is_sel != prev {
                        let mut sel = state.selected.lock().unwrap();
                        if is_sel {
                            if !sel.contains(real_idx) {
                                sel.push(*real_idx);
                            }
                        } else {
                            sel.retain(|&i| i != *real_idx);
                        }
                    }
                    ui.label(egui::RichText::new(name.clone()).strong().monospace());
                    ui.label(ecosystem_badge(eco.clone()));
                    ui.label(egui::RichText::new(last_active.clone()).small().monospace().color(
                        last_active_color_from_str(last_active),
                    ));
                    ui.label(egui::RichText::new(size_str.clone()).small().monospace().color(
                        theme::LEMON,
                    ));
                    ui.add_space(10.0);
                    if ui.button("Clean").clicked() {
                        quick_clean(state, *real_idx);
                    }
                    ui.add_space(4.0);
                    if ui.button("Open").clicked() {
                        if let Err(e) = that(path) {
                            state.toasts.lock().unwrap().push(Toast {
                                text: format!("Could not open folder: {}", e),
                                color: theme::CORAL,
                                born: std::time::Instant::now(),
                                ttl_ms: 4000,
                            });
                        }
                    }
                });
                ui.add(egui::Separator::default());
            }
        });
    }
}

fn ecosystem_badge(eco: Ecosystem) -> egui::RichText {
    let (label, color) = match eco {
        Ecosystem::Node => ("Node", theme::LEMON),
        Ecosystem::Rust => ("Rust", Color32::from_rgb(239, 68, 68)),
        Ecosystem::Python => ("Python", Color32::from_rgb(59, 130, 246)),
        Ecosystem::Go => ("Go", Color32::from_rgb(245, 158, 11)),
        _ => ("Other", theme::TEXT_MUTED),
    };
    egui::RichText::new(format!("[{}]", label)).small().color(color).strong()
}

fn last_active_color_from_str(s: &str) -> Color32 {
    if s.contains("d ago") && s.parse::<u64>().unwrap_or(0) > 30 {
        theme::CORAL
    } else {
        theme::TEXT_MUTED
    }
}

fn quick_clean(state: &mut AppState, idx: usize) {
    if idx >= state.projects.len() {
        return;
    }
    let proj = &state.projects[idx];
    if proj.caches.is_empty() {
        return;
    }
    let paths: Vec<PathBuf> = proj.caches.iter().map(|c| c.path.clone()).collect();
    let total_bytes: u64 = paths.iter().filter_map(|p| walk_dir_size(p)).sum();
    let (tx, rx) = mpsc::channel::<DeleteResult>();
    state.purge_rx = Some(rx);
    state.purge_handle = Some(purge_caches_background(&paths, tx));
    state.purge_total_gb = total_bytes as f64 / 1_073_741_824.0;
    state.purge_active = true;
    state.purge_done = 0;
    state.purge_total = paths.len();
    state.purge_error = None;
}

fn open_purge_modal(state: &mut AppState) {
    let sel = state.selected.lock().unwrap();
    let mut list = Vec::new();
    let mut total: u64 = 0;
    for &idx in sel.iter() {
        if let Some(proj) = state.projects.get(idx) {
            for cache in &proj.caches {
                list.push((cache.name.clone(), cache.path.clone(), cache.size_bytes));
                total += cache.size_bytes;
            }
        }
    }
    drop(sel);
    if list.is_empty() {
        return;
    }
    state.purge_total = list.len();
    state.purge_total_gb = total as f64 / 1_073_741_824.0;
    state.purge_active = true;
    state.purge_done = 0;
    state.purge_error = None;
    state.pending_purge = Some(list);
}

fn render_modal(ctx: &Context, state: &mut AppState) {
    if state.pending_purge.is_none() {
        return;
    }
    let total: f64 = state.pending_purge.as_ref().unwrap().iter().map(|(_, _, s)| *s).sum::<u64>() as f64 / 1_073_741_824.0;
    let list_len = state.pending_purge.as_ref().unwrap().len();
    egui::Window::new("purge-confirm")
        .min_width(360.0)
        .show(ctx, |ui| {
            ui.heading("Confirm Cache Purge");
            ui.label(egui::RichText::new(format!(
                "You are about to permanently delete {} cache folders ({:.2} GB). This cannot be undone.",
                list_len,
                total
            )));
            ui.add_space(8.0);
            for (name, path, size) in state.pending_purge.as_ref().unwrap().iter() {
                let gb = *size as f64 / 1_073_741_824.0;
                ui.label(egui::RichText::new(format!(
                    "- {} ({:.2} GB) - {}",
                    name,
                    gb,
                    path.display()
                ))
                .small()
                .monospace()
                .color(theme::TEXT_MUTED));
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    state.pending_purge = None;
                    state.purge_active = false;
                    if let Some(h) = state.purge_handle.take() {
                        let _ = h.join();
                    }
                    return;
                }
                ui.add_space(8.0);
                if ui
                    .button(egui::RichText::new("Confirm Purge").strong().color(theme::TEXT_PRIMARY))
                    .clicked()
                {
                    let list = state.pending_purge.take().unwrap();
                    let paths: Vec<PathBuf> = list.iter().map(|(_, p, _)| p.clone()).collect();
                    let total_bytes: u64 = list.iter().map(|(_, _, s)| *s).sum();
                    let (tx, rx) = mpsc::channel::<DeleteResult>();
                    state.purge_rx = Some(rx);
                    state.purge_handle = Some(purge_caches_background(&paths, tx));
                    state.purge_total_gb = total_bytes as f64 / 1_073_741_824.0;
                    state.purge_active = true;
                    state.purge_done = 0;
                    state.purge_total = paths.len();
                    state.purge_error = None;
                }
            });
        });
        if state.purge_active && state.purge_total > 0 {
            let progress = state.purge_done as f32 / state.purge_total as f32;
            egui::Window::new("purge-progress").show(ctx, |ui| {
                ui.heading("Purging Caches...");
                ui.label(egui::RichText::new(format!(
                    "Freed {:.2} GB so far ({} / {} folders)",
                    state.space_freed_gb, state.purge_done, state.purge_total
                ))
                .small()
                .monospace()
                .color(theme::LIME));
                ui.add_space(8.0);
                ui.add(egui::ProgressBar::new(progress).fill(theme::LEMON).show_percentage());
                if let Some(err) = &state.purge_error {
                    ui.label(egui::RichText::new(format!("Error: {}", err)).small().monospace().color(theme::CORAL));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        state.purge_active = false;
                        state.pending_purge = None;
                        if let Some(h) = state.purge_handle.take() {
                            let _ = h.join();
                        }
                    }
                });
            });
        }
    }

fn render_toasts(ui: &mut Ui, state: &mut AppState) {
    let now = std::time::Instant::now();
    let mut toasts = state.toasts.lock().unwrap();
    let mut remaining = Vec::new();
    for t in toasts.drain(..) {
        if now.duration_since(t.born).as_millis() < (t.ttl_ms as u128 * 1000) {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&t.text).small().monospace().color(t.color));
                ui.add_space(4.0);
                let _ = ui.small_button(egui::RichText::new("x").color(theme::TEXT_MUTED));
            });
            ui.add_space(4.0);
            remaining.push(t);
        }
    }
    *toasts = remaining;
}

fn pick_workspace(state: &mut AppState, ctx: &Context) {
    if let Some(path) = rfd::FileDialog::new().pick_folder() {
        state.workspace = Some(path.clone());
        start_scan(state, path, ctx);
    }
}

fn start_scan(state: &mut AppState, root: PathBuf, ctx: &Context) {
    state.projects.clear();
    state.scan_active = true;
    state.total_reclaimable_gb = 0.0;
    state.dormant_count = 0;
    state.dormant_bytes = 0;
    *state.selected.lock().unwrap() = Vec::new();
    let (tx, rx) = mpsc::channel::<Project>();
    state.rx = Some(rx);
    std::thread::spawn(move || {
        if let Ok(projects) = scanner::scan_workspace(&root) {
            for p in projects {
                let _ = tx.send(p);
            }
        }
        let _ = tx.send(Project {
            name: "__done__".into(),
            path: PathBuf::new(),
            ecosystem: Ecosystem::Other,
            caches: Vec::new(),
            total_cache_bytes: 0,
            last_active_secs: 0,
            last_active_label: String::new(),
            source_size_bytes: 0,
        });
    });
    ctx.request_repaint();
}

fn walk_dir_size(path: &std::path::Path) -> Option<u64> {
    let mut total = 0u64;
    for entry in walkdir::WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                total += meta.len();
            }
        }
    }
    Some(total)
}

fn format_gb(gb: f64) -> String {
    if gb < 0.01 {
        format!("{:.2} MB", gb * 1024.0)
    } else {
        format!("{:.2} GB", gb)
    }
}

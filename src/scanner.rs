//! High-performance parallel disk scanner with topology building
//!
//! This module provides the core scanning algorithm that can be integrated
//! into any Rust application, including Tauri apps.

use dashmap::DashMap;
use parking_lot::Mutex;
use rayon::prelude::*;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Progress tracking for live updates (thread-safe)
#[derive(Default)]
pub struct ScanProgress {
    pub dirs_scanned: AtomicU64,
    pub files_found: AtomicU64,
    pub bytes_found: AtomicU64,
    pub errors: AtomicU64,
    pub running: AtomicBool,
    pub current_path: Mutex<String>,
}

impl ScanProgress {
    pub fn new() -> Self {
        Self {
            dirs_scanned: AtomicU64::new(0),
            files_found: AtomicU64::new(0),
            bytes_found: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            running: AtomicBool::new(true),
            current_path: Mutex::new(String::new()),
        }
    }

    pub fn snapshot(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            dirs_scanned: self.dirs_scanned.load(Ordering::Relaxed),
            files_found: self.files_found.load(Ordering::Relaxed),
            bytes_found: self.bytes_found.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            current_path: self.current_path.lock().clone(),
        }
    }
}

/// A snapshot of progress at a point in time (safe to send to frontend)
#[derive(Clone, Debug, serde::Serialize)]
pub struct ProgressSnapshot {
    pub dirs_scanned: u64,
    pub files_found: u64,
    pub bytes_found: u64,
    pub errors: u64,
    pub current_path: String,
}

/// A node in the disk topology tree
#[derive(Debug, Clone, serde::Serialize)]
pub struct TopologyNode {
    pub name: String,
    #[serde(skip)]
    pub path: PathBuf,
    pub size: u64,
    pub size_human: String,
    pub is_dir: bool,
    pub children: Vec<TopologyNode>,
    pub file_count: u64,
    pub dir_count: u64,
    pub is_aggregated: bool,
    #[serde(skip_serializing_if = "is_zero")]
    pub aggregated_count: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl TopologyNode {
    pub fn new_file(name: String, path: PathBuf, size: u64) -> Self {
        Self {
            name,
            path,
            size,
            size_human: format_size(size),
            is_dir: false,
            children: Vec::new(),
            file_count: 1,
            dir_count: 0,
            is_aggregated: false,
            aggregated_count: 0,
        }
    }

    pub fn new_dir(name: String, path: PathBuf) -> Self {
        Self {
            name,
            path,
            size: 0,
            size_human: String::new(),
            is_dir: true,
            children: Vec::new(),
            file_count: 0,
            dir_count: 1,
            is_aggregated: false,
            aggregated_count: 0,
        }
    }

    pub fn new_aggregated(name: String, size: u64, count: u64, file_count: u64, dir_count: u64) -> Self {
        Self {
            name,
            path: PathBuf::new(),
            size,
            size_human: format_size(size),
            is_dir: false,
            children: Vec::new(),
            file_count,
            dir_count,
            is_aggregated: true,
            aggregated_count: count,
        }
    }

    /// Recursively calculate total size
    pub fn calculate_size(&mut self) -> u64 {
        if self.is_dir && !self.is_aggregated {
            self.size = self.children.iter_mut().map(|c| c.calculate_size()).sum();
        }
        self.size_human = format_size(self.size);
        self.size
    }

    /// Recursively count files and dirs
    pub fn calculate_counts(&mut self) -> (u64, u64) {
        if self.is_dir && !self.is_aggregated {
            let mut total_files = 0u64;
            let mut total_dirs = 1u64;
            for child in &mut self.children {
                let (f, d) = child.calculate_counts();
                total_files += f;
                total_dirs += d;
            }
            self.file_count = total_files;
            self.dir_count = total_dirs;
        }
        (self.file_count, self.dir_count)
    }

    /// Apply threshold-based aggregation: collapse items below size threshold
    pub fn apply_threshold(&mut self, threshold: u64) {
        if !self.is_dir || self.is_aggregated {
            return;
        }

        for child in &mut self.children {
            child.apply_threshold(threshold);
        }

        let (large, small): (Vec<_>, Vec<_>) = self.children
            .drain(..)
            .partition(|c| c.size >= threshold);

        if !small.is_empty() {
            let other_size: u64 = small.iter().map(|c| c.size).sum();
            let other_files: u64 = small.iter().map(|c| c.file_count).sum();
            let other_dirs: u64 = small.iter().map(|c| c.dir_count).sum();
            let other_count = small.len() as u64;

            let other = TopologyNode::new_aggregated(
                format!("[{} items below {}]", other_count, format_size(threshold)),
                other_size,
                other_count,
                other_files,
                other_dirs,
            );

            self.children = large;
            self.children.push(other);
        } else {
            self.children = large;
        }

        self.children.sort_by(|a, b| b.size.cmp(&a.size));
    }

    /// Apply coverage-based aggregation: keep items until coverage_pct is reached
    /// Smart pruning: only recurse into items > min_pct of root
    pub fn apply_coverage(&mut self, coverage_pct: f64) {
        self.apply_coverage_recursive(coverage_pct, 15, self.size, 1.0);
    }

    fn apply_coverage_recursive(&mut self, coverage_pct: f64, max_items: usize, root_size: u64, min_pct: f64) {
        if !self.is_dir || self.is_aggregated || self.size == 0 {
            return;
        }

        self.children.sort_by(|a, b| b.size.cmp(&a.size));

        let min_size = (root_size as f64 * min_pct / 100.0) as u64;
        for child in &mut self.children {
            if child.size >= min_size {
                child.apply_coverage_recursive(coverage_pct, max_items, root_size, min_pct);
            } else if child.is_dir && !child.children.is_empty() {
                let total_files: u64 = child.children.iter().map(|c| c.file_count).sum();
                let total_dirs: u64 = child.children.iter().map(|c| c.dir_count).sum();
                child.file_count = total_files;
                child.dir_count = total_dirs + 1;
                child.children.clear();
            }
        }

        let target = (self.size as f64 * coverage_pct / 100.0) as u64;
        let mut covered: u64 = 0;
        let mut keep_count = 0;

        for child in &self.children {
            if covered >= target || keep_count >= max_items {
                break;
            }
            covered += child.size;
            keep_count += 1;
        }

        keep_count = keep_count.max(1);

        if keep_count < self.children.len() {
            let (keep, aggregate): (Vec<_>, Vec<_>) = self.children
                .drain(..)
                .enumerate()
                .partition(|(i, _)| *i < keep_count);

            let keep: Vec<_> = keep.into_iter().map(|(_, c)| c).collect();
            let aggregate: Vec<_> = aggregate.into_iter().map(|(_, c)| c).collect();

            if !aggregate.is_empty() {
                let other_size: u64 = aggregate.iter().map(|c| c.size).sum();
                let other_files: u64 = aggregate.iter().map(|c| c.file_count).sum();
                let other_dirs: u64 = aggregate.iter().map(|c| c.dir_count).sum();
                let other_count = aggregate.len() as u64;

                let pct_remaining = if self.size > 0 {
                    (other_size as f64 / self.size as f64) * 100.0
                } else {
                    0.0
                };

                let other = TopologyNode::new_aggregated(
                    format!("[{} more, {:.1}%]", other_count, pct_remaining),
                    other_size,
                    other_count,
                    other_files,
                    other_dirs,
                );

                self.children = keep;
                self.children.push(other);
            } else {
                self.children = keep;
            }
        }
    }

    /// Limit depth of tree
    pub fn limit_depth(&mut self, max_depth: usize, current_depth: usize) {
        if !self.is_dir || self.is_aggregated {
            return;
        }

        if current_depth >= max_depth && !self.children.is_empty() {
            let total_size: u64 = self.children.iter().map(|c| c.size).sum();
            let total_files: u64 = self.children.iter().map(|c| c.file_count).sum();
            let total_dirs: u64 = self.children.iter().map(|c| c.dir_count).sum();
            let count = self.children.len() as u64;

            self.children = vec![TopologyNode::new_aggregated(
                format!("[{} items, depth limit]", count),
                total_size,
                count,
                total_files,
                total_dirs,
            )];
        } else {
            for child in &mut self.children {
                child.limit_depth(max_depth, current_depth + 1);
            }
        }
    }
}

/// Shared state for parallel scanning
struct ScanState {
    nodes: DashMap<PathBuf, TopologyNode>,
    parent_map: DashMap<PathBuf, PathBuf>,
    progress: Arc<ScanProgress>,
}

/// Scan configuration
#[derive(Clone)]
pub struct ScanConfig {
    /// Number of worker threads (default: 12)
    pub num_workers: usize,
    /// Coverage percentage for aggregation (default: 97.0)
    pub coverage_pct: Option<f64>,
    /// Size threshold for aggregation (alternative to coverage)
    pub threshold: Option<u64>,
    /// Maximum depth (0 = unlimited)
    pub max_depth: usize,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            num_workers: 12,
            coverage_pct: Some(97.0),
            threshold: None,
            max_depth: 0,
        }
    }
}

/// Scan result
#[derive(Debug, serde::Serialize)]
pub struct ScanResult {
    pub root: TopologyNode,
    pub scan_time_secs: f64,
    pub total_size: u64,
    pub total_files: u64,
    pub total_dirs: u64,
    pub errors: u64,
    pub throughput_gbps: f64,
}

/// Disk information from statvfs
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiskInfo {
    pub total_capacity: u64,
    pub used: u64,
    pub available: u64,
    pub total_human: String,
    pub used_human: String,
    pub available_human: String,
    pub usage_pct: f64,
}

/// Get disk info for a path
#[cfg(unix)]
pub fn get_disk_info(path: &PathBuf) -> Option<DiskInfo> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;

    let path_str = path.to_str()?;
    let c_path = CString::new(path_str).ok()?;

    unsafe {
        let mut stat: MaybeUninit<libc::statvfs> = MaybeUninit::uninit();
        if libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) == 0 {
            let stat = stat.assume_init();
            let block_size = stat.f_frsize as u64;
            let total = stat.f_blocks as u64 * block_size;
            let available = stat.f_bavail as u64 * block_size;
            let free = stat.f_bfree as u64 * block_size;
            let used = total - free;
            let usage_pct = (used as f64 / total as f64) * 100.0;

            Some(DiskInfo {
                total_capacity: total,
                used,
                available,
                total_human: format_size(total),
                used_human: format_size(used),
                available_human: format_size(available),
                usage_pct,
            })
        } else {
            None
        }
    }
}

#[cfg(not(unix))]
pub fn get_disk_info(_path: &PathBuf) -> Option<DiskInfo> {
    None
}

/// Main scanner struct
pub struct DiskScanner {
    config: ScanConfig,
    progress: Arc<ScanProgress>,
}

impl DiskScanner {
    pub fn new(config: ScanConfig) -> Self {
        Self {
            config,
            progress: Arc::new(ScanProgress::new()),
        }
    }

    /// Get a reference to progress for monitoring
    pub fn progress(&self) -> Arc<ScanProgress> {
        Arc::clone(&self.progress)
    }

    /// Perform the scan
    pub fn scan(&self, root: PathBuf) -> ScanResult {
        let start = Instant::now();

        // Reset progress
        self.progress.dirs_scanned.store(0, Ordering::Relaxed);
        self.progress.files_found.store(0, Ordering::Relaxed);
        self.progress.bytes_found.store(0, Ordering::Relaxed);
        self.progress.errors.store(0, Ordering::Relaxed);
        self.progress.running.store(true, Ordering::Relaxed);

        let state = ScanState {
            nodes: DashMap::new(),
            parent_map: DashMap::new(),
            progress: Arc::clone(&self.progress),
        };

        // Configure thread pool
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.config.num_workers)
            .build()
            .expect("Failed to build thread pool");

        let root_clone = root.clone();
        pool.install(|| {
            parallel_scan(root_clone, &state);
        });

        self.progress.running.store(false, Ordering::Relaxed);

        let scan_time = start.elapsed().as_secs_f64();

        // Build tree
        let mut tree = build_tree(&state, &root);
        tree.calculate_size();
        tree.calculate_counts();

        // Apply aggregation
        if let Some(pct) = self.config.coverage_pct {
            tree.apply_coverage(pct);
        } else if let Some(threshold) = self.config.threshold {
            tree.apply_threshold(threshold);
        }

        // Limit depth if specified
        if self.config.max_depth > 0 {
            tree.limit_depth(self.config.max_depth, 0);
        }

        let total_size = tree.size;
        let total_files = tree.file_count;
        let total_dirs = tree.dir_count;
        let errors = self.progress.errors.load(Ordering::Relaxed);
        let throughput_gbps = (total_size as f64 / (1024.0 * 1024.0 * 1024.0)) / scan_time;

        ScanResult {
            root: tree,
            scan_time_secs: scan_time,
            total_size,
            total_files,
            total_dirs,
            errors,
            throughput_gbps,
        }
    }
}

/// Scan a directory and return subdirectories to process
fn scan_directory(path: &PathBuf, state: &ScanState) -> Vec<PathBuf> {
    let mut subdirs = Vec::new();
    let mut files = Vec::new();
    let mut dir_bytes: u64 = 0;
    let mut dir_files: u64 = 0;

    {
        let mut current = state.progress.current_path.lock();
        *current = path.to_string_lossy().to_string();
    }

    let dir_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());

    match fs::read_dir(path) {
        Ok(entries) => {
            for entry in entries.flatten() {
                if let Ok(metadata) = entry.metadata() {
                    let entry_path = entry.path();
                    let entry_name = entry.file_name().to_string_lossy().to_string();

                    if metadata.is_file() {
                        let size = metadata.len();
                        files.push(TopologyNode::new_file(entry_name, entry_path, size));
                        dir_bytes += size;
                        dir_files += 1;
                    } else if metadata.is_dir() {
                        subdirs.push(entry_path.clone());
                        state.parent_map.insert(entry_path, path.clone());
                    }
                }
            }
        }
        Err(_) => {
            state.progress.errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    state.progress.dirs_scanned.fetch_add(1, Ordering::Relaxed);
    state.progress.files_found.fetch_add(dir_files, Ordering::Relaxed);
    state.progress.bytes_found.fetch_add(dir_bytes, Ordering::Relaxed);

    let mut node = TopologyNode::new_dir(dir_name, path.clone());
    node.children = files;
    state.nodes.insert(path.clone(), node);

    subdirs
}

/// Recursive parallel scan using rayon
fn parallel_scan(path: PathBuf, state: &ScanState) {
    let subdirs = scan_directory(&path, state);
    subdirs.into_par_iter().for_each(|subdir| {
        parallel_scan(subdir, state);
    });
}

/// Build tree from flat DashMap
fn build_tree(state: &ScanState, root: &PathBuf) -> TopologyNode {
    // Collect all paths and sort by depth (deepest first)
    let mut paths: Vec<_> = state.nodes.iter().map(|r| r.key().clone()).collect();
    paths.sort_by(|a, b| {
        let depth_a = a.components().count();
        let depth_b = b.components().count();
        depth_b.cmp(&depth_a)
    });

    // Build tree bottom-up: attach children to parents
    for path in paths {
        if path == *root {
            continue;
        }
        if let Some(parent_path) = state.parent_map.get(&path).map(|r| r.clone()) {
            if let Some((_, node)) = state.nodes.remove(&path) {
                if let Some(mut parent) = state.nodes.get_mut(&parent_path) {
                    parent.children.push(node);
                }
            }
        }
    }

    state.nodes.remove(root).map(|(_, n)| n).unwrap_or_else(|| {
        TopologyNode::new_dir(root.to_string_lossy().to_string(), root.clone())
    })
}

/// Format bytes as human-readable string
pub fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;

    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }

    if unit_idx == 0 {
        format!("{} {}", bytes, UNITS[unit_idx])
    } else {
        format!("{:.2} {}", size, UNITS[unit_idx])
    }
}

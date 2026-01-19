mod report;

use dashmap::DashMap;
use humansize::{format_size, BINARY};
use parking_lot::Mutex;
use rayon::prelude::*;
use std::env;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::thread;

/// Threshold for collapsing small items (default 100MB)
const DEFAULT_THRESHOLD: u64 = 100 * 1024 * 1024;

/// Maximum depth to display (0 = unlimited)
const DEFAULT_MAX_DEPTH: usize = 4;

/// Coverage threshold - stop showing items once this % of parent is covered (default 97%)
const DEFAULT_COVERAGE: f64 = 97.0;

/// A node in the disk topology tree
#[derive(Debug, Clone)]
struct TopologyNode {
    name: String,
    path: PathBuf,
    size: u64,
    is_dir: bool,
    children: Vec<TopologyNode>,
    file_count: u64,
    dir_count: u64,
    // For aggregated "other" nodes
    is_aggregated: bool,
    aggregated_count: u64,
}

impl TopologyNode {
    fn new_file(name: String, path: PathBuf, size: u64) -> Self {
        Self {
            name,
            path,
            size,
            is_dir: false,
            children: Vec::new(),
            file_count: 1,
            dir_count: 0,
            is_aggregated: false,
            aggregated_count: 0,
        }
    }

    fn new_dir(name: String, path: PathBuf) -> Self {
        Self {
            name,
            path,
            size: 0,
            is_dir: true,
            children: Vec::new(),
            file_count: 0,
            dir_count: 1,
            is_aggregated: false,
            aggregated_count: 0,
        }
    }

    fn new_aggregated(name: String, size: u64, count: u64, file_count: u64, dir_count: u64) -> Self {
        Self {
            name,
            path: PathBuf::new(),
            size,
            is_dir: false,
            children: Vec::new(),
            file_count,
            dir_count,
            is_aggregated: true,
            aggregated_count: count,
        }
    }

    /// Recursively calculate total size
    fn calculate_size(&mut self) -> u64 {
        if self.is_dir && !self.is_aggregated {
            self.size = self.children.iter_mut().map(|c| c.calculate_size()).sum();
        }
        self.size
    }

    /// Recursively count files and dirs
    fn calculate_counts(&mut self) -> (u64, u64) {
        if self.is_dir && !self.is_aggregated {
            let mut total_files = 0u64;
            let mut total_dirs = 1u64; // Count self
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

    /// Apply threshold: collapse small items into "other"
    fn apply_threshold(&mut self, threshold: u64) {
        if !self.is_dir || self.is_aggregated {
            return;
        }

        // First, recursively apply to children
        for child in &mut self.children {
            child.apply_threshold(threshold);
        }

        // Separate children into above/below threshold
        let (large, small): (Vec<_>, Vec<_>) = self.children
            .drain(..)
            .partition(|c| c.size >= threshold);

        // Aggregate small items
        if !small.is_empty() {
            let other_size: u64 = small.iter().map(|c| c.size).sum();
            let other_files: u64 = small.iter().map(|c| c.file_count).sum();
            let other_dirs: u64 = small.iter().map(|c| c.dir_count).sum();
            let other_count = small.len() as u64;

            let other = TopologyNode::new_aggregated(
                format!("[{} items below {}]", other_count, format_size(threshold, BINARY)),
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

        // Sort children by size (largest first)
        self.children.sort_by(|a, b| b.size.cmp(&a.size));
    }

    /// Limit depth of tree
    fn limit_depth(&mut self, max_depth: usize, current_depth: usize) {
        if !self.is_dir || self.is_aggregated {
            return;
        }

        if current_depth >= max_depth && !self.children.is_empty() {
            // Collapse all children into one aggregated node
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

    /// Apply coverage-based aggregation: keep items until coverage_pct is reached, aggregate rest
    /// Also limits max children per node and prunes small branches
    fn apply_coverage(&mut self, coverage_pct: f64) {
        // Max 15 items per level, only recurse into items >1% of root
        self.apply_coverage_recursive(coverage_pct, 15, self.size, 1.0);
    }

    fn apply_coverage_recursive(&mut self, coverage_pct: f64, max_items: usize, root_size: u64, min_pct: f64) {
        if !self.is_dir || self.is_aggregated || self.size == 0 {
            return;
        }

        // Sort children by size (largest first)
        self.children.sort_by(|a, b| b.size.cmp(&a.size));

        // Only recurse into children that are significant (> min_pct of root)
        let min_size = (root_size as f64 * min_pct / 100.0) as u64;
        for child in &mut self.children {
            if child.size >= min_size {
                child.apply_coverage_recursive(coverage_pct, max_items, root_size, min_pct);
            } else {
                // Prune small branches - collapse their children
                if child.is_dir && !child.children.is_empty() {
                    let total_files: u64 = child.children.iter().map(|c| c.file_count).sum();
                    let total_dirs: u64 = child.children.iter().map(|c| c.dir_count).sum();
                    child.file_count = total_files;
                    child.dir_count = total_dirs + 1;
                    child.children.clear();
                }
            }
        }

        // Keep items until we hit coverage threshold OR max items
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

        // Ensure we keep at least some items
        keep_count = keep_count.max(1);

        // If there are items to aggregate
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

    /// Collect top items for reporting (sorted by size)
    fn collect_top_items(&self) -> Vec<(String, u64, u64, u64, bool)> {
        let mut items: Vec<(String, u64, u64, u64, bool)> = self.children
            .iter()
            .map(|c| (c.name.clone(), c.size, c.file_count, c.dir_count, c.is_aggregated))
            .collect();
        items.sort_by(|a, b| b.1.cmp(&a.1));
        items
    }

    /// Print tree structure
    fn print_tree(&self, prefix: &str, is_last: bool, show_counts: bool) {
        let connector = if is_last { "└── " } else { "├── " };
        let size_str = format_size(self.size, BINARY);

        let type_indicator = if self.is_aggregated {
            "◆"
        } else if self.is_dir {
            "📁"
        } else {
            "📄"
        };

        let count_str = if show_counts && (self.is_dir || self.is_aggregated) {
            format!(" ({} files, {} dirs)", self.file_count, self.dir_count)
        } else {
            String::new()
        };

        let pct = ""; // We'll add percentage at parent level

        println!(
            "{}{}{} {} [{}]{}",
            prefix, connector, type_indicator, self.name, size_str, count_str
        );

        let new_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });

        for (i, child) in self.children.iter().enumerate() {
            let is_last_child = i == self.children.len() - 1;
            child.print_tree(&new_prefix, is_last_child, show_counts);
        }
    }

    /// Print with percentages relative to parent
    fn print_tree_with_pct(&self, prefix: &str, is_last: bool, parent_size: u64, show_counts: bool) {
        let connector = if is_last { "└── " } else { "├── " };
        let size_str = format_size(self.size, BINARY);

        let type_indicator = if self.is_aggregated {
            "◆"
        } else if self.is_dir {
            "📁"
        } else {
            "📄"
        };

        let pct = if parent_size > 0 {
            (self.size as f64 / parent_size as f64) * 100.0
        } else {
            100.0
        };

        let count_str = if show_counts && (self.is_dir || self.is_aggregated) {
            format!(" ({} files, {} dirs)", self.file_count, self.dir_count)
        } else {
            String::new()
        };

        println!(
            "{}{}{} {} [{} / {:.1}%]{}",
            prefix, connector, type_indicator, self.name, size_str, pct, count_str
        );

        let new_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });

        for (i, child) in self.children.iter().enumerate() {
            let is_last_child = i == self.children.len() - 1;
            child.print_tree_with_pct(&new_prefix, is_last_child, self.size, show_counts);
        }
    }

    /// Export as JSON
    fn to_json(&self, indent: usize) -> String {
        let spaces = "  ".repeat(indent);
        let mut json = format!("{}{{\n", spaces);
        json += &format!("{}  \"name\": \"{}\",\n", spaces, self.name.replace("\"", "\\\""));
        json += &format!("{}  \"size\": {},\n", spaces, self.size);
        json += &format!("{}  \"size_human\": \"{}\",\n", spaces, format_size(self.size, BINARY));
        json += &format!("{}  \"is_dir\": {},\n", spaces, self.is_dir);
        json += &format!("{}  \"is_aggregated\": {},\n", spaces, self.is_aggregated);
        json += &format!("{}  \"file_count\": {},\n", spaces, self.file_count);
        json += &format!("{}  \"dir_count\": {},\n", spaces, self.dir_count);

        if !self.children.is_empty() {
            json += &format!("{}  \"children\": [\n", spaces);
            for (i, child) in self.children.iter().enumerate() {
                json += &child.to_json(indent + 2);
                if i < self.children.len() - 1 {
                    json += ",";
                }
                json += "\n";
            }
            json += &format!("{}  ]\n", spaces);
        } else {
            json += &format!("{}  \"children\": []\n", spaces);
        }

        json += &format!("{}}}", spaces);
        json
    }
}

/// Progress tracking for live updates
struct Progress {
    dirs_scanned: AtomicU64,
    files_found: AtomicU64,
    bytes_found: AtomicU64,
    errors: AtomicU64,
    running: AtomicBool,
    current_path: Mutex<String>,
}

impl Progress {
    fn new() -> Self {
        Self {
            dirs_scanned: AtomicU64::new(0),
            files_found: AtomicU64::new(0),
            bytes_found: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            running: AtomicBool::new(true),
            current_path: Mutex::new(String::new()),
        }
    }
}

/// Shared state for parallel scanning
struct ScanState {
    nodes: DashMap<PathBuf, TopologyNode>,
    parent_map: DashMap<PathBuf, PathBuf>,
    progress: Progress,
}

impl ScanState {
    fn new() -> Self {
        Self {
            nodes: DashMap::new(),
            parent_map: DashMap::new(),
            progress: Progress::new(),
        }
    }
}

/// Scan a directory and return its node + subdirectories to process
fn scan_directory(path: &PathBuf, state: &ScanState) -> Vec<PathBuf> {
    let mut subdirs = Vec::new();
    let mut files = Vec::new();
    let mut dir_bytes: u64 = 0;
    let mut dir_files: u64 = 0;

    // Update current path for progress display
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
                    let entry_name = entry
                        .file_name()
                        .to_string_lossy()
                        .to_string();

                    if metadata.is_file() {
                        let size = metadata.len();
                        files.push(TopologyNode::new_file(
                            entry_name,
                            entry_path,
                            size,
                        ));
                        dir_bytes += size;
                        dir_files += 1;
                    } else if metadata.is_dir() {
                        subdirs.push(entry_path.clone());

                        // Record parent relationship
                        state.parent_map.insert(entry_path, path.clone());
                    }
                }
            }
        }
        Err(_) => {
            state.progress.errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    // Update progress counters
    state.progress.dirs_scanned.fetch_add(1, Ordering::Relaxed);
    state.progress.files_found.fetch_add(dir_files, Ordering::Relaxed);
    state.progress.bytes_found.fetch_add(dir_bytes, Ordering::Relaxed);

    // Create node for this directory with its files
    let mut node = TopologyNode::new_dir(dir_name, path.clone());
    node.children = files;

    state.nodes.insert(path.clone(), node);

    subdirs
}

/// Recursive parallel scan
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
        depth_b.cmp(&depth_a) // Deepest first
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

fn print_usage() {
    eprintln!("Usage: disk_scanner [OPTIONS] <PATH>");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  -t, --threshold <BYTES>  Size threshold for aggregation (default: 100MB)");
    eprintln!("                           Supports suffixes: K, M, G (e.g., 50M, 1G)");
    eprintln!("  -p, --coverage <PCT>     Coverage %: aggregate once this % is shown (default: 97)");
    eprintln!("                           Use instead of threshold for smarter aggregation");
    eprintln!("  -d, --depth <N>          Maximum depth to display (default: 4, 0=unlimited)");
    eprintln!("  -j, --json               Output as JSON");
    eprintln!("  -c, --counts             Show file/directory counts");
    eprintln!("  -r, --report             Generate comprehensive visual report");
    eprintln!("  --html <FILE>            Generate interactive HTML treemap (unlimited depth)");
    eprintln!("  -w, --workers <N>        Number of worker threads (default: 12)");
    eprintln!("  -h, --help               Show this help");
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  disk_scanner ~                    # Scan home with defaults");
    eprintln!("  disk_scanner -p 95 /             # Show items until 95% coverage, aggregate rest");
    eprintln!("  disk_scanner -t 1G -d 3 /        # 1GB threshold, depth 3");
    eprintln!("  disk_scanner -r -t 500M ~        # Visual report with 500MB threshold");
    eprintln!("  disk_scanner --html report.html / # Interactive HTML treemap (drillable)");
    eprintln!("  disk_scanner -p 90 --html out.html / # 90% coverage HTML treemap");
}

/// Start progress display thread
fn start_progress_display(state: Arc<ScanState>, start_time: Instant) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let spinner = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        let mut spin_idx = 0;

        while state.progress.running.load(Ordering::Relaxed) {
            let dirs = state.progress.dirs_scanned.load(Ordering::Relaxed);
            let files = state.progress.files_found.load(Ordering::Relaxed);
            let bytes = state.progress.bytes_found.load(Ordering::Relaxed);
            let errors = state.progress.errors.load(Ordering::Relaxed);
            let elapsed = start_time.elapsed().as_secs_f64();

            let current_path = {
                let path = state.progress.current_path.lock();
                // Truncate long paths for display
                if path.len() > 50 {
                    format!("...{}", &path[path.len()-47..])
                } else {
                    path.clone()
                }
            };

            let throughput = if elapsed > 0.0 {
                bytes as f64 / elapsed / (1024.0 * 1024.0 * 1024.0)
            } else {
                0.0
            };

            // Clear line and print progress
            eprint!("\r\x1b[K{} Scanning: {} dirs, {} files, {} ({:.1} GB/s) {}",
                spinner[spin_idx],
                dirs,
                files,
                format_size(bytes, BINARY),
                throughput,
                if errors > 0 { format!("[{} errors]", errors) } else { String::new() }
            );
            eprint!("\r\x1b[K{} {} | {} dirs | {} files | {} | {:.1} GB/s",
                spinner[spin_idx],
                current_path,
                dirs,
                files,
                format_size(bytes, BINARY),
                throughput
            );
            let _ = io::stderr().flush();

            spin_idx = (spin_idx + 1) % spinner.len();
            thread::sleep(Duration::from_millis(100));
        }

        // Clear the progress line
        eprint!("\r\x1b[K");
        let _ = io::stderr().flush();
    })
}

fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().to_uppercase();
    let (num_str, multiplier) = if s.ends_with('K') {
        (&s[..s.len()-1], 1024u64)
    } else if s.ends_with('M') {
        (&s[..s.len()-1], 1024 * 1024)
    } else if s.ends_with('G') {
        (&s[..s.len()-1], 1024 * 1024 * 1024)
    } else {
        (s.as_str(), 1)
    };

    num_str.parse::<u64>().ok().map(|n| n * multiplier)
}

#[cfg(unix)]
fn get_disk_info(path: &PathBuf) -> Option<report::DiskInfo> {
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

            Some(report::DiskInfo {
                total_capacity: total,
                used,
                available,
            })
        } else {
            None
        }
    }
}

#[cfg(not(unix))]
fn get_disk_info(_path: &PathBuf) -> Option<report::DiskInfo> {
    None
}

fn main() {
    let args: Vec<String> = env::args().collect();

    let mut threshold = DEFAULT_THRESHOLD;
    let mut max_depth = DEFAULT_MAX_DEPTH;
    let mut coverage_pct: Option<f64> = None;  // None means use threshold, Some means use coverage
    let mut json_output = false;
    let mut show_counts = false;
    let mut report_mode = false;
    let mut html_output: Option<String> = None;
    let mut num_workers = 12;
    let mut path: Option<PathBuf> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_usage();
                return;
            }
            "-p" | "--coverage" => {
                i += 1;
                if i < args.len() {
                    coverage_pct = args[i].parse().ok();
                }
            }
            "-t" | "--threshold" => {
                i += 1;
                if i < args.len() {
                    threshold = parse_size(&args[i]).unwrap_or(DEFAULT_THRESHOLD);
                }
            }
            "-d" | "--depth" => {
                i += 1;
                if i < args.len() {
                    max_depth = args[i].parse().unwrap_or(DEFAULT_MAX_DEPTH);
                }
            }
            "-j" | "--json" => {
                json_output = true;
            }
            "-c" | "--counts" => {
                show_counts = true;
            }
            "-r" | "--report" => {
                report_mode = true;
            }
            "--html" => {
                i += 1;
                if i < args.len() {
                    html_output = Some(args[i].clone());
                }
            }
            "-w" | "--workers" => {
                i += 1;
                if i < args.len() {
                    num_workers = args[i].parse().unwrap_or(12);
                }
            }
            _ => {
                if !args[i].starts_with('-') {
                    path = Some(PathBuf::from(&args[i]));
                }
            }
        }
        i += 1;
    }

    let root = path.unwrap_or_else(|| {
        env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    });

    if !root.exists() {
        eprintln!("Error: Path does not exist: {}", root.display());
        std::process::exit(1);
    }

    // Get disk info
    let disk_info = get_disk_info(&root);

    // Configure thread pool
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(num_workers)
        .build()
        .expect("Failed to build thread pool");

    let start = Instant::now();
    let state = Arc::new(ScanState::new());

    // Start progress display (unless JSON output)
    let progress_handle = if !json_output {
        let agg_mode = if let Some(pct) = coverage_pct {
            format!("coverage: {}%", pct)
        } else {
            format!("threshold: {}", format_size(threshold, BINARY))
        };
        let depth_str = if html_output.is_some() {
            "unlimited (HTML)".to_string()
        } else if max_depth == 0 {
            "unlimited".to_string()
        } else {
            max_depth.to_string()
        };
        eprintln!("Scanning {} with {} workers ({}, depth: {})\n",
            root.display(),
            num_workers,
            agg_mode,
            depth_str
        );
        Some(start_progress_display(Arc::clone(&state), start))
    } else {
        None
    };

    let state_clone = Arc::clone(&state);
    let root_clone = root.clone();

    pool.install(|| {
        parallel_scan(root_clone, &state_clone);
    });

    // Stop progress display
    state.progress.running.store(false, Ordering::Relaxed);
    if let Some(handle) = progress_handle {
        let _ = handle.join();
    }

    let scan_time = start.elapsed();
    let final_dirs = state.progress.dirs_scanned.load(Ordering::Relaxed);
    let final_files = state.progress.files_found.load(Ordering::Relaxed);
    let final_bytes = state.progress.bytes_found.load(Ordering::Relaxed);
    let final_errors = state.progress.errors.load(Ordering::Relaxed);

    if !json_output {
        eprintln!("✓ Scan completed in {:.2}s: {} dirs, {} files, {} {}",
            scan_time.as_secs_f64(),
            final_dirs,
            final_files,
            format_size(final_bytes, BINARY),
            if final_errors > 0 { format!("({} permission errors)", final_errors) } else { String::new() }
        );
        eprintln!("Building topology...");
    }

    // Build tree
    let mut tree = build_tree(&state, &root);

    // Calculate sizes and counts
    tree.calculate_size();
    tree.calculate_counts();

    // Apply aggregation: coverage-based or threshold-based
    if let Some(pct) = coverage_pct {
        // Coverage-based: keep items until X% is covered
        tree.apply_coverage(pct);
    } else {
        // Threshold-based: aggregate items below size threshold
        tree.apply_threshold(threshold);
    }

    // Limit depth (for HTML, use unlimited depth unless explicitly set)
    let effective_depth = if html_output.is_some() && max_depth == DEFAULT_MAX_DEPTH {
        0  // Unlimited depth for HTML by default
    } else {
        max_depth
    };

    if effective_depth > 0 {
        tree.limit_depth(effective_depth, 0);
    }

    let total_time = start.elapsed();

    // Collect top items for report
    let top_items = tree.collect_top_items();

    // Output
    if let Some(html_file) = html_output {
        // Generate HTML report
        let json_data = tree.to_json(0);
        let html = report::generate_html_report(
            &tree.name,
            tree.size,
            tree.file_count,
            tree.dir_count,
            &json_data,
            scan_time.as_secs_f64(),
            disk_info.as_ref(),
        );
        match File::create(&html_file) {
            Ok(mut file) => {
                if let Err(e) = file.write_all(html.as_bytes()) {
                    eprintln!("Error writing HTML: {}", e);
                } else {
                    eprintln!("HTML report written to: {}", html_file);
                }
            }
            Err(e) => eprintln!("Error creating file: {}", e),
        }
    } else if json_output {
        println!("{}", tree.to_json(0));
    } else if report_mode {
        // Generate comprehensive visual report
        let report_text = report::generate_report(
            &tree.name,
            tree.size,
            tree.file_count,
            tree.dir_count,
            &top_items,
            scan_time.as_secs_f64(),
            threshold,
            max_depth,
            disk_info.as_ref(),
        );
        println!("{}", report_text);

        // Also print the tree
        println!("\n┌──────────────────────────────────────────────────────────────────────────────┐");
        println!("│                              DIRECTORY TREE                                 │");
        println!("├──────────────────────────────────────────────────────────────────────────────┤");
        println!("│ 📁 {} [{}] ({} files, {} dirs)",
            tree.name,
            format_size(tree.size, BINARY),
            tree.file_count,
            tree.dir_count
        );
        println!("│");

        for (i, child) in tree.children.iter().enumerate() {
            let is_last = i == tree.children.len() - 1;
            print_tree_with_border(child, "│ ", is_last, tree.size);
        }
        println!("└──────────────────────────────────────────────────────────────────────────────┘");
    } else {
        println!();
        println!("════════════════════════════════════════════════════════════════════════════════");
        println!("DISK TOPOLOGY: {}", root.display());
        println!("════════════════════════════════════════════════════════════════════════════════");
        println!();
        println!("📁 {} [{}] ({} files, {} dirs)",
            tree.name,
            format_size(tree.size, BINARY),
            tree.file_count,
            tree.dir_count
        );

        for (i, child) in tree.children.iter().enumerate() {
            let is_last = i == tree.children.len() - 1;
            child.print_tree_with_pct("", is_last, tree.size, show_counts);
        }

        println!();
        println!("════════════════════════════════════════════════════════════════════════════════");
        println!("Scan time: {:.2}s | Total time: {:.2}s | Threshold: {} | Depth: {}",
            scan_time.as_secs_f64(),
            total_time.as_secs_f64(),
            format_size(threshold, BINARY),
            if max_depth == 0 { "unlimited".to_string() } else { max_depth.to_string() }
        );
        println!("════════════════════════════════════════════════════════════════════════════════");
    }
}

fn print_tree_with_border(node: &TopologyNode, prefix: &str, is_last: bool, parent_size: u64) {
    let connector = if is_last { "└── " } else { "├── " };
    let size_str = format_size(node.size, BINARY);

    let type_indicator = if node.is_aggregated {
        "◆"
    } else if node.is_dir {
        "📁"
    } else {
        "📄"
    };

    let pct = if parent_size > 0 {
        (node.size as f64 / parent_size as f64) * 100.0
    } else {
        100.0
    };

    println!(
        "{}{}{} {} [{} / {:.1}%]",
        prefix, connector, type_indicator, node.name, size_str, pct
    );

    let new_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });

    for (i, child) in node.children.iter().enumerate() {
        let is_last_child = i == node.children.len() - 1;
        print_tree_with_border(child, &new_prefix, is_last_child, node.size);
    }
}

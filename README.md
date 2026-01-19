# disk_scanner

High-performance parallel disk scanner with smart aggregation for building disk usage visualizations.

[![Crates.io](https://img.shields.io/crates/v/disk_scanner.svg)](https://crates.io/crates/disk_scanner)
[![Documentation](https://docs.rs/disk_scanner/badge.svg)](https://docs.rs/disk_scanner)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

## Features

- **Blazing Fast**: Uses rayon work-stealing parallelism to achieve 40+ GB/s throughput on NVMe drives
- **Smart Aggregation**: Coverage-based or threshold-based aggregation to keep output manageable
- **Real-time Progress**: Thread-safe progress tracking for live UI updates
- **Multiple Output Formats**: Tree view, JSON, and interactive HTML treemap
- **Tauri Ready**: All types are serde-serializable for easy frontend integration
- **Dual Use**: Works as both a CLI tool and a Rust library

## Performance

On Apple M4 Max with NVMe storage:
- **1.3 TiB** scanned in **~30 seconds**
- **44 GB/s** sustained throughput
- **6.2 million files**, **1 million directories**

## Installation

### CLI Tool

```bash
cargo install disk_scanner
```

### As a Library

```bash
cargo add disk_scanner
```

Or add to your `Cargo.toml`:

```toml
[dependencies]
disk_scanner = "0.2"
```

## CLI Usage

```bash
# Basic scan of home directory
disk_scanner ~

# Scan root with 95% coverage (show items until 95% of space covered)
disk_scanner -p 95 /

# Use size threshold instead (aggregate items below 1GB)
disk_scanner -t 1G /

# Generate interactive HTML treemap
disk_scanner --html report.html /

# Generate visual ASCII report
disk_scanner -r ~/Documents

# Limit depth and use more workers
disk_scanner -p 90 -d 6 -w 16 /

# Output as JSON
disk_scanner -j ~/Projects
```

### CLI Options

```
Usage: disk_scanner [OPTIONS] <PATH>

Options:
  -t, --threshold <BYTES>  Size threshold for aggregation (default: 100MB)
                           Supports suffixes: K, M, G (e.g., 50M, 1G)
  -p, --coverage <PCT>     Coverage %: aggregate once this % is shown (default: 97)
                           Use instead of threshold for smarter aggregation
  -d, --depth <N>          Maximum depth to display (default: 4, 0=unlimited)
  -j, --json               Output as JSON
  -c, --counts             Show file/directory counts
  -r, --report             Generate comprehensive visual report
  --html <FILE>            Generate interactive HTML treemap (unlimited depth)
  -w, --workers <N>        Number of worker threads (default: 12)
  -h, --help               Show this help
```

### Output Examples

**Tree View (default)**
```
DISK TOPOLOGY: /Users/josh
================================================================================

/Users/josh [1.28 TiB] (6,234,567 files, 892,345 dirs)
├── Library [456.78 GiB / 34.8%]
│   ├── Caches [234.56 GiB / 51.3%]
│   └── [42 more, 3.2%]
├── .cargo [123.45 GiB / 9.4%]
└── [156 more, 2.1%]
```

**HTML Treemap (`--html`)**

Generates an interactive D3.js treemap with:
- Click-to-zoom navigation
- Breadcrumb trail
- Hover tooltips with file/directory counts
- Disk usage overview bar
- Responsive dark theme

## Library Usage

### Quick Start

```rust
use disk_scanner::{DiskScanner, ScanConfig};
use std::path::PathBuf;

// Create scanner with 97% coverage aggregation
let config = ScanConfig {
    coverage_pct: Some(97.0),
    ..Default::default()
};

let scanner = DiskScanner::new(config);
let result = scanner.scan(PathBuf::from("/"));

println!("Scanned {} in {:.2}s", result.root.size_human, result.scan_time_secs);
println!("Files: {}, Dirs: {}", result.total_files, result.total_dirs);
println!("Throughput: {:.1} GB/s", result.throughput_gbps);

// Result is JSON-serializable
let json = serde_json::to_string(&result).unwrap();
```

### Progress Tracking

Monitor scan progress in real-time (perfect for UIs):

```rust
use disk_scanner::{DiskScanner, ScanConfig};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

let scanner = DiskScanner::new(ScanConfig::default());
let progress = scanner.progress();

// Monitor progress from another thread
let progress_clone = progress.clone();
thread::spawn(move || {
    while progress_clone.running.load(Ordering::Relaxed) {
        let snap = progress_clone.snapshot();
        println!("Scanned {} dirs, {} files, {} bytes",
            snap.dirs_scanned, snap.files_found, snap.bytes_found);
        thread::sleep(Duration::from_millis(100));
    }
});

let result = scanner.scan(PathBuf::from("/home"));
```

### Configuration Options

```rust
use disk_scanner::ScanConfig;

let config = ScanConfig {
    // Number of parallel workers (default: 12)
    num_workers: 12,

    // Coverage-based aggregation: keep largest items until X% is covered
    coverage_pct: Some(97.0),

    // Or use threshold-based: hide items below this size
    // threshold: Some(10 * 1024 * 1024), // 10 MB

    // Limit tree depth (0 = unlimited)
    max_depth: 0,
};
```

### Aggregation Strategies

**Coverage-based (Recommended)**

Shows the largest items that account for a percentage of total space:

```rust
let config = ScanConfig {
    coverage_pct: Some(97.0), // Show items until 97% of space is covered
    ..Default::default()
};
```

**Threshold-based**

Hides items below a size threshold:

```rust
let config = ScanConfig {
    threshold: Some(100 * 1024 * 1024), // Hide items below 100 MB
    coverage_pct: None,
    ..Default::default()
};
```

### Tauri Integration

```rust
use disk_scanner::{DiskScanner, ScanConfig, ScanResult};
use std::path::PathBuf;

#[tauri::command]
async fn scan_disk(path: String, coverage_pct: Option<f64>) -> Result<ScanResult, String> {
    let config = ScanConfig {
        coverage_pct: coverage_pct.or(Some(97.0)),
        ..Default::default()
    };
    let scanner = DiskScanner::new(config);
    Ok(scanner.scan(PathBuf::from(path)))
}
```

### Disk Information

Get filesystem capacity information:

```rust
use disk_scanner::get_disk_info;
use std::path::PathBuf;

if let Some(info) = get_disk_info(&PathBuf::from("/")) {
    println!("Total: {}", info.total_human);
    println!("Used: {} ({:.1}%)", info.used_human, info.usage_pct);
    println!("Available: {}", info.available_human);
}
```

## Output Structure

The `ScanResult` contains a hierarchical `TopologyNode` tree:

```rust
pub struct TopologyNode {
    pub name: String,          // File/directory name
    pub size: u64,             // Size in bytes
    pub size_human: String,    // Human-readable size
    pub is_dir: bool,          // Is this a directory?
    pub children: Vec<TopologyNode>,  // Child nodes
    pub file_count: u64,       // Total files in subtree
    pub dir_count: u64,        // Total directories in subtree
    pub is_aggregated: bool,   // Is this an aggregation bucket?
    pub aggregated_count: u64, // Number of items aggregated
}
```

## Implementation Notes

- **Parallelism**: Uses Rayon's work-stealing scheduler for optimal CPU utilization
- **Thread Safety**: `ScanProgress` is `Send + Sync` for safe sharing across threads
- **Symlinks**: Not followed (prevents infinite loops and double-counting)
- **Permissions**: Errors are counted but don't stop the scan
- **Memory**: Builds flat HashMap during scan, converts to tree after completion

## Comparison with Similar Tools

| Tool | Speed | Interactive | Library | Aggregation |
|------|-------|-------------|---------|-------------|
| **disk_scanner** | 44 GB/s | HTML treemap | Yes | Coverage/Threshold |
| ncdu | ~1 GB/s | TUI | No | None |
| dust | ~5 GB/s | No | No | Top N |
| dua | ~3 GB/s | TUI | No | None |

## License

MIT License - see [LICENSE](LICENSE) for details.

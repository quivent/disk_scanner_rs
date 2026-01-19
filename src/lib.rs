//! High-performance parallel disk scanner library
//!
//! This library provides fast, parallel disk scanning with smart aggregation
//! for building disk usage visualizations. Designed to be integrated into
//! Tauri apps or other Rust applications.
//!
//! # Features
//!
//! - **Blazing Fast**: Uses rayon work-stealing parallelism to achieve 40+ GB/s throughput
//! - **Smart Aggregation**: Coverage-based or threshold-based aggregation
//! - **Real-time Progress**: Thread-safe progress tracking for live UI updates
//! - **Tauri Ready**: All types are serde-serializable for easy frontend integration
//!
//! # Quick Start
//!
//! ```no_run
//! use disk_scanner::{DiskScanner, ScanConfig};
//! use std::path::PathBuf;
//!
//! // Create scanner with 97% coverage aggregation
//! let config = ScanConfig {
//!     coverage_pct: Some(97.0),
//!     ..Default::default()
//! };
//!
//! let scanner = DiskScanner::new(config);
//! let result = scanner.scan(PathBuf::from("/"));
//!
//! // Result is serializable to JSON for frontend
//! let json = serde_json::to_string(&result).unwrap();
//! ```
//!
//! # Progress Tracking
//!
//! ```no_run
//! use disk_scanner::{DiskScanner, ScanConfig};
//! use std::path::PathBuf;
//! use std::sync::atomic::Ordering;
//!
//! let scanner = DiskScanner::new(ScanConfig::default());
//! let progress = scanner.progress();
//!
//! // Monitor progress from another thread
//! std::thread::spawn(move || {
//!     while progress.running.load(Ordering::Relaxed) {
//!         let snap = progress.snapshot();
//!         println!("Scanned {} dirs, {} bytes", snap.dirs_scanned, snap.bytes_found);
//!         std::thread::sleep(std::time::Duration::from_millis(100));
//!     }
//! });
//! ```

mod scanner;

pub use scanner::{
    DiskScanner,
    ScanConfig,
    ScanResult,
    ScanProgress,
    ProgressSnapshot,
    TopologyNode,
    DiskInfo,
    get_disk_info,
    format_size,
};

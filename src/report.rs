use humansize::{format_size, BINARY};
use std::collections::HashMap;

/// Disk information
pub struct DiskInfo {
    pub total_capacity: u64,
    pub used: u64,
    pub available: u64,
}

/// Generate a comprehensive visual report
pub fn generate_report(
    root_name: &str,
    total_size: u64,
    total_files: u64,
    total_dirs: u64,
    top_items: &[(String, u64, u64, u64, bool)],
    scan_time: f64,
    threshold: u64,
    max_depth: usize,
    disk_info: Option<&DiskInfo>,
) -> String {
    let mut report = String::new();

    // Big header with disk usage
    report.push_str("\n");
    report.push_str("╔══════════════════════════════════════════════════════════════════════════════╗\n");
    report.push_str("║                                                                              ║\n");

    if let Some(disk) = disk_info {
        let used_pct = (disk.used as f64 / disk.total_capacity as f64) * 100.0;
        let scanned_pct = (total_size as f64 / disk.total_capacity as f64) * 100.0;

        // Big disk capacity display
        report.push_str(&format!(
            "║    💾 DISK: {:>10} total   {:>10} used   {:>10} free          ║\n",
            format_size(disk.total_capacity, BINARY),
            format_size(disk.used, BINARY),
            format_size(disk.available, BINARY)
        ));
        report.push_str("║                                                                              ║\n");

        // Visual disk usage bar
        let bar_width = 60;
        let used_bars = ((used_pct / 100.0) * bar_width as f64) as usize;
        let scanned_bars = ((scanned_pct / 100.0) * bar_width as f64) as usize;

        let mut bar = String::new();
        for i in 0..bar_width {
            if i < scanned_bars {
                bar.push('█');
            } else if i < used_bars {
                bar.push('▓');
            } else {
                bar.push('░');
            }
        }

        report.push_str(&format!("║    [{bar}]    ║\n"));
        report.push_str(&format!(
            "║     █ Scanned: {:>5.1}%    ▓ Other used: {:>5.1}%    ░ Free: {:>5.1}%          ║\n",
            scanned_pct,
            used_pct - scanned_pct,
            100.0 - used_pct
        ));
    } else {
        report.push_str(&format!(
            "║                    📁 SCANNED: {:>12}                               ║\n",
            format_size(total_size, BINARY)
        ));
    }

    report.push_str("║                                                                              ║\n");
    report.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");
    report.push_str("║                                                                              ║\n");

    // Big numbers section
    report.push_str(&format!(
        "║   {:^12}        {:^16}        {:^16}         ║\n",
        format_size(total_size, BINARY),
        format_with_commas(total_files),
        format_with_commas(total_dirs)
    ));
    report.push_str(&format!(
        "║   {:^12}        {:^16}        {:^16}         ║\n",
        "SCANNED", "FILES", "DIRECTORIES"
    ));
    report.push_str("║                                                                              ║\n");
    report.push_str("╠══════════════════════════════════════════════════════════════════════════════╣\n");

    // Performance row
    let throughput = (total_size as f64 / (1024.0 * 1024.0 * 1024.0)) / scan_time;
    report.push_str(&format!(
        "║   ⚡ {:.2}s scan time   │   📊 {:.1} GB/s throughput   │   🔍 Threshold: {:>8} ║\n",
        scan_time,
        throughput,
        format_size(threshold, BINARY)
    ));
    report.push_str("╚══════════════════════════════════════════════════════════════════════════════╝\n\n");

    // Top items bar chart - cleaner format
    report.push_str("┌──────────────────────────────────────────────────────────────────────────────┐\n");
    report.push_str("│                          📊 SIZE DISTRIBUTION                               │\n");
    report.push_str("├──────────────────────────────────────────────────────────────────────────────┤\n");

    let max_bar_width = 35;

    for (i, (name, size, _files, _dirs, is_agg)) in top_items.iter().take(15).enumerate() {
        let pct = (*size as f64 / total_size as f64) * 100.0;
        let bar_width = ((pct / 100.0) * max_bar_width as f64) as usize;

        // Color-code by percentage
        let bar_char = if pct > 20.0 { '█' }
                       else if pct > 10.0 { '▓' }
                       else if pct > 5.0 { '▒' }
                       else { '░' };
        let bar: String = std::iter::repeat(bar_char).take(bar_width.max(1)).collect();

        let icon = if *is_agg { "◆" } else { "📁" };
        let truncated_name: String = if name.len() > 20 {
            format!("{}…", &name[..19])
        } else {
            format!("{:<20}", name)
        };

        // Rank number
        let rank = format!("{:>2}.", i + 1);

        report.push_str(&format!(
            "│ {} {} {} {:>9} {:>5.1}% {:<35}│\n",
            rank, icon, truncated_name,
            format_size(*size, BINARY),
            pct,
            bar
        ));
    }

    if top_items.len() > 15 {
        let remaining: u64 = top_items.iter().skip(15).map(|(_, s, _, _, _)| *s).sum();
        let remaining_count = top_items.len() - 15;
        let pct = (remaining as f64 / total_size as f64) * 100.0;
        report.push_str(&format!(
            "│     ... and {} more items totaling {:>9} ({:.1}%)                      │\n",
            remaining_count,
            format_size(remaining, BINARY),
            pct
        ));
    }
    report.push_str("└──────────────────────────────────────────────────────────────────────────────┘\n\n");

    // Category breakdown - horizontal bars
    report.push_str("┌──────────────────────────────────────────────────────────────────────────────┐\n");
    report.push_str("│                          📂 BY CATEGORY                                     │\n");
    report.push_str("├──────────────────────────────────────────────────────────────────────────────┤\n");

    let mut categories: HashMap<&str, u64> = HashMap::new();
    for (name, size, _, _, _) in top_items {
        let category = categorize_path(name);
        *categories.entry(category).or_insert(0) += size;
    }

    let mut cat_vec: Vec<_> = categories.into_iter().collect();
    cat_vec.sort_by(|a, b| b.1.cmp(&a.1));

    for (category, size) in cat_vec.iter().take(8) {
        let pct = (*size as f64 / total_size as f64) * 100.0;
        let bar_width = ((pct / 100.0) * 40.0) as usize;
        let bar: String = "▰".repeat(bar_width) + &"▱".repeat(40 - bar_width);

        report.push_str(&format!(
            "│  {:<18} {:>10} {:>5.1}%  {}  │\n",
            category,
            format_size(*size, BINARY),
            pct,
            bar
        ));
    }
    report.push_str("└──────────────────────────────────────────────────────────────────────────────┘\n");

    report
}

fn format_with_commas(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.insert(0, ',');
        }
        result.insert(0, c);
    }
    result
}

fn categorize_path(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    if lower.contains("cache") || (lower.starts_with('.') && lower.contains("cache")) {
        "Cache"
    } else if lower.contains("library") {
        "System/Library"
    } else if lower.contains("node_modules") || lower.contains(".npm") {
        "Node.js/npm"
    } else if lower.contains("target") || lower.contains(".cargo") || lower.contains(".rustup") {
        "Rust/Cargo"
    } else if lower.contains("huggingface") || lower.contains("model") || lower.contains("mlx") {
        "ML Models"
    } else if lower.contains("tauri") {
        "Tauri Apps"
    } else if lower.starts_with('.') {
        "Hidden/Config"
    } else if lower.contains("document") || lower.contains("download") {
        "User Data"
    } else {
        "Projects"
    }
}

/// Generate HTML treemap for browser visualization
pub fn generate_html_report(
    root_name: &str,
    total_size: u64,
    total_files: u64,
    total_dirs: u64,
    json_data: &str,
    scan_time: f64,
    disk_info: Option<&DiskInfo>,
) -> String {
    let disk_total = disk_info.map(|d| d.total_capacity).unwrap_or(total_size);
    let disk_used = disk_info.map(|d| d.used).unwrap_or(total_size);
    let disk_free = disk_info.map(|d| d.available).unwrap_or(0);
    let usage_pct = (disk_used as f64 / disk_total as f64) * 100.0;

    format!(r#"<!DOCTYPE html>
<html>
<head>
    <title>Disk Topology: {root_name}</title>
    <script src="https://d3js.org/d3.v7.min.js"></script>
    <style>
        * {{ box-sizing: border-box; margin: 0; padding: 0; }}
        body {{
            font-family: -apple-system, BlinkMacSystemFont, 'SF Pro Display', 'Segoe UI', Roboto, sans-serif;
            background: linear-gradient(135deg, #0a0a1a 0%, #1a1a2e 100%);
            color: #eee;
            min-height: 100vh;
            padding: 30px;
        }}
        .container {{ max-width: 1400px; margin: 0 auto; }}

        h1 {{
            font-size: 2.5em;
            font-weight: 300;
            margin-bottom: 30px;
            background: linear-gradient(90deg, #00d4ff, #00ff88);
            -webkit-background-clip: text;
            -webkit-text-fill-color: transparent;
        }}

        .hero {{
            background: rgba(255,255,255,0.03);
            border-radius: 20px;
            padding: 40px;
            margin-bottom: 30px;
            border: 1px solid rgba(255,255,255,0.1);
        }}

        .disk-bar {{
            height: 40px;
            background: rgba(255,255,255,0.1);
            border-radius: 20px;
            overflow: hidden;
            margin: 20px 0;
            position: relative;
        }}

        .disk-bar-fill {{
            height: 100%;
            background: linear-gradient(90deg, #00d4ff, #00ff88);
            border-radius: 20px;
            transition: width 1s ease;
        }}

        .disk-bar-label {{
            position: absolute;
            right: 15px;
            top: 50%;
            transform: translateY(-50%);
            font-weight: 600;
            color: #fff;
            text-shadow: 0 1px 3px rgba(0,0,0,0.5);
        }}

        .stats {{
            display: grid;
            grid-template-columns: repeat(4, 1fr);
            gap: 20px;
            margin: 30px 0;
        }}

        .stat-box {{
            background: rgba(255,255,255,0.05);
            padding: 25px;
            border-radius: 15px;
            text-align: center;
            border: 1px solid rgba(255,255,255,0.1);
            transition: transform 0.2s, background 0.2s;
        }}

        .stat-box:hover {{
            transform: translateY(-5px);
            background: rgba(255,255,255,0.08);
        }}

        .stat-value {{
            font-size: 2.2em;
            font-weight: 600;
            background: linear-gradient(90deg, #00d4ff, #00ff88);
            -webkit-background-clip: text;
            -webkit-text-fill-color: transparent;
        }}

        .stat-label {{
            color: #888;
            margin-top: 8px;
            font-size: 0.9em;
            text-transform: uppercase;
            letter-spacing: 1px;
        }}

        #treemap-container {{
            background: rgba(255,255,255,0.03);
            border-radius: 20px;
            border: 1px solid rgba(255,255,255,0.1);
            overflow: hidden;
            padding: 20px;
        }}

        #breadcrumb {{
            display: flex;
            align-items: center;
            gap: 8px;
            margin-bottom: 15px;
            flex-wrap: wrap;
        }}

        .breadcrumb-item {{
            background: rgba(0,212,255,0.2);
            padding: 8px 16px;
            border-radius: 20px;
            cursor: pointer;
            transition: all 0.2s;
            border: 1px solid rgba(0,212,255,0.3);
        }}

        .breadcrumb-item:hover {{
            background: rgba(0,212,255,0.4);
        }}

        .breadcrumb-sep {{
            color: #666;
        }}

        #treemap {{
            width: 100%;
            height: calc(100vh - 450px);
            min-height: 400px;
            max-height: 800px;
        }}

        .node {{
            stroke: rgba(0,0,0,0.5);
            stroke-width: 1px;
            cursor: pointer;
            transition: all 0.2s;
        }}

        .node:hover {{
            stroke: #00d4ff;
            stroke-width: 2px;
            filter: brightness(1.2);
        }}

        .node-label {{
            fill: white;
            font-size: 12px;
            font-weight: 500;
            pointer-events: none;
            text-shadow: 0 1px 3px rgba(0,0,0,0.9);
        }}

        .node-size {{
            fill: rgba(255,255,255,0.8);
            font-size: 11px;
            pointer-events: none;
            text-shadow: 0 1px 3px rgba(0,0,0,0.9);
        }}

        #tooltip {{
            position: fixed;
            background: rgba(20,20,40,0.98);
            padding: 18px 22px;
            border-radius: 12px;
            border: 1px solid #00d4ff;
            pointer-events: none;
            opacity: 0;
            z-index: 1000;
            backdrop-filter: blur(10px);
            box-shadow: 0 10px 40px rgba(0,0,0,0.5);
            max-width: 350px;
        }}

        #tooltip strong {{
            color: #00d4ff;
            font-size: 1.15em;
            display: block;
            margin-bottom: 8px;
        }}

        #tooltip .tip-row {{
            display: flex;
            justify-content: space-between;
            margin: 4px 0;
            color: #aaa;
        }}

        #tooltip .tip-value {{
            color: #fff;
            font-weight: 500;
        }}

        .legend {{
            display: flex;
            gap: 15px;
            margin-top: 15px;
            flex-wrap: wrap;
            justify-content: center;
        }}

        .legend-item {{
            display: flex;
            align-items: center;
            gap: 6px;
            font-size: 0.85em;
            color: #888;
        }}

        .legend-color {{
            width: 14px;
            height: 14px;
            border-radius: 3px;
        }}

        .instructions {{
            text-align: center;
            color: #666;
            font-size: 0.9em;
            margin-top: 10px;
        }}
    </style>
</head>
<body>
    <div class="container">
        <h1>💾 Disk Topology</h1>

        <div class="hero">
            <div style="display: flex; justify-content: space-between; align-items: center; margin-bottom: 15px;">
                <span style="font-size: 1.4em; color: #888;">Total Capacity</span>
                <span style="font-size: 2em; font-weight: 600;">{disk_total_fmt}</span>
            </div>
            <div class="disk-bar">
                <div class="disk-bar-fill" style="width: {usage_pct:.1}%"></div>
                <span class="disk-bar-label">{usage_pct:.1}% used</span>
            </div>
            <div style="display: flex; justify-content: space-between; color: #888;">
                <span>🟢 {disk_free_fmt} free</span>
                <span>🔵 {disk_used_fmt} used</span>
            </div>
        </div>

        <div class="stats">
            <div class="stat-box">
                <div class="stat-value">{scanned_fmt}</div>
                <div class="stat-label">Scanned</div>
            </div>
            <div class="stat-box">
                <div class="stat-value">{files_fmt}</div>
                <div class="stat-label">Files</div>
            </div>
            <div class="stat-box">
                <div class="stat-value">{dirs_fmt}</div>
                <div class="stat-label">Directories</div>
            </div>
            <div class="stat-box">
                <div class="stat-value">{scan_time:.1}s</div>
                <div class="stat-label">Scan Time</div>
            </div>
        </div>

        <div id="treemap-container">
            <div id="breadcrumb"></div>
            <div id="treemap"></div>
            <p class="instructions">Click a folder to zoom in • Click breadcrumb to zoom out</p>
        </div>
    </div>
    <div id="tooltip"></div>

    <script>
        const data = {json_data};
        const container = document.getElementById('treemap');
        const width = container.clientWidth || 1200;
        const height = container.clientHeight || Math.max(400, Math.min(800, window.innerHeight - 450));

        // Color scale based on depth and type
        const colors = ['#00d4ff', '#00ff88', '#ff6b6b', '#ffd93d', '#6bcb77', '#4d96ff', '#ff8fab', '#845ec2', '#ffc75f', '#00c9a7'];
        const colorScale = d3.scaleOrdinal().range(colors);

        // Build hierarchy
        const root = d3.hierarchy(data)
            .sum(d => d.children && d.children.length ? 0 : d.size)
            .sort((a, b) => b.value - a.value);

        // Treemap layout
        const treemap = d3.treemap()
            .size([width, height])
            .paddingTop(22)
            .paddingRight(3)
            .paddingBottom(3)
            .paddingLeft(3)
            .round(true);

        // Create SVG
        const svg = d3.select('#treemap')
            .append('svg')
            .attr('width', width)
            .attr('height', height);

        const tooltip = d3.select('#tooltip');

        let currentRoot = root;
        let pathStack = [root];

        function formatSize(bytes) {{
            if (bytes >= 1024*1024*1024*1024) return (bytes / (1024*1024*1024*1024)).toFixed(2) + ' TiB';
            if (bytes >= 1024*1024*1024) return (bytes / (1024*1024*1024)).toFixed(2) + ' GiB';
            if (bytes >= 1024*1024) return (bytes / (1024*1024)).toFixed(2) + ' MiB';
            if (bytes >= 1024) return (bytes / 1024).toFixed(2) + ' KiB';
            return bytes + ' B';
        }}

        function positionTooltip(event) {{
            const tooltipNode = tooltip.node();
            const tooltipRect = tooltipNode.getBoundingClientRect();
            const padding = 15;

            let left = event.clientX + padding;
            let top = event.clientY - 10;

            // Check right edge
            if (left + tooltipRect.width > window.innerWidth - padding) {{
                left = event.clientX - tooltipRect.width - padding;
            }}

            // Check bottom edge
            if (top + tooltipRect.height > window.innerHeight - padding) {{
                top = window.innerHeight - tooltipRect.height - padding;
            }}

            // Check top edge
            if (top < padding) {{
                top = padding;
            }}

            // Check left edge
            if (left < padding) {{
                left = padding;
            }}

            tooltip
                .style('left', left + 'px')
                .style('top', top + 'px');
        }}

        function updateBreadcrumb() {{
            const bc = d3.select('#breadcrumb');
            bc.html('');

            pathStack.forEach((node, i) => {{
                if (i > 0) {{
                    bc.append('span').attr('class', 'breadcrumb-sep').text('›');
                }}
                bc.append('span')
                    .attr('class', 'breadcrumb-item')
                    .text(node.data.name || '/')
                    .on('click', () => {{
                        pathStack = pathStack.slice(0, i + 1);
                        currentRoot = node;
                        render(node);
                    }});
            }});
        }}

        function render(node) {{
            treemap(node);

            svg.selectAll('*').remove();

            const nodes = node.children || [];

            // Draw rectangles
            const cells = svg.selectAll('g')
                .data(nodes)
                .join('g')
                .attr('transform', d => `translate(${{d.x0}},${{d.y0}})`);

            cells.append('rect')
                .attr('class', 'node')
                .attr('width', d => Math.max(0, d.x1 - d.x0))
                .attr('height', d => Math.max(0, d.y1 - d.y0))
                .attr('fill', (d, i) => {{
                    if (d.data.is_aggregated) return '#555';
                    return colorScale(d.data.name);
                }})
                .attr('rx', 4)
                .on('click', (event, d) => {{
                    if (d.children && d.children.length > 0 && !d.data.is_aggregated) {{
                        pathStack.push(d);
                        currentRoot = d;
                        render(d);
                    }}
                }})
                .on('mouseover', (event, d) => {{
                    const pct = node.value ? ((d.value / node.value) * 100).toFixed(1) : 0;
                    tooltip.style('opacity', 1)
                        .html(`
                            <strong>${{d.data.name}}</strong>
                            <div class="tip-row"><span>Size:</span><span class="tip-value">${{d.data.size_human || formatSize(d.value)}}</span></div>
                            <div class="tip-row"><span>Percentage:</span><span class="tip-value">${{pct}}%</span></div>
                            <div class="tip-row"><span>Files:</span><span class="tip-value">${{(d.data.file_count || 0).toLocaleString()}}</span></div>
                            <div class="tip-row"><span>Directories:</span><span class="tip-value">${{(d.data.dir_count || 0).toLocaleString()}}</span></div>
                            ${{d.children && d.children.length && !d.data.is_aggregated ? '<div style="margin-top:8px;color:#00d4ff;font-size:0.9em;">Click to zoom in →</div>' : ''}}
                        `);
                    positionTooltip(event);
                }})
                .on('mousemove', (event) => {{
                    positionTooltip(event);
                }})
                .on('mouseout', () => tooltip.style('opacity', 0));

            // Header bar for folders
            cells.filter(d => d.children && d.children.length > 0)
                .append('rect')
                .attr('width', d => Math.max(0, d.x1 - d.x0))
                .attr('height', 20)
                .attr('fill', 'rgba(0,0,0,0.3)')
                .attr('rx', 4)
                .style('pointer-events', 'none');

            // Labels
            cells.append('text')
                .attr('class', 'node-label')
                .attr('x', 6)
                .attr('y', 14)
                .text(d => {{
                    const w = d.x1 - d.x0;
                    if (w < 40) return '';
                    const name = d.data.name || '';
                    const maxChars = Math.floor(w / 7);
                    return name.length > maxChars ? name.slice(0, maxChars - 1) + '…' : name;
                }});

            // Size labels
            cells.append('text')
                .attr('class', 'node-size')
                .attr('x', 6)
                .attr('y', d => {{
                    const h = d.y1 - d.y0;
                    return h > 45 ? 35 : h - 5;
                }})
                .text(d => {{
                    const w = d.x1 - d.x0;
                    const h = d.y1 - d.y0;
                    if (w < 60 || h < 35) return '';
                    return d.data.size_human || formatSize(d.value);
                }});

            // Percentage labels for larger cells
            cells.append('text')
                .attr('class', 'node-size')
                .attr('x', 6)
                .attr('y', d => {{
                    const h = d.y1 - d.y0;
                    return h > 60 ? 50 : -100;
                }})
                .text(d => {{
                    const w = d.x1 - d.x0;
                    const h = d.y1 - d.y0;
                    if (w < 60 || h < 55) return '';
                    const pct = node.value ? ((d.value / node.value) * 100).toFixed(1) : 0;
                    return pct + '%';
                }});

            updateBreadcrumb();
        }}

        // Initial render
        render(root);

        // Handle window resize
        let resizeTimeout;
        window.addEventListener('resize', () => {{
            clearTimeout(resizeTimeout);
            resizeTimeout = setTimeout(() => {{
                const newWidth = container.clientWidth || 1200;
                const newHeight = container.clientHeight || Math.max(400, Math.min(800, window.innerHeight - 450));

                svg.attr('width', newWidth).attr('height', newHeight);
                treemap.size([newWidth, newHeight]);
                render(currentRoot);
            }}, 150);
        }});
    </script>
</body>
</html>"#,
        root_name = root_name,
        disk_total_fmt = format_size(disk_total, BINARY),
        disk_used_fmt = format_size(disk_used, BINARY),
        disk_free_fmt = format_size(disk_free, BINARY),
        usage_pct = usage_pct,
        scanned_fmt = format_size(total_size, BINARY),
        files_fmt = format_with_commas(total_files),
        dirs_fmt = format_with_commas(total_dirs),
        scan_time = scan_time,
        json_data = json_data
    )
}

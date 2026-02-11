#![windows_subsystem = "windows"]

use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::UNIX_EPOCH;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{AddFontResourceW, RemoveFontResourceW};
use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, HWND_BROADCAST, WM_FONTCHANGE};

#[derive(Default)]
struct AppState {
    loaded: HashSet<String>,
}

#[derive(Clone, Serialize)]
struct ProcessResult {
    loaded: usize,
    failed: usize,
    missing: usize,
    duplicates: usize,
    subs: usize,
    fonts: usize,
    logs: Vec<String>,
}

#[derive(Clone, Serialize)]
struct UnloadResult {
    count: usize,
}

#[derive(Serialize, Deserialize, Default)]
struct CacheFile {
    entries: HashMap<String, CacheEntry>,
}

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    modified: u64,
    names: Vec<String>,
}

enum WorkerResult {
    Process(Result<ProcessResult, String>),
    Unload(Result<UnloadResult, String>),
    Clean(Result<UnloadResult, String>),
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Tab {
    Operate,
    Logs,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Mode {
    NoResidue,
    Normal,
}

struct FontLoaderApp {
    tab: Tab,
    mode: Mode,
    logs: Vec<String>,
    state: Arc<Mutex<AppState>>,
    busy: bool,
    worker_rx: Option<mpsc::Receiver<WorkerResult>>,
    last_summary: Option<ProcessResult>,
    dark_mode: bool,
    pending_paths: Vec<String>,
}

impl FontLoaderApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_custom_fonts(&cc.egui_ctx);
        cc.egui_ctx.set_visuals(egui::Visuals::dark());

        let mut style = (*cc.egui_ctx.style()).clone();
        style.text_styles = [
            (
                egui::TextStyle::Heading,
                egui::FontId::new(28.0, egui::FontFamily::Proportional),
            ),
            (
                egui::TextStyle::Body,
                egui::FontId::new(20.0, egui::FontFamily::Proportional),
            ),
            (
                egui::TextStyle::Monospace,
                egui::FontId::new(18.0, egui::FontFamily::Monospace),
            ),
            (
                egui::TextStyle::Button,
                egui::FontId::new(20.0, egui::FontFamily::Proportional),
            ),
            (
                egui::TextStyle::Small,
                egui::FontId::new(16.0, egui::FontFamily::Proportional),
            ),
        ]
        .into();
        cc.egui_ctx.set_style(style);

        Self {
            tab: Tab::Operate,
            mode: Mode::NoResidue,
            logs: Vec::new(),
            state: Arc::new(Mutex::new(AppState::default())),
            busy: false,
            worker_rx: None,
            last_summary: None,
            dark_mode: true,
            pending_paths: Vec::new(),
        }
    }

    fn append_logs(&mut self, items: impl IntoIterator<Item = String>) {
        for item in items {
            self.logs.push(item);
        }
    }

    fn enqueue_paths(&mut self, paths: Vec<PathBuf>) {
        let paths: Vec<String> = paths
            .into_iter()
            .filter_map(|p| p.to_str().map(|s| s.to_string()))
            .collect();
        if paths.is_empty() {
            return;
        }
        let mut added = 0;
        for path in paths {
            if !self.pending_paths.contains(&path) {
                self.pending_paths.push(path);
                added += 1;
            }
        }
        if added > 0 {
            self.logs.push(format!("[i] 已加入待处理: {}", added));
        }
    }

    fn handle_process_pending(&mut self) {
        if self.busy {
            self.logs.push("[i] 正在处理，请稍候".to_string());
            return;
        }
        if self.pending_paths.is_empty() {
            self.logs.push("[i] 没有待处理的路径".to_string());
            return;
        }
        let paths = std::mem::take(&mut self.pending_paths);
        let use_cache = self.mode == Mode::Normal;
        let state = self.state.clone();
        let (tx, rx) = mpsc::channel();
        self.worker_rx = Some(rx);
        self.busy = true;
        thread::spawn(move || {
            let result = process_drop_worker(paths, use_cache, state);
            let _ = tx.send(WorkerResult::Process(result));
        });
    }

    fn handle_unload(&mut self) {
        if self.busy {
            self.logs.push("[i] 正在处理，请稍候".to_string());
            return;
        }
        let state = self.state.clone();
        let (tx, rx) = mpsc::channel();
        self.worker_rx = Some(rx);
        self.busy = true;
        thread::spawn(move || {
            let result = unload_fonts_worker(state);
            let _ = tx.send(WorkerResult::Unload(result));
        });
    }

    fn handle_clean(&mut self, folder: PathBuf) {
        if self.busy {
            self.logs.push("[i] 正在处理，请稍候".to_string());
            return;
        }
        let folder_str = folder.to_string_lossy().to_string();
        self.logs
            .push(format!("[i] 正在强力清理目录: {}", folder_str));
        let (tx, rx) = mpsc::channel();
        self.worker_rx = Some(rx);
        self.busy = true;
        thread::spawn(move || {
            let result = clean_folder_worker(folder);
            let _ = tx.send(WorkerResult::Clean(result));
        });
    }

    fn poll_worker(&mut self) {
        let Some(rx) = self.worker_rx.take() else {
            return;
        };
        let mut finished = false;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                WorkerResult::Process(result) => {
                    self.busy = false;
                    finished = true;
                    match result {
                        Ok(res) => {
                            let summary = format!(
                                "完成: 字幕{} 字体{} 已载入{} 失败{} 缺失{} 重复{}",
                                res.subs, res.fonts, res.loaded, res.failed, res.missing, res.duplicates
                            );
                            self.append_logs(res.logs.clone());
                            self.logs.push(summary);
                            self.last_summary = Some(res);
                        }
                        Err(err) => {
                            self.logs.push(format!("[X] {}", err));
                        }
                    }
                }
                WorkerResult::Unload(result) => {
                    self.busy = false;
                    finished = true;
                    match result {
                        Ok(res) => {
                            self.logs.push(format!("卸载完成: {}", res.count));
                            self.last_summary = Some(ProcessResult {
                                loaded: 0,
                                failed: 0,
                                missing: 0,
                                duplicates: 0,
                                subs: 0,
                                fonts: 0,
                                logs: Vec::new(),
                            });
                        }
                        Err(err) => {
                            self.logs.push(format!("[X] {}", err));
                        }
                    }
                }
                WorkerResult::Clean(result) => {
                    self.busy = false;
                    finished = true;
                    match result {
                        Ok(res) => {
                            self.logs.push(format!("强力清理完成，释放了 {} 个字体引用", res.count));
                        }
                        Err(err) => {
                            self.logs.push(format!("[X] {}", err));
                        }
                    }
                }
            }
        }
        if finished {
            self.worker_rx = None;
        } else {
            self.worker_rx = Some(rx);
        }
    }
}

impl eframe::App for FontLoaderApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_worker();
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if !dropped.is_empty() {
            let paths: Vec<PathBuf> = dropped.into_iter().filter_map(|f| f.path).collect();
            if !paths.is_empty() {
                self.enqueue_paths(paths);
            }
        }

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let operate = ui.selectable_label(self.tab == Tab::Operate, "操作");
                if operate.clicked() {
                    self.tab = Tab::Operate;
                }
                let logs = ui.selectable_label(self.tab == Tab::Logs, "日志");
                if logs.clicked() {
                    self.tab = Tab::Logs;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut dark = self.dark_mode;
                    if ui.checkbox(&mut dark, "暗色").changed() {
                        self.dark_mode = dark;
                        if dark {
                            ctx.set_visuals(egui::Visuals::dark());
                        } else {
                            ctx.set_visuals(egui::Visuals::light());
                        }
                    }
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Operate => {
                ui.vertical(|ui| {
                    ui.label("将字幕/字体文件或文件夹拖入窗口，加入待处理后再点击开始处理");
                    ui.add_space(4.0);
                    
                    let available_width = ui.available_width();
                    let spacing = ui.spacing().item_spacing.x;
                    let row_height = 35.0;

                    // 第一行：选文件，选文件夹
                    ui.horizontal(|ui| {
                        let btn_w = (available_width - spacing) / 2.0;
                        if ui.add_sized([btn_w, row_height], egui::Button::new("选文件")).clicked() {
                            if let Some(files) = rfd::FileDialog::new().pick_files() {
                                self.enqueue_paths(files);
                            }
                        }
                        if ui.add_sized([btn_w, row_height], egui::Button::new("选文件夹")).clicked() {
                            if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                                self.enqueue_paths(vec![folder]);
                            }
                        }
                    });

                    ui.add_space(4.0);

                    // 第二行：开始处理（加载），卸载
                    ui.horizontal(|ui| {
                        let btn_w = (available_width - spacing) / 2.0;
                        if ui.add_sized([btn_w, row_height], egui::Button::new("加载字体")).clicked() {
                            self.handle_process_pending();
                        }
                        if ui.add_sized([btn_w, row_height], egui::Button::new("卸载已加载字体")).clicked() {
                            self.handle_unload();
                        }
                    });

                    ui.add_space(4.0);

                    // 第三行：强制清理
                    if ui
                        .add_sized([available_width, row_height], egui::Button::new("⚠强制清理目录残留"))
                        .on_hover_text("选择一个文件夹，尝试强制卸载其中所有字体文件的系统占用（无论是否由本程序加载）")
                        .clicked()
                    {
                        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                            self.handle_clean(folder);
                        }
                    }

                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        let mut mode = self.mode;
                        ui.label("模式:");
                        if ui.radio_value(&mut mode, Mode::NoResidue, "无残留").clicked() {
                            self.mode = Mode::NoResidue;
                        }
                        if ui.radio_value(&mut mode, Mode::Normal, "普通").clicked() {
                            self.mode = Mode::Normal;
                        }
                    });

                    ui.label(format!("待处理路径: {}", self.pending_paths.len()));
                    if let Some(summary) = &self.last_summary {
                        ui.label(format!(
                            "摘要: 字幕{} 字体{} 已载入{} 失败{} 缺失{} 重复{}",
                            summary.subs,
                            summary.fonts,
                            summary.loaded,
                            summary.failed,
                            summary.missing,
                            summary.duplicates
                        ));
                    }

                    if self.busy {
                        ui.label("处理中...");
                    }
                });
            }
            Tab::Logs => {
                egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                    for line in &self.logs {
                        ui.label(line);
                    }
                });
            }
        });
    }
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Ok(state) = self.state.lock() {
            let mut count = 0;
            for path in state.loaded.iter() {
                if remove_font_resource(path) {
                    count += 1;
                }
            }
            if count > 0 {
                broadcast_font_change();
            }
        }
    }
}

fn process_drop_worker(
    paths: Vec<String>,
    use_cache: bool,
    state: Arc<Mutex<AppState>>,
) -> Result<ProcessResult, String> {
    let file_list = collect_files(&paths)?;
    let mut sub_files = Vec::new();
    let mut font_files = Vec::new();
    for path in file_list {
        if is_sub_file(&path) {
            sub_files.push(path);
        } else if is_font_file(&path) {
            font_files.push(path);
        }
    }

    let mut required_fonts = HashSet::new();
    let mut unsupported_subs = Vec::new();
    for sub in &sub_files {
        if is_ass_file(sub) {
            if let Some(text) = read_text(sub) {
                for font in parse_ass_fonts(&text) {
                    required_fonts.insert(font);
                }
            }
        } else {
            unsupported_subs.push(sub.to_string_lossy().to_string());
        }
    }

    let mut cache = if use_cache {
        load_cache_file()
    } else {
        CacheFile::default()
    };
    let font_index = build_font_index(&font_files, use_cache, &mut cache);
    if use_cache {
        let _ = save_cache_file(&cache);
    }

    let mut logs = Vec::new();
    for sub in unsupported_subs {
        logs.push(format!("[i] 跳过不支持解析的字幕: {}", sub));
    }
    let mut loaded = 0;
    let mut failed = 0;
    let mut missing = 0;
    let mut duplicates = 0;

    let mut state = state.lock().map_err(|_| "状态锁失败".to_string())?;
    for font in required_fonts.iter() {
        let key = font.to_lowercase();
        if let Some(files) = font_index.get(&key) {
            if let Some(path) = files.first() {
                let path_str = path.to_string_lossy().to_string();
                if state.loaded.contains(&path_str) {
                    duplicates += 1;
                    logs.push(format!("[^] {} > {}", font, path_str));
                } else if add_font_resource(&path_str) {
                    state.loaded.insert(path_str.clone());
                    loaded += 1;
                    logs.push(format!("[ok] {} > {}", font, path_str));
                } else {
                    failed += 1;
                    logs.push(format!("[X] {} > {}", font, path_str));
                }
            } else {
                missing += 1;
                logs.push(format!("[??] {}", font));
            }
        } else {
            missing += 1;
            logs.push(format!("[??] {}", font));
        }
    }

    if loaded > 0 {
        broadcast_font_change();
    }

    Ok(ProcessResult {
        loaded,
        failed,
        missing,
        duplicates,
        subs: sub_files.len(),
        fonts: font_files.len(),
        logs,
    })
}

fn unload_fonts_worker(state: Arc<Mutex<AppState>>) -> Result<UnloadResult, String> {
    let mut state = state.lock().map_err(|_| "状态锁失败".to_string())?;
    let mut count = 0;
    let mut removed = Vec::new();
    for path in state.loaded.iter() {
        if remove_font_resource(path) {
            count += 1;
            removed.push(path.clone());
        }
    }
    for path in removed {
        state.loaded.remove(&path);
    }
    if count > 0 {
        broadcast_font_change();
    }
    Ok(UnloadResult { count })
}

fn clean_folder_worker(folder: PathBuf) -> Result<UnloadResult, String> {
    let mut files = Vec::new();
    let _ = walk_dir(&folder, &mut files);
    let mut count = 0;
    for path in files {
        if is_font_file(&path) {
            let path_str = path.to_string_lossy().to_string();
            while remove_font_resource(&path_str) {
                count += 1;
            }
        }
    }
    if count > 0 {
        broadcast_font_change();
    }
    Ok(UnloadResult { count })
}

fn build_font_index(
    font_files: &[PathBuf],
    use_cache: bool,
    cache: &mut CacheFile,
) -> HashMap<String, Vec<PathBuf>> {
    let mut index: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for path in font_files {
        let path_str = path.to_string_lossy().to_string();
        let names = if use_cache {
            if let Some(entry) = cache.entries.get(&path_str) {
                if metadata_mtime(path) == Some(entry.modified) {
                    entry.names.clone()
                } else {
                    let names = parse_font_names(path);
                    cache.entries.insert(
                        path_str.clone(),
                        CacheEntry {
                            modified: metadata_mtime(path).unwrap_or(0),
                            names: names.clone(),
                        },
                    );
                    names
                }
            } else {
                let names = parse_font_names(path);
                cache.entries.insert(
                    path_str.clone(),
                    CacheEntry {
                        modified: metadata_mtime(path).unwrap_or(0),
                        names: names.clone(),
                    },
                );
                names
            }
        } else {
            parse_font_names(path)
        };
        for name in names {
            let key = name.to_lowercase();
            index.entry(key).or_default().push(path.clone());
        }
    }
    index
}

fn metadata_mtime(path: &Path) -> Option<u64> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let duration = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(duration.as_secs())
}

fn read_text(path: &Path) -> Option<String> {
    let data = fs::read(path).ok()?;
    if data.starts_with(&[0xFF, 0xFE]) {
        return decode_utf16(&data[2..], true);
    }
    if data.starts_with(&[0xFE, 0xFF]) {
        return decode_utf16(&data[2..], false);
    }
    if data.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8(data[3..].to_vec()).ok();
    }
    String::from_utf8(data).ok()
}

fn decode_utf16(data: &[u8], little_endian: bool) -> Option<String> {
    if data.len() % 2 != 0 {
        return None;
    }
    let mut buf = Vec::with_capacity(data.len() / 2);
    let mut i = 0;
    while i + 1 < data.len() {
        let value = if little_endian {
            u16::from_le_bytes([data[i], data[i + 1]])
        } else {
            u16::from_be_bytes([data[i], data[i + 1]])
        };
        buf.push(value);
        i += 2;
    }
    Some(String::from_utf16_lossy(&buf))
}

fn parse_ass_fonts(text: &str) -> HashSet<String> {
    let mut fonts = HashSet::new();
    let mut section = String::new();
    let mut style_font_idx: Option<usize> = None;
    let mut event_text_idx: Option<usize> = None;

    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].to_lowercase();
            continue;
        }
        let lower = line.to_lowercase();
        if section.contains("styles") {
            if lower.starts_with("format:") {
                let format = parse_format(line, 7);
                style_font_idx = format.iter().position(|v| v == "fontname");
            } else if lower.starts_with("style:") {
                if let Some(font) = parse_style_font(line, style_font_idx) {
                    fonts.insert(font);
                }
            }
        } else if section.contains("events") {
            if lower.starts_with("format:") {
                let format = parse_format(line, 7);
                event_text_idx = format.iter().position(|v| v == "text");
            } else if lower.starts_with("dialogue:") || lower.starts_with("comment:") {
                if let Some(text) = extract_event_text(line, event_text_idx) {
                    for font in parse_fn_tags(&text) {
                        fonts.insert(font);
                    }
                }
            }
        }
    }

    fonts
}

fn parse_format(line: &str, start: usize) -> Vec<String> {
    let content = line[start..].trim();
    content
        .split(',')
        .map(|v| v.trim().to_lowercase())
        .collect()
}

fn parse_style_font(line: &str, idx: Option<usize>) -> Option<String> {
    let content = line[6..].trim();
    let parts: Vec<&str> = content.split(',').collect();
    let raw = if let Some(i) = idx {
        parts.get(i)
    } else {
        parts.get(1)
    }?;
    normalize_font_name(raw)
}

fn extract_event_text(line: &str, idx: Option<usize>) -> Option<String> {
    let content = line[9..].trim();
    let index = idx.unwrap_or(9);
    let mut count = 0;
    let mut split_at = None;
    for (pos, ch) in content.char_indices() {
        if ch == ',' {
            if count == index {
                split_at = Some(pos + 1);
                break;
            }
            count += 1;
        }
    }
    let text = match split_at {
        Some(pos) => &content[pos..],
        None => "",
    };
    Some(text.to_string())
}

fn parse_fn_tags(text: &str) -> Vec<String> {
    let mut res = Vec::new();
    let mut start = 0;
    while let Some(pos) = text[start..].find("\\fn") {
        let idx = start + pos + 3;
        let mut s = &text[idx..];
        s = s.trim_start();
        if s.starts_with('(') {
            if let Some(end) = s[1..].find(')') {
                let name = &s[1..1 + end];
                if let Some(normalized) = normalize_font_name(name) {
                    res.push(normalized);
                }
                start = idx + 1 + end + 1;
                continue;
            }
        }
        let mut end = s.len();
        for (i, ch) in s.char_indices() {
            if ch == '\\' || ch == '}' {
                end = i;
                break;
            }
        }
        let name = &s[..end];
        if let Some(normalized) = normalize_font_name(name) {
            res.push(normalized);
        }
        start = idx + end;
    }
    res
}

fn normalize_font_name(name: &str) -> Option<String> {
    let mut s = name.trim().trim_matches('\u{0}').to_string();
    if s.starts_with('@') {
        s.remove(0);
    }
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn parse_font_names(path: &Path) -> Vec<String> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(_) => return Vec::new(),
    };
    parse_font_names_from_bytes(&data)
}

fn parse_font_names_from_bytes(data: &[u8]) -> Vec<String> {
    let mut names = HashSet::new();
    if data.len() < 4 {
        return Vec::new();
    }
    if &data[0..4] == b"ttcf" {
        for offset in parse_ttc_offsets(data) {
            for name in parse_otf_names_at(data, offset) {
                names.insert(name);
            }
        }
    } else {
        for name in parse_otf_names_at(data, 0) {
            names.insert(name);
        }
    }
    names.into_iter().collect()
}

fn parse_ttc_offsets(data: &[u8]) -> Vec<usize> {
    if data.len() < 12 {
        return Vec::new();
    }
    let num_fonts = read_u32_be(data, 8).unwrap_or(0) as usize;
    let mut offsets = Vec::new();
    let mut pos = 12;
    for _ in 0..num_fonts {
        if let Some(val) = read_u32_be(data, pos) {
            offsets.push(val as usize);
        }
        pos += 4;
    }
    offsets
}

fn parse_otf_names_at(data: &[u8], offset: usize) -> Vec<String> {
    if data.len() < offset + 12 {
        return Vec::new();
    }
    let num_tables = read_u16_be(data, offset + 4).unwrap_or(0) as usize;
    let table_start = offset + 12;
    let mut name_table = None;
    for i in 0..num_tables {
        let rec = table_start + i * 16;
        if data.len() < rec + 16 {
            break;
        }
        let tag = &data[rec..rec + 4];
        if tag == b"name" {
            let table_offset = read_u32_be(data, rec + 8).unwrap_or(0) as usize;
            let length = read_u32_be(data, rec + 12).unwrap_or(0) as usize;
            name_table = Some((table_offset, length));
            break;
        }
    }
    let Some((table_offset, length)) = name_table else {
        return Vec::new();
    };
    let table_pos = offset + table_offset;
    if data.len() < table_pos + length || data.len() < table_pos + 6 {
        return Vec::new();
    }
    let count = read_u16_be(data, table_pos + 2).unwrap_or(0) as usize;
    let string_offset = read_u16_be(data, table_pos + 4).unwrap_or(0) as usize;
    let records_start = table_pos + 6;
    let mut result = HashSet::new();
    for i in 0..count {
        let rec = records_start + i * 12;
        if data.len() < rec + 12 {
            break;
        }
        let platform = read_u16_be(data, rec).unwrap_or(0);
        let name_id = read_u16_be(data, rec + 6).unwrap_or(0);
        let length = read_u16_be(data, rec + 8).unwrap_or(0) as usize;
        let offset_str = read_u16_be(data, rec + 10).unwrap_or(0) as usize;
        if platform != 3 {
            continue;
        }
        if name_id != 1 && name_id != 4 {
            continue;
        }
        let str_start = table_pos + string_offset + offset_str;
        let str_end = str_start + length;
        if data.len() < str_end || length == 0 {
            continue;
        }
        let name = decode_utf16be(&data[str_start..str_end]);
        if let Some(normalized) = normalize_font_name(&name) {
            result.insert(normalized);
        }
    }
    result.into_iter().collect()
}

fn decode_utf16be(data: &[u8]) -> String {
    let mut buf = Vec::with_capacity(data.len() / 2);
    let mut i = 0;
    while i + 1 < data.len() {
        buf.push(u16::from_be_bytes([data[i], data[i + 1]]));
        i += 2;
    }
    String::from_utf16_lossy(&buf)
}

fn read_u16_be(data: &[u8], offset: usize) -> Option<u16> {
    if data.len() < offset + 2 {
        None
    } else {
        Some(u16::from_be_bytes([data[offset], data[offset + 1]]))
    }
}

fn read_u32_be(data: &[u8], offset: usize) -> Option<u32> {
    if data.len() < offset + 4 {
        None
    } else {
        Some(u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]))
    }
}

fn is_sub_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|v| v.to_str()).map(|v| v.to_lowercase()),
        Some(ext)
            if ext == "ass"
                || ext == "ssa"
                || ext == "srt"
                || ext == "vtt"
                || ext == "sub"
                || ext == "idx"
                || ext == "sup"
    )
}

fn is_ass_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|v| v.to_str()).map(|v| v.to_lowercase()),
        Some(ext) if ext == "ass" || ext == "ssa"
    )
}

fn is_font_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|v| v.to_str()).map(|v| v.to_lowercase()),
        Some(ext) if ext == "ttf" || ext == "otf" || ext == "ttc"
    )
}

fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn add_font_resource(path: &str) -> bool {
    let wide = to_wide(path);
    unsafe { AddFontResourceW(PCWSTR(wide.as_ptr())) > 0 }
}

fn remove_font_resource(path: &str) -> bool {
    let wide = to_wide(path);
    unsafe { RemoveFontResourceW(PCWSTR(wide.as_ptr())).0 != 0 }
}

fn broadcast_font_change() {
    unsafe {
        SendMessageW(HWND_BROADCAST, WM_FONTCHANGE, WPARAM(0), LPARAM(0));
    }
}

fn cache_file_path() -> Option<PathBuf> {
    let exe_path = std::env::current_exe().ok()?;
    let exe_dir = exe_path.parent()?;
    Some(exe_dir.join("cache.json"))
}

fn load_cache_file() -> CacheFile {
    let Some(path) = cache_file_path() else {
        return CacheFile::default();
    };
    let data = fs::read(path).ok();
    if let Some(bytes) = data {
        serde_json::from_slice(&bytes).unwrap_or_default()
    } else {
        CacheFile::default()
    }
}

fn save_cache_file(cache: &CacheFile) -> Result<(), String> {
    let Some(path) = cache_file_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let data = serde_json::to_vec_pretty(cache).map_err(|e| e.to_string())?;
    fs::write(path, data).map_err(|e| e.to_string())?;
    Ok(())
}

fn collect_files(paths: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for raw in paths {
        let path = PathBuf::from(raw);
        if path.is_file() {
            files.push(path);
        } else if path.is_dir() {
            let _ = walk_dir(&path, &mut files);
        }
    }
    Ok(files)
}

fn walk_dir(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let _ = walk_dir(&path, out);
            } else if path.is_file() {
                out.push(path);
            }
        }
    }
    Ok(())
}

fn setup_custom_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // 1. 微软雅黑 (主字体)
    let msyh_path = PathBuf::from("C:\\Windows\\Fonts\\msyh.ttc");
    if let Ok(font_data) = fs::read(&msyh_path) {
        fonts.font_data.insert(
            "msyh".to_owned(),
            std::sync::Arc::new(egui::FontData::from_owned(font_data)),
        );
        fonts.families.get_mut(&egui::FontFamily::Proportional).unwrap().insert(0, "msyh".to_owned());
        fonts.families.get_mut(&egui::FontFamily::Monospace).unwrap().push("msyh".to_owned());
    }

    // 2. Segoe UI Symbol (符号备选)
    let symbol_path = PathBuf::from("C:\\Windows\\Fonts\\seguisym.ttf");
    if let Ok(font_data) = fs::read(&symbol_path) {
        fonts.font_data.insert(
            "symbols".to_owned(),
            std::sync::Arc::new(egui::FontData::from_owned(font_data)),
        );
        fonts.families.get_mut(&egui::FontFamily::Proportional).unwrap().push("symbols".to_owned());
    }

    ctx.set_fonts(fonts);
}

fn main() -> eframe::Result<()> {
    let mut options = eframe::NativeOptions::default();
    options.viewport.min_inner_size = Some(egui::vec2(400.0, 400.0));
    eframe::run_native(
        "NewFontLoader (egui)",
        options,
        Box::new(|cc| Ok(Box::new(FontLoaderApp::new(cc)))),
    )
}

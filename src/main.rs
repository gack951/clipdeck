#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use arboard::{Clipboard as ImageClipboard, ImageData};
use chrono::{DateTime, Local, Utc};
#[cfg(windows)]
use clipboard_win::{Clipboard as WinClipboard, Getter, Setter, formats};
use eframe::egui::{self, ColorImage, TextureHandle};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};
#[cfg(not(windows))]
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};
#[cfg(windows)]
use std::{ffi::OsStr, os::windows::ffi::OsStrExt, ptr};

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn GetClipboardSequenceNumber() -> u32;
}
#[cfg(windows)]
#[link(name = "shell32")]
unsafe extern "system" {
    fn ShellExecuteW(
        hwnd: *mut std::ffi::c_void,
        operation: *const u16,
        file: *const u16,
        parameters: *const u16,
        directory: *const u16,
        show: i32,
    ) -> isize;
}

const MAX_ITEMS: usize = 300;
const MAX_TEXT: usize = 2_000_000;
const MAX_PIXELS: usize = 16_000_000;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
enum Content {
    Text(String),
    Files(Vec<String>),
    Image(String),
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    id: String,
    created: DateTime<Utc>,
    pinned: bool,
    content: Content,
}

#[derive(Default, Serialize, Deserialize)]
struct History {
    entries: Vec<Entry>,
}

fn storage() -> PathBuf {
    let root = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    root.join("ClipDeck")
}
fn save(history: &History) {
    let root = storage();
    if fs::create_dir_all(&root).is_ok() {
        if let Ok(data) = serde_json::to_vec(history) {
            let tmp = root.join("history.json.tmp");
            if fs::write(&tmp, data).is_ok() {
                let _ = fs::rename(tmp, root.join("history.json"));
            }
        }
    }
}
fn load() -> History {
    fs::read(storage().join("history.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}
fn cleanup_image(entry: &Entry) {
    if let Content::Image(name) = &entry.content {
        if !name.contains('/') && !name.contains('\\') {
            let _ = fs::remove_file(storage().join("images").join(name));
        }
    }
}
#[cfg(windows)]
fn file_paths() -> Option<Vec<String>> {
    let _guard = WinClipboard::new_attempts(3).ok()?;
    let mut files = Vec::<String>::new();
    formats::FileList.read_clipboard(&mut files).ok()?;
    if files.is_empty() { None } else { Some(files) }
}
#[cfg(windows)]
fn read_content() -> Option<Content> {
    // CF_HDROPを最優先する。Explorerのファイルコピーを単なるパス文字列に変換しないため。
    if let Some(files) = file_paths() {
        return Some(Content::Files(files));
    }
    if let Ok(mut clipboard) = ImageClipboard::new() {
        if let Ok(img) = clipboard.get_image() {
            let (width, height) = (img.width, img.height);
            if width > 0 && height > 0 && width.saturating_mul(height) <= MAX_PIXELS {
                let name = format!(
                    "{}-{}.png",
                    Utc::now().timestamp_micros(),
                    std::process::id()
                );
                let path = storage().join("images").join(&name);
                if fs::create_dir_all(path.parent()?).is_ok() {
                    if image::save_buffer(
                        &path,
                        &img.bytes,
                        width as u32,
                        height as u32,
                        image::ColorType::Rgba8,
                    )
                    .is_ok()
                    {
                        return Some(Content::Image(name));
                    }
                }
            }
        }
        if let Ok(text) = clipboard.get_text() {
            if !text.trim().is_empty() && text.len() <= MAX_TEXT {
                return Some(Content::Text(text));
            }
        }
    }
    None
}
fn restore(content: &Content) -> Result<(), String> {
    match content {
        Content::Text(text) => {
            let mut clipboard = ImageClipboard::new().map_err(|e| e.to_string())?;
            clipboard.set_text(text).map_err(|e| e.to_string())?;
        }
        Content::Files(files) => {
            restore_files(files)?;
        }
        Content::Image(name) => {
            let path = storage().join("images").join(name);
            let pixels = image::open(path).map_err(|e| e.to_string())?.to_rgba8();
            let (width, height) = pixels.dimensions();
            let mut clipboard = ImageClipboard::new().map_err(|e| e.to_string())?;
            clipboard
                .set_image(ImageData {
                    width: width as usize,
                    height: height as usize,
                    bytes: Cow::Owned(pixels.into_raw()),
                })
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
#[cfg(windows)]
fn restore_files(files: &[String]) -> Result<(), String> {
    let guard = WinClipboard::new_attempts(10).map_err(|e| e.to_string())?;
    formats::FileList
        .write_clipboard(files)
        .map_err(|e| e.to_string())?;
    drop(guard);
    Ok(())
}
#[cfg(not(windows))]
fn restore_files(_files: &[String]) -> Result<(), String> {
    Err("ファイル一覧のクリップボード復元はWindowsのみ対応しています".to_owned())
}
fn as_path(text: &str) -> Option<PathBuf> {
    let candidate = text.trim().trim_matches('"');
    if candidate.contains('\n') || candidate.contains('\r') {
        return None;
    }
    let path = PathBuf::from(candidate);
    if path.exists() { Some(path) } else { None }
}
fn as_url(text: &str) -> Option<String> {
    let candidate = text.trim();
    if candidate.contains(char::is_whitespace) {
        return None;
    }
    let url = url::Url::parse(candidate).ok()?;
    if matches!(url.scheme(), "http" | "https") && url.host_str().is_some() {
        Some(candidate.to_owned())
    } else {
        None
    }
}
#[cfg(windows)]
fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}
#[cfg(windows)]
fn shell_open(target: &str) -> Result<(), String> {
    let action = wide(OsStr::new("open"));
    let target = wide(OsStr::new(target));
    // ShellExecuteWへURLまたはパスを渡す。シェルコマンド文字列の評価を回避するため。
    let result = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            action.as_ptr(),
            target.as_ptr(),
            ptr::null(),
            ptr::null(),
            1,
        )
    };
    if result > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecuteW failed: {result}"))
    }
}
#[cfg(not(windows))]
fn shell_open(target: &str) -> Result<(), String> {
    Command::new("xdg-open")
        .arg(target)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(windows)]
fn explorer_folder(path: &Path) -> Result<(), String> {
    Command::new("explorer.exe")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}
#[cfg(not(windows))]
fn explorer_folder(path: &Path) -> Result<(), String> {
    Command::new("xdg-open")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}
#[cfg(windows)]
fn explorer_select(path: &Path) -> Result<(), String> {
    Command::new("explorer.exe")
        .arg(format!("/select,{}", path.display()))
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}
#[cfg(not(windows))]
fn explorer_select(path: &Path) -> Result<(), String> {
    explorer_folder(path.parent().unwrap_or(path))
}
fn content_key(c: &Content) -> String {
    match c {
        Content::Text(s) => format!("t:{s}"),
        Content::Files(f) => format!("f:{}", f.join("\0")),
        Content::Image(s) => format!("i:{s}"),
    }
}
fn label(c: &Content) -> String {
    match c {
        Content::Text(s) => s.replace(['\r', '\n'], "  ").chars().take(120).collect(),
        Content::Files(f) => format!(
            "{} 件のファイル・フォルダ: {}",
            f.len(),
            f.first().map(String::as_str).unwrap_or("")
        ),
        Content::Image(s) => format!("画像: {s}"),
    }
}
#[cfg(windows)]
fn watch(tx: mpsc::Sender<Content>, ctx: egui::Context) {
    thread::spawn(move || {
        let mut sequence = unsafe { GetClipboardSequenceNumber() };
        loop {
            thread::sleep(Duration::from_millis(300));
            let now = unsafe { GetClipboardSequenceNumber() };
            if now == 0 || now == sequence {
                continue;
            }
            sequence = now;
            if let Some(item) = read_content() {
                if tx.send(item).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        }
    });
}
#[cfg(not(windows))]
fn watch(tx: mpsc::Sender<Content>, ctx: egui::Context) {
    thread::spawn(move || {
        let mut last = 0;
        loop {
            thread::sleep(Duration::from_millis(300));
            let Ok(mut clipboard) = ImageClipboard::new() else {
                continue;
            };
            let item = if let Ok(img) = clipboard.get_image() {
                let mut hasher = DefaultHasher::new();
                img.width.hash(&mut hasher);
                img.height.hash(&mut hasher);
                img.bytes.hash(&mut hasher);
                let fingerprint = hasher.finish();
                if fingerprint == last || img.width.saturating_mul(img.height) > MAX_PIXELS {
                    continue;
                }
                last = fingerprint;
                let name = format!(
                    "{}-{}.png",
                    Utc::now().timestamp_micros(),
                    std::process::id()
                );
                let path = storage().join("images").join(&name);
                if fs::create_dir_all(path.parent().unwrap()).is_err()
                    || image::save_buffer(
                        &path,
                        &img.bytes,
                        img.width as u32,
                        img.height as u32,
                        image::ColorType::Rgba8,
                    )
                    .is_err()
                {
                    continue;
                }
                Content::Image(name)
            } else if let Ok(text) = clipboard.get_text() {
                let mut hasher = DefaultHasher::new();
                text.hash(&mut hasher);
                let fingerprint = hasher.finish();
                if fingerprint == last || text.trim().is_empty() || text.len() > MAX_TEXT {
                    continue;
                }
                last = fingerprint;
                Content::Text(text)
            } else {
                continue;
            };
            if tx.send(item).is_err() {
                break;
            }
            ctx.request_repaint();
        }
    });
}

struct App {
    history: History,
    rx: Receiver<Content>,
    query: String,
    filter: usize,
    textures: HashMap<String, TextureHandle>,
    status: String,
    monitoring: bool,
}
impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let (tx, rx) = mpsc::channel();
        watch(tx, cc.egui_ctx.clone());
        Self {
            history: load(),
            rx,
            query: String::new(),
            filter: 0,
            textures: HashMap::new(),
            status: String::new(),
            monitoring: true,
        }
    }
    fn ingest(&mut self, content: Content) {
        if !self.monitoring {
            if let Content::Image(name) = &content {
                cleanup_image(&Entry {
                    id: String::new(),
                    created: Utc::now(),
                    pinned: false,
                    content: Content::Image(name.clone()),
                });
            }
            return;
        }
        let key = content_key(&content);
        if !matches!(content, Content::Image(_)) {
            if let Some(pos) = self
                .history
                .entries
                .iter()
                .position(|x| content_key(&x.content) == key)
            {
                let mut entry = self.history.entries.remove(pos);
                entry.created = Utc::now();
                self.history.entries.insert(0, entry);
                save(&self.history);
                return;
            }
        }
        let entry = Entry {
            id: format!(
                "{}-{}",
                Utc::now().timestamp_micros(),
                self.history.entries.len()
            ),
            created: Utc::now(),
            pinned: false,
            content,
        };
        self.history.entries.insert(0, entry);
        while self.history.entries.len() > MAX_ITEMS {
            if let Some(idx) = self.history.entries.iter().rposition(|e| !e.pinned) {
                let old = self.history.entries.remove(idx);
                cleanup_image(&old);
                self.textures.remove(&old.id);
            } else {
                break;
            }
        }
        save(&self.history);
    }
    fn texture(&mut self, ctx: &egui::Context, entry: &Entry) -> Option<TextureHandle> {
        let Content::Image(name) = &entry.content else {
            return None;
        };
        if let Some(texture) = self.textures.get(&entry.id) {
            return Some(texture.clone());
        }
        let img = image::open(storage().join("images").join(name))
            .ok()?
            .to_rgba8();
        let dimensions = [img.width() as usize, img.height() as usize];
        let color = ColorImage::from_rgba_unmultiplied(dimensions, &img.into_raw());
        let texture = ctx.load_texture(&entry.id, color, egui::TextureOptions::LINEAR);
        self.textures.insert(entry.id.clone(), texture.clone());
        Some(texture)
    }
    fn action(&mut self, result: Result<(), String>) {
        self.status = result
            .map(|_| "操作しました".to_owned())
            .unwrap_or_else(|e| format!("エラー: {e}"));
    }
}
impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        while let Ok(item) = self.rx.try_recv() {
            self.ingest(item);
        }
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("ClipDeck");
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.monitoring, "監視");
                ui.label(format!("履歴 {} 件", self.history.entries.len()));
                if ui.button("データフォルダ").clicked() {
                    self.action(explorer_folder(&storage()));
                }
                if ui.button("未固定履歴を削除").clicked() {
                    let removed: Vec<_> = self
                        .history
                        .entries
                        .iter()
                        .filter(|e| !e.pinned)
                        .cloned()
                        .collect();
                    self.history.entries.retain(|e| e.pinned);
                    for entry in &removed {
                        cleanup_image(entry);
                        self.textures.remove(&entry.id);
                    }
                    save(&self.history);
                }
            });
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text("履歴を検索（テキスト・ファイル名）")
                        .desired_width(360.),
                );
                egui::ComboBox::from_id_salt("filter")
                    .selected_text(["すべて", "テキスト", "ファイル", "画像"][self.filter])
                    .show_ui(ui, |ui| {
                        for (i, name) in ["すべて", "テキスト", "ファイル", "画像"]
                            .iter()
                            .enumerate()
                        {
                            ui.selectable_value(&mut self.filter, i, *name);
                        }
                    });
            });
            if !self.status.is_empty() {
                ui.label(&self.status);
            }
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                let visible: Vec<Entry> = self
                    .history
                    .entries
                    .iter()
                    .filter(|e| {
                        let kind = match &e.content {
                            Content::Text(_) => 1,
                            Content::Files(_) => 2,
                            Content::Image(_) => 3,
                        };
                        (self.filter == 0 || self.filter == kind)
                            && (self.query.is_empty()
                                || label(&e.content)
                                    .to_lowercase()
                                    .contains(&self.query.to_lowercase()))
                    })
                    .cloned()
                    .collect();
                for entry in visible {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                entry
                                    .created
                                    .with_timezone(&Local)
                                    .format("%m/%d %H:%M:%S")
                                    .to_string(),
                            );
                            if ui
                                .button(if entry.pinned {
                                    "★ 固定解除"
                                } else {
                                    "☆ 固定"
                                })
                                .clicked()
                            {
                                if let Some(e) =
                                    self.history.entries.iter_mut().find(|e| e.id == entry.id)
                                {
                                    e.pinned = !e.pinned;
                                    save(&self.history);
                                }
                            }
                            if ui.button("クリップボードに戻す").clicked() {
                                self.action(restore(&entry.content));
                            }
                            if ui.button("削除").clicked() {
                                self.history.entries.retain(|e| e.id != entry.id);
                                cleanup_image(&entry);
                                self.textures.remove(&entry.id);
                                save(&self.history);
                            }
                        });
                        match &entry.content {
                            Content::Text(text) => {
                                let preview: String = text.chars().take(700).collect();
                                ui.label(&preview);
                                if let Some(url) = as_url(text) {
                                    if ui.button("URLを開く").clicked() {
                                        self.action(shell_open(&url));
                                    }
                                }
                                if let Some(path) = as_path(text) {
                                    ui.horizontal(|ui| {
                                        if ui.button("開く").clicked() {
                                            self.action(shell_open(&path.to_string_lossy()));
                                        }
                                        if ui.button("Explorerで選択").clicked() {
                                            self.action(explorer_select(&path));
                                        }
                                        if let Some(parent) = path.parent() {
                                            if ui.button("親フォルダ").clicked() {
                                                self.action(explorer_folder(parent));
                                            }
                                        }
                                    });
                                }
                            }
                            Content::Files(files) => {
                                ui.label(format!("{} 件", files.len()));
                                for path in files.iter().take(12) {
                                    ui.label(path);
                                }
                                if files.len() > 12 {
                                    ui.label(
                                        "（ほかのファイルはクリップボード復元で利用できます）",
                                    );
                                }
                                if let Some(first) = files.first() {
                                    let p = Path::new(first);
                                    ui.horizontal(|ui| {
                                        if ui.button("先頭を開く").clicked() {
                                            self.action(shell_open(first));
                                        }
                                        if ui.button("Explorerで選択").clicked() {
                                            self.action(explorer_select(p));
                                        }
                                        if let Some(parent) = p.parent() {
                                            if ui.button("親フォルダ").clicked() {
                                                self.action(explorer_folder(parent));
                                            }
                                        }
                                    });
                                }
                            }
                            Content::Image(_) => {
                                if let Some(tex) = self.texture(ui.ctx(), &entry) {
                                    let size = tex.size_vec2();
                                    ui.add(
                                        egui::Image::new(&tex)
                                            .max_size(egui::vec2(420., 220.))
                                            .fit_to_exact_size(egui::vec2(
                                                size.x.min(420.),
                                                size.y.min(220.),
                                            )),
                                    );
                                } else {
                                    ui.label("画像データを読み込めません");
                                }
                            }
                        }
                    });
                    ui.add_space(5.);
                }
            });
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ClipDeck")
            .with_inner_size([920., 720.])
            .with_min_inner_size([570., 400.]),
        ..Default::default()
    };
    eframe::run_native(
        "ClipDeck",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

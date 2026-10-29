#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use arboard::{Clipboard as ImageClipboard, ImageData};
use chrono::{DateTime, Local, Utc};
#[cfg(windows)]
use clipboard_win::{Clipboard as WinClipboard, Getter, Setter, formats};
use eframe::egui::{self, ColorImage, TextureHandle};
use mouse_position::mouse_position::Mouse;
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
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
const DIALOG_SIZE: [f32; 2] = [620.0, 220.0];

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

#[derive(Serialize, Deserialize)]
struct History {
    entries: Vec<Entry>,
    #[serde(default = "default_open_with")]
    open_with: String,
}

impl Default for History {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            open_with: default_open_with(),
        }
    }
}

fn default_open_with() -> String {
    "code".to_owned()
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
            let target = root.join("history.json");
            if fs::write(&tmp, data).is_ok() {
                #[cfg(windows)]
                let _ = fs::remove_file(&target);
                let _ = fs::rename(tmp, target);
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
fn restore(content: &Content) -> Result<Option<ImageClipboard>, String> {
    match content {
        Content::Text(text) => {
            let mut clipboard = ImageClipboard::new().map_err(|e| e.to_string())?;
            clipboard.set_text(text).map_err(|e| e.to_string())?;
            Ok(Some(clipboard))
        }
        Content::Files(files) => {
            restore_files(files)?;
            Ok(None)
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
            Ok(Some(clipboard))
        }
    }
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
fn path_candidate(text: &str) -> Option<PathBuf> {
    let candidate = text.trim().trim_matches('"');
    if candidate.is_empty() || candidate.contains('\n') || candidate.contains('\r') {
        return None;
    }
    let path = PathBuf::from(candidate);
    if path.is_absolute() { Some(path) } else { None }
}
fn as_path(text: &str) -> Option<PathBuf> {
    path_candidate(text).filter(|path| path.exists())
}
fn existing_parent(path: &Path) -> Option<&Path> {
    path.parent().filter(|parent| parent.exists())
}
fn is_excel(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "xls" | "xlsx" | "xlsm" | "xlsb"
            )
        })
}
#[cfg(windows)]
fn unc_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    if value.starts_with(r"\\") {
        return value.into_owned();
    }
    let bytes = value.as_bytes();
    if bytes.len() >= 3 && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/') {
        if let Ok(host) = std::env::var("COMPUTERNAME") {
            return format!(r"\\{}\{}$\{}", host, bytes[0] as char, &value[3..]);
        }
    }
    value.into_owned()
}
#[cfg(not(windows))]
fn unc_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
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
fn open_with(app: &str, path: &Path) -> Result<(), String> {
    if app.trim().is_empty() {
        return Err("指定アプリが空です".to_owned());
    }
    Command::new(app.trim())
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}
#[cfg(windows)]
fn open_excel_read_only(path: &Path, _fallback: &str) -> Result<(), String> {
    let action = wide(OsStr::new("open"));
    let excel = wide(OsStr::new("excel.exe"));
    let parameters = wide(OsStr::new(&format!(r#"/r "{}""#, path.display())));
    let result = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            action.as_ptr(),
            excel.as_ptr(),
            parameters.as_ptr(),
            ptr::null(),
            1,
        )
    };
    if result > 32 {
        Ok(())
    } else {
        Err(format!("Excelを起動できませんでした: {result}"))
    }
}
#[cfg(not(windows))]
fn open_excel_read_only(path: &Path, fallback: &str) -> Result<(), String> {
    open_with(fallback, path)
}
fn copy_unc(path: &Path) -> Result<ImageClipboard, String> {
    let mut clipboard = ImageClipboard::new().map_err(|e| e.to_string())?;
    clipboard
        .set_text(unc_path(path))
        .map_err(|e| e.to_string())?;
    Ok(clipboard)
}
fn window_position(size: [f32; 2]) -> Option<egui::Pos2> {
    let Mouse::Position { x, y } = Mouse::get_mouse_position() else {
        return None;
    };
    let display = display_info::DisplayInfo::from_point(x, y).ok()?;
    Some(egui::pos2(
        display.x as f32 + (display.width as f32 - size[0]) / 2.0,
        display.y as f32 + (display.height as f32 - size[1]) / 2.0,
    ))
}
#[cfg(target_os = "linux")]
fn move_native_window(position: egui::Pos2, raise: bool) -> Result<(), String> {
    use x11rb::{
        CURRENT_TIME,
        connection::Connection,
        protocol::xproto::{
            Atom, AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt, EventMask,
            InputFocus, StackMode, Window,
        },
        rust_connection::RustConnection,
    };

    fn find_window(
        connection: &RustConnection,
        window: Window,
        pid_atom: Atom,
        pid: u32,
    ) -> Option<Window> {
        for child in connection.query_tree(window).ok()?.reply().ok()?.children {
            let property = connection
                .get_property(false, child, pid_atom, AtomEnum::CARDINAL, 0, 1)
                .ok()?
                .reply()
                .ok()?;
            if property.value32().and_then(|mut values| values.next()) == Some(pid) {
                return Some(child);
            }
            if let Some(found) = find_window(connection, child, pid_atom, pid) {
                return Some(found);
            }
        }
        None
    }

    let (connection, screen) = x11rb::connect(None).map_err(|e| e.to_string())?;
    let root = connection.setup().roots[screen].root;
    let pid_atom = connection
        .intern_atom(false, b"_NET_WM_PID")
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?
        .atom;
    let window = find_window(&connection, root, pid_atom, std::process::id())
        .ok_or_else(|| "ClipDeck window not found".to_owned())?;
    let mut configuration = ConfigureWindowAux::new()
        .x(position.x.round() as i32)
        .y(position.y.round() as i32);
    if raise {
        configuration = configuration.stack_mode(StackMode::ABOVE);
    }
    connection
        .configure_window(window, &configuration)
        .map_err(|e| e.to_string())?;
    if raise {
        let active_window = connection
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?
            .atom;
        let event = ClientMessageEvent::new(32, window, active_window, [1, CURRENT_TIME, 0, 0, 0]);
        connection
            .send_event(
                false,
                root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            )
            .map_err(|e| e.to_string())?;
        connection
            .set_input_focus(InputFocus::PARENT, window, CURRENT_TIME)
            .map_err(|e| e.to_string())?;
    }
    connection.flush().map_err(|e| e.to_string())
}
#[cfg(not(target_os = "linux"))]
fn move_native_window(_position: egui::Pos2, _raise: bool) -> Result<(), String> {
    Ok(())
}

fn configure_fonts(ctx: &egui::Context) {
    #[cfg(windows)]
    let candidates = [
        r"C:\Windows\Fonts\YuGothR.ttc",
        r"C:\Windows\Fonts\meiryo.ttc",
    ];
    #[cfg(not(windows))]
    let candidates = [
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJKjp-Regular.otf",
    ];
    let Some(bytes) = candidates.iter().find_map(|path| fs::read(path).ok()) else {
        return;
    };
    let name = "system-japanese".to_owned();
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert(name.clone(), Arc::new(egui::FontData::from_owned(bytes)));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push(name.clone());
    }
    ctx.set_fonts(fonts);
}
fn tray_icon() -> Result<tray::TrayIcon, String> {
    let mut rgba = vec![0; 32 * 32 * 4];
    for y in 0..32 {
        for x in 0..32 {
            let i = (y * 32 + x) * 4;
            let inside = (4..28).contains(&x) && (4..28).contains(&y);
            rgba[i..i + 4].copy_from_slice(if inside {
                &[77, 163, 255, 255]
            } else {
                &[0, 0, 0, 0]
            });
        }
    }
    let icon = tray::Icon::from_rgba(rgba, 32, 32).map_err(|e| e.to_string())?;
    tray::TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("ClipDeck")
        .build()
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
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
                ctx.request_repaint();
            }
        }
    });
}
#[cfg(not(windows))]
fn watch(tx: mpsc::Sender<Content>, ctx: egui::Context) {
    thread::spawn(move || {
        let mut last = 0;
        let mut initialized = false;
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
                if !initialized {
                    last = fingerprint;
                    initialized = true;
                    continue;
                }
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
                if !initialized {
                    last = fingerprint;
                    initialized = true;
                    continue;
                }
                if fingerprint == last || text.trim().is_empty() || text.len() > MAX_TEXT {
                    continue;
                }
                last = fingerprint;
                Content::Text(text)
            } else {
                initialized = true;
                continue;
            };
            if tx.send(item).is_err() {
                break;
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
            ctx.request_repaint();
        }
    });
}

struct PathDialog {
    input: String,
    focus_requested: bool,
}

struct App {
    history: History,
    rx: Receiver<Content>,
    query: String,
    filter: usize,
    textures: HashMap<String, TextureHandle>,
    status: String,
    monitoring: bool,
    path_dialog: Option<PathDialog>,
    tray_rx: Receiver<()>,
    show_history: bool,
    raise_frames: u8,
    owned_clipboard: Option<ImageClipboard>,
    exiting: bool,
    _tray: Option<tray::TrayIcon>,
}
impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        configure_fonts(&cc.egui_ctx);
        let (tx, rx) = mpsc::channel();
        watch(tx, cc.egui_ctx.clone());
        let (tray_tx, tray_rx) = mpsc::channel();
        let tray_ctx = cc.egui_ctx.clone();
        thread::spawn(move || {
            while let Ok(event) = tray::TrayIconEvent::receiver().recv() {
                let clicked = matches!(
                    event,
                    tray::TrayIconEvent::Click {
                        button_state: tray::MouseButtonState::Up,
                        ..
                    } | tray::TrayIconEvent::DoubleClick { .. }
                );
                if !clicked {
                    continue;
                }
                if tray_tx.send(()).is_err() {
                    break;
                }
                tray_ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
                tray_ctx.request_repaint();
            }
        });
        let tray = tray_icon();
        if let Err(error) = &tray {
            eprintln!("task tray unavailable: {error}");
        }
        let status = tray.as_ref().err().cloned().unwrap_or_default();
        Self {
            history: load(),
            rx,
            query: String::new(),
            filter: 0,
            textures: HashMap::new(),
            status,
            monitoring: true,
            path_dialog: None,
            tray_rx,
            show_history: false,
            raise_frames: 0,
            owned_clipboard: None,
            exiting: false,
            _tray: tray.ok(),
        }
    }
    fn ingest(&mut self, content: Content, ctx: &egui::Context) {
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
        if let Content::Text(text) = &content {
            if let Some(path) = path_candidate(text) {
                self.path_dialog = Some(PathDialog {
                    input: path.to_string_lossy().into_owned(),
                    focus_requested: true,
                });
                self.show_history = false;
                self.raise_frames = 2;
                if let Some(position) = window_position(DIALOG_SIZE) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(position));
                    let _ = move_native_window(position, false);
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
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
    fn path_dialog_ui(&mut self, ui: &mut egui::Ui) {
        ui.painter()
            .rect_filled(ui.max_rect(), 0.0, egui::Color32::from_rgb(27, 27, 27));
        let ctrl_enter = ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Enter));
        let ctrl_backspace = ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Backspace));
        let enter = ui.input(|i| !i.modifiers.ctrl && i.key_pressed(egui::Key::Enter));
        let backspace_pressed =
            ui.input(|i| !i.modifiers.ctrl && i.key_pressed(egui::Key::Backspace));
        let copy = ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::C));
        let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
        if ctrl_backspace {
            ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Backspace));
        } else if backspace_pressed {
            ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace));
        }

        let dialog = self.path_dialog.as_mut().unwrap();
        ui.heading("パスを開く");
        let response = ui.add(
            egui::TextEdit::singleline(&mut dialog.input)
                .desired_width(f32::INFINITY)
                .id_salt("path-input"),
        );
        if dialog.focus_requested {
            response.request_focus();
            dialog.focus_requested = false;
        }

        let path = PathBuf::from(dialog.input.trim().trim_matches('"'));
        let path_exists = path.exists();
        let parent = existing_parent(&path).map(Path::to_path_buf);
        ui.label(if path_exists {
            "パス: 存在します"
        } else {
            "パス: 存在しません"
        });
        ui.label(if parent.is_some() {
            "親フォルダ: 存在します"
        } else {
            "親フォルダ: 存在しません"
        });

        let mut result = None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(path_exists, egui::Button::new("開く (Enter)"))
                .clicked()
                || (enter && path_exists)
            {
                result = Some(shell_open(&path.to_string_lossy()));
            }
            if ui
                .add_enabled(parent.is_some(), egui::Button::new("親を開く (Backspace)"))
                .clicked()
                || (backspace_pressed && parent.is_some())
            {
                result = Some(explorer_folder(parent.as_ref().unwrap()));
            }
            if ui
                .add_enabled(path_exists, egui::Button::new("指定アプリ (Ctrl+Enter)"))
                .clicked()
                || (ctrl_enter && path_exists)
            {
                result = Some(if is_excel(&path) {
                    open_excel_read_only(&path, &self.history.open_with)
                } else {
                    open_with(&self.history.open_with, &path)
                });
            }
            if ctrl_backspace && parent.is_some() {
                result = Some(open_with(&self.history.open_with, parent.as_ref().unwrap()));
            }
        });
        ui.label("Ctrl+Backspace: 親を指定アプリで開く / Ctrl+C: UNCパスをコピー / Esc: 閉じる");

        if copy {
            match copy_unc(&path) {
                Ok(clipboard) => {
                    self.owned_clipboard = Some(clipboard);
                    self.action(Ok(()));
                }
                Err(error) => self.action(Err(error)),
            }
        }
        if let Some(result) = result {
            let succeeded = result.is_ok();
            self.action(result);
            if succeeded {
                self.hide(ui.ctx());
            }
        }
        if escape {
            self.hide(ui.ctx());
        }
        if !self.status.is_empty() {
            ui.label(&self.status);
        }
    }
    fn action(&mut self, result: Result<(), String>) {
        self.status = result
            .map(|_| "操作しました".to_owned())
            .unwrap_or_else(|e| format!("エラー: {e}"));
    }
    fn hide(&mut self, ctx: &egui::Context) {
        self.path_dialog = None;
        self.show_history = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }
    fn process_events(&mut self, ctx: &egui::Context) {
        if self.tray_rx.try_recv().is_ok() {
            self.path_dialog = None;
            self.show_history = true;
            self.raise_frames = 2;
            if let Some(position) = window_position(DIALOG_SIZE) {
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(position));
                let _ = move_native_window(position, false);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        while let Ok(item) = self.rx.try_recv() {
            self.ingest(item, ctx);
        }
    }
}
impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_events(ctx);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::from_rgb(0.012, 0.012, 0.012).to_array()
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ctx.request_repaint_after(Duration::from_millis(200));
        if self.raise_frames > 0 {
            self.raise_frames -= 1;
            if self.raise_frames == 0 {
                if let Some(position) = window_position(DIALOG_SIZE) {
                    let _ = move_native_window(position, true);
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
        }
        if ui.input(|i| i.viewport().close_requested()) && !self.exiting {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide(&ctx);
        }
        self.process_events(&ctx);
        if self.exiting {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if self.path_dialog.is_some() {
            egui::CentralPanel::default().show(ui, |ui| self.path_dialog_ui(ui));
            return;
        }
        if !self.show_history {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            return;
        }
        egui::CentralPanel::default().show(ui, |ui| {
            ui.painter()
                .rect_filled(ui.max_rect(), 0.0, egui::Color32::from_rgb(27, 27, 27));
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
                if ui.button("終了").clicked() {
                    self.exiting = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label("指定アプリ:");
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut self.history.open_with)
                            .desired_width(240.0),
                    )
                    .changed()
                {
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
                                match restore(&entry.content) {
                                    Ok(clipboard) => {
                                        if let Some(clipboard) = clipboard {
                                            self.owned_clipboard = Some(clipboard);
                                        }
                                        self.action(Ok(()));
                                    }
                                    Err(error) => self.action(Err(error)),
                                }
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
    let viewport = egui::ViewportBuilder::default()
        .with_title("ClipDeck")
        .with_inner_size(DIALOG_SIZE)
        .with_visible(false)
        .with_decorations(false)
        .with_taskbar(false);
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "ClipDeck",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_absolute_paths_without_requiring_them_to_exist() {
        let candidate = if cfg!(windows) {
            r"C:\not-created\file.txt"
        } else {
            "/not-created/file.txt"
        };
        assert_eq!(path_candidate(candidate), Some(PathBuf::from(candidate)));
        assert_eq!(path_candidate("relative/file.txt"), None);
        assert_eq!(path_candidate("/one\n/two"), None);
    }

    #[test]
    fn detects_excel_extensions_case_insensitively() {
        assert!(is_excel(Path::new("report.XLSX")));
        assert!(is_excel(Path::new("report.xlsm")));
        assert!(!is_excel(Path::new("report.csv")));
    }

    #[test]
    fn old_history_files_receive_the_default_app() {
        let history: History = serde_json::from_str(r#"{"entries":[]}"#).unwrap();
        assert_eq!(history.open_with, "code");
    }
}

// Terra Studio: a local desktop toolkit for YouTube creators.
// Serves the UI on 127.0.0.1, shows it in its own WebView2 window (browser fallback), proxies
// generation requests to the Anthropic Messages API with the user's own key,
// and imports YouTube Studio CSV/ZIP exports. Everything is stored per user
// in %APPDATA%\TerraStudio.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tiny_http::{Header, Method, Request, Response, Server};

const PAGE: &str = concat!(
    include_str!("../assets/head.part"),
    include_str!("../assets/style.part"),
    include_str!("../assets/mid.part"),
    include_str!("../assets/body.part"),
    include_str!("../assets/tail.part")
);
const PORTS: [u16; 4] = [47815, 47816, 47817, 47818];
const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2026-10-21";
const MODELS: [&str; 3] = ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"];
/// Seconds without a heartbeat from the window before the app exits.
const IDLE_EXIT_SECS: u64 = 180;
const MAX_UPLOAD: u64 = 60 * 1024 * 1024;

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct Config {
    /// Plaintext key, in memory only. On Windows it is written to disk
    /// encrypted (`api_key_dpapi`); older plaintext files are migrated on load.
    #[serde(skip_serializing_if = "String::is_empty")]
    api_key: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    api_key_dpapi: String,
    /// "claude", "gemini", or "" to pick automatically from the saved keys.
    provider: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    gemini_key: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    gemini_key_dpapi: String,
    /// Gemini model id such as "gemini-x-flash"; empty picks the newest Flash.
    gemini_model: String,
    /// Separate Gemini key for images: image models need a billed project,
    /// while the text key above can stay on the free tier.
    #[serde(skip_serializing_if = "String::is_empty")]
    image_key: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    image_key_dpapi: String,
    image_model: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    eleven_key: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    eleven_key_dpapi: String,
    eleven_voice: String,
    eleven_voice_name: String,
    eleven_model: String,
    /// YouTube (Google OAuth "Desktop app" client the user creates in Google Cloud).
    yt_client_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    yt_secret: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    yt_secret_dpapi: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    yt_refresh: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    yt_refresh_dpapi: String,
    yt_channel: String,
    yt_synced: u64,
    model: String,
    effort: String,
    profile: Value,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            api_key: String::new(),
            api_key_dpapi: String::new(),
            provider: String::new(),
            gemini_key: String::new(),
            gemini_key_dpapi: String::new(),
            gemini_model: String::new(),
            image_key: String::new(),
            image_key_dpapi: String::new(),
            image_model: String::new(),
            eleven_key: String::new(),
            eleven_key_dpapi: String::new(),
            eleven_voice: String::new(),
            eleven_voice_name: String::new(),
            eleven_model: String::new(),
            yt_client_id: String::new(),
            yt_secret: String::new(),
            yt_secret_dpapi: String::new(),
            yt_refresh: String::new(),
            yt_refresh_dpapi: String::new(),
            yt_channel: String::new(),
            yt_synced: 0,
            model: MODELS[1].into(),
            effort: "medium".into(),
            profile: json!({}),
        }
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn app_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("TerraStudio");
    let _ = fs::create_dir_all(&dir);
    dir
}

fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)
}

fn load_config(dir: &Path) -> Config {
    let mut c: Config = fs::read_to_string(dir.join("config.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    // A key still in plaintext comes from an older version or a failed encryption.
    let had_plain = !c.api_key.is_empty() || !c.gemini_key.is_empty() || !c.image_key.is_empty() || !c.eleven_key.is_empty()
        || !c.yt_secret.is_empty() || !c.yt_refresh.is_empty();
    let open = |enc: &str| from_hex(enc).and_then(|b| secret::unprotect(&b)).unwrap_or_default();
    if !c.api_key_dpapi.is_empty() {
        c.api_key = open(&c.api_key_dpapi);
    }
    if !c.gemini_key_dpapi.is_empty() {
        c.gemini_key = open(&c.gemini_key_dpapi);
    }
    if !c.image_key_dpapi.is_empty() {
        c.image_key = open(&c.image_key_dpapi);
    }
    if !c.eleven_key_dpapi.is_empty() {
        c.eleven_key = open(&c.eleven_key_dpapi);
    }
    if !c.yt_secret_dpapi.is_empty() {
        c.yt_secret = open(&c.yt_secret_dpapi);
    }
    if !c.yt_refresh_dpapi.is_empty() {
        c.yt_refresh = open(&c.yt_refresh_dpapi);
    }
    if had_plain && secret::AVAILABLE {
        let _ = save_config(dir, &c);
    }
    c
}

/// Writes config.json with the API key encrypted for the current Windows user.
fn save_config(dir: &Path, c: &Config) -> Result<(), String> {
    let mut disk = c.clone();
    disk.api_key_dpapi.clear();
    disk.gemini_key_dpapi.clear();
    disk.image_key_dpapi.clear();
    disk.eleven_key_dpapi.clear();
    disk.yt_secret_dpapi.clear();
    disk.yt_refresh_dpapi.clear();
    for (plain, enc) in [(&c.yt_secret, &mut disk.yt_secret_dpapi), (&c.yt_refresh, &mut disk.yt_refresh_dpapi)] {
        if !plain.is_empty() {
            if let Some(e) = secret::protect(plain.as_bytes()) {
                *enc = to_hex(&e);
            }
        }
    }
    if !disk.yt_secret_dpapi.is_empty() {
        disk.yt_secret.clear();
    }
    if !disk.yt_refresh_dpapi.is_empty() {
        disk.yt_refresh.clear();
    }
    if !c.eleven_key.is_empty() {
        if let Some(enc) = secret::protect(c.eleven_key.as_bytes()) {
            disk.eleven_key_dpapi = to_hex(&enc);
            disk.eleven_key.clear();
        }
    }
    if !c.api_key.is_empty() {
        if let Some(enc) = secret::protect(c.api_key.as_bytes()) {
            disk.api_key_dpapi = to_hex(&enc);
            disk.api_key.clear();
        }
    }
    if !c.gemini_key.is_empty() {
        if let Some(enc) = secret::protect(c.gemini_key.as_bytes()) {
            disk.gemini_key_dpapi = to_hex(&enc);
            disk.gemini_key.clear();
        }
    }
    if !c.image_key.is_empty() {
        if let Some(enc) = secret::protect(c.image_key.as_bytes()) {
            disk.image_key_dpapi = to_hex(&enc);
            disk.image_key.clear();
        }
    }
    let bytes = serde_json::to_vec_pretty(&disk).map_err(|e| e.to_string())?;
    write_atomic(&dir.join("config.json"), &bytes).map_err(|e| e.to_string())
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// Windows DPAPI: data encrypted this way can only be decrypted by the same
/// Windows user account, so a copied config.json does not leak the key.
#[cfg(windows)]
mod secret {
    use std::ffi::c_void;
    pub const AVAILABLE: bool = true;
    #[repr(C)]
    struct Blob {
        len: u32,
        data: *mut u8,
    }
    #[link(name = "crypt32")]
    extern "system" {
        fn CryptProtectData(input: *const Blob, descr: *const u16, entropy: *const Blob, reserved: *mut c_void,
            prompt: *mut c_void, flags: u32, output: *mut Blob) -> i32;
        fn CryptUnprotectData(input: *const Blob, descr: *mut *mut u16, entropy: *const Blob, reserved: *mut c_void,
            prompt: *mut c_void, flags: u32, output: *mut Blob) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(mem: *mut c_void) -> *mut c_void;
    }
    const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;
    fn run(input: &[u8], encrypt: bool) -> Option<Vec<u8>> {
        let inp = Blob { len: input.len() as u32, data: input.as_ptr() as *mut u8 };
        let mut out = Blob { len: 0, data: std::ptr::null_mut() };
        let ok = unsafe {
            if encrypt {
                CryptProtectData(&inp, std::ptr::null(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
            } else {
                CryptUnprotectData(&inp, std::ptr::null_mut(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
            }
        };
        if ok == 0 || out.data.is_null() {
            return None;
        }
        let v = unsafe { std::slice::from_raw_parts(out.data, out.len as usize).to_vec() };
        unsafe { LocalFree(out.data as *mut c_void) };
        Some(v)
    }
    pub fn protect(b: &[u8]) -> Option<Vec<u8>> {
        run(b, true)
    }
    pub fn unprotect(b: &[u8]) -> Option<String> {
        run(b, false).and_then(|v| String::from_utf8(v).ok())
    }
}

/// Development builds on other systems keep the key in plaintext.
#[cfg(not(windows))]
mod secret {
    pub const AVAILABLE: bool = false;
    pub fn protect(_: &[u8]) -> Option<Vec<u8>> {
        None
    }
    pub fn unprotect(_: &[u8]) -> Option<String> {
        None
    }
}

/// Serialises read-modify-write of last.json.
static LAST_JSON_LOCK: Mutex<()> = Mutex::new(());
/// Serialises read-modify-write of data.json (import and delete).
static DATA_JSON_LOCK: Mutex<()> = Mutex::new(());
/// Largest size one CSV inside a ZIP may expand to.
const MAX_CSV: u64 = 100 * 1024 * 1024;

fn load_data(dir: &Path) -> Value {
    load_json(&dir.join("data.json"))
}

fn load_json(path: &Path) -> Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(|v: &Value| v.is_object())
        .unwrap_or_else(|| json!({}))
}

fn main() {
    // Second launch: the same version is already running, so just open another window.
    // An older version (after an update) is asked to quit so the new exe takes over.
    for &p in &PORTS {
        match ping(p) {
            Some(v) if v == PING_ID => {
                // Bring the running app's own window to the front; fall back to a browser window.
                let shown = ureq::AgentBuilder::new()
                    .timeout(Duration::from_millis(1500))
                    .try_proxy_from_env(false)
                    .build()
                    .post(&format!("http://127.0.0.1:{p}/api/show"))
                    .set("X-Terra", "1")
                    .call()
                    .ok()
                    .and_then(|r| r.into_json::<Value>().ok())
                    .is_some_and(|j| j["shown"] == true);
                if !shown {
                    open_window(p);
                }
                return;
            }
            Some(_) => {
                let _ = ureq::AgentBuilder::new()
                    .timeout(Duration::from_millis(1500))
                    .try_proxy_from_env(false)
                    .build()
                    .post(&format!("http://127.0.0.1:{p}/api/quit"))
                    .set("X-Terra", "1")
                    .call();
                for _ in 0..20 {
                    std::thread::sleep(Duration::from_millis(150));
                    if ping(p).is_none() {
                        break;
                    }
                }
            }
            None => {}
        }
    }
    let Some((server, port)) = PORTS
        .iter()
        .find_map(|&p| Server::http(("127.0.0.1", p)).ok().map(|s| (s, p)))
    else {
        show_error("Terra Studio tidak bisa dimulai: port 47815 sampai 47818 sedang dipakai program lain.");
        return;
    };

    let dir = app_dir();
    let mut first = load_config(&dir);
    first_run_setup(&dir, &mut first);
    let cfg = Arc::new(Mutex::new(first));
    // Give the window a minute of grace to load before the idle timer counts.
    let last_seen = Arc::new(AtomicU64::new(now() + 60));
    {
        let last_seen = last_seen.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(10));
            // With the app's own window, closing it ends the process, so no idle timer is needed.
            if !NATIVE.load(Ordering::Relaxed)
                && now().saturating_sub(last_seen.load(Ordering::Relaxed)) > IDLE_EXIT_SECS
            {
                std::process::exit(0);
            }
        });
    }

    let web_dir = dir.join("webview");
    let serve = move || {
        for req in server.incoming_requests() {
            let cfg = cfg.clone();
            let dir = dir.clone();
            let last_seen = last_seen.clone();
            std::thread::spawn(move || handle(req, port, &cfg, &dir, &last_seen));
        }
    };

    #[cfg(windows)]
    {
        std::thread::spawn(serve);
        // Runs until the window closes. Returns only if WebView2 is unavailable.
        native::run(port, web_dir);
        open_window(port);
        loop {
            std::thread::park();
        }
    }
    #[cfg(not(windows))]
    {
        let _ = web_dir;
        open_window(port);
        serve();
    }
}

/// True while the app shows its own WebView2 window (Windows).
static NATIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The app's own window: WebView2 (built into Windows 10/11) inside a native frame,
/// so no Edge or Chrome process opens. Falls back to the browser window if WebView2 is missing.
#[cfg(windows)]
mod native {
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;
    use std::sync::{Mutex, OnceLock};
    use tao::dpi::{LogicalSize, PhysicalSize};
    use tao::event::{Event, WindowEvent};
    use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
    use tao::platform::windows::IconExtWindows;
    use tao::window::{Icon, WindowBuilder};
    use wry::{WebContext, WebViewBuilder};

    static PROXY: OnceLock<Mutex<EventLoopProxy<()>>> = OnceLock::new();
    static HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(hwnd: isize, attr: u32, value: *const std::ffi::c_void, size: u32) -> i32;
    }

    /// Paints the Windows title bar in the app's own background colour so the
    /// window reads as one surface. Windows 11 honours the colours; Windows 10
    /// only switches the bar to dark. Failures are harmless and ignored.
    pub fn tint_title(skin: &str) {
        let hwnd = HWND.load(Ordering::Relaxed);
        if hwnd == 0 {
            return;
        }
        let (bg, ink) = if skin == "lumut" { ((0x11, 0x22, 0x17), (0xE8, 0xF2, 0xE6)) } else { ((0x0F, 0x0E, 0x0C), (0xF4, 0xEC, 0xDD)) };
        let colorref = |(r, g, b): (u32, u32, u32)| r | (g << 8) | (b << 16);
        let set = |attr: u32, v: u32| unsafe {
            DwmSetWindowAttribute(hwnd, attr, &v as *const u32 as *const std::ffi::c_void, 4);
        };
        set(20, 1); // DWMWA_USE_IMMERSIVE_DARK_MODE
        set(35, colorref(bg)); // DWMWA_CAPTION_COLOR
        set(34, colorref(bg)); // DWMWA_BORDER_COLOR
        set(36, colorref(ink)); // DWMWA_TEXT_COLOR
    }

    /// Asks the window to come to the front. False when there is no native window.
    pub fn show() -> bool {
        PROXY.get().and_then(|p| p.lock().ok().map(|p| p.send_event(()).is_ok())).unwrap_or(false)
    }

    fn local(url: &str, port: u16) -> bool {
        url.starts_with(&format!("http://127.0.0.1:{port}/")) || url == "about:blank"
    }

    pub fn run(port: u16, data_dir: PathBuf) {
        let event_loop = EventLoopBuilder::<()>::with_user_event().build();
        let Ok(window) = WindowBuilder::new()
            .with_title("Terra Studio")
            .with_inner_size(LogicalSize::new(1440.0, 920.0))
            .with_min_inner_size(LogicalSize::new(1024.0, 680.0))
            .with_window_icon(Icon::from_resource(1, Some(PhysicalSize::new(64, 64))).ok())
            .build(&event_loop)
        else {
            return;
        };
        {
            use tao::platform::windows::WindowExtWindows;
            HWND.store(window.hwnd() as isize, Ordering::Relaxed);
            tint_title("batu");
        }
        let _ = std::fs::create_dir_all(&data_dir);
        let mut ctx = WebContext::new(Some(data_dir));
        let url = format!("http://127.0.0.1:{port}/");
        let webview = WebViewBuilder::with_web_context(&mut ctx)
            .with_url(&url)
            .with_background_color((15, 14, 12, 255))
            .with_navigation_handler(move |u| {
                if local(&u, port) {
                    return true;
                }
                super::open_browser(&u);
                false
            })
            .with_new_window_req_handler(move |u| {
                if !local(&u, port) {
                    super::open_browser(&u);
                }
                false
            })
            .build(&window);
        let Ok(webview) = webview else {
            drop(window);
            return;
        };
        super::NATIVE.store(true, Ordering::Relaxed);
        let _ = PROXY.set(Mutex::new(event_loop.create_proxy()));
        event_loop.run(move |event, _, flow| {
            *flow = ControlFlow::Wait;
            let _ = (&webview, &ctx);
            match event {
                Event::UserEvent(()) => {
                    window.set_minimized(false);
                    window.set_visible(true);
                    window.set_focus();
                }
                Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                    std::process::exit(0);
                }
                _ => {}
            }
        });
    }
}

/// Opens a URL in the user's normal browser (Google sign-in should not run inside the app window).
fn open_browser(url: &str) {
    #[cfg(windows)]
    {
        let _ = Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("xdg-open").arg(url).spawn();
    }
}

const PING_ID: &str = concat!("terra-studio ", env!("CARGO_PKG_VERSION"));

/// Some(id) when a Terra Studio (any version) answers on this port.
fn ping(port: u16) -> Option<String> {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(800))
        .try_proxy_from_env(false)
        .build()
        .get(&format!("http://127.0.0.1:{port}/api/ping"))
        .call()
        .ok()
        .and_then(|r| r.into_string().ok())
        .filter(|s| s.starts_with("terra-studio"))
}

fn open_window(port: u16) {
    let url = format!("http://127.0.0.1:{port}/");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // Prefer an app-style window (no tabs or address bar) in Edge or Chrome.
        let mut candidates: Vec<PathBuf> = Vec::new();
        for var in ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(var) {
                let base = PathBuf::from(base);
                candidates.push(base.join(r"Microsoft\Edge\Application\msedge.exe"));
                candidates.push(base.join(r"Google\Chrome\Application\chrome.exe"));
            }
        }
        for exe in candidates.iter().filter(|p| p.exists()) {
            if Command::new(exe)
                .arg(format!("--app={url}"))
                .arg("--window-size=1280,860")
                .spawn()
                .is_ok()
            {
                return;
            }
        }
        let _ = Command::new("cmd")
            .args(["/C", "start", "", &url])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
    #[cfg(not(windows))]
    {
        eprintln!("Terra Studio running at {url}");
        let _ = Command::new("xdg-open").arg(&url).spawn();
    }
}

#[cfg(windows)]
fn show_error(msg: &str) {
    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(hwnd: *mut core::ffi::c_void, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (t, c) = (wide(msg), wide("Terra Studio"));
    unsafe {
        MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), 0x10);
    }
}

#[cfg(not(windows))]
fn show_error(msg: &str) {
    eprintln!("{msg}");
}

fn respond(req: Request, code: u16, ctype: &str, body: Vec<u8>) {
    let resp = Response::from_data(body)
        .with_status_code(code)
        .with_header(Header::from_bytes("Content-Type", ctype).unwrap())
        .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap())
        .with_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
    let _ = req.respond(resp);
}

fn respond_json(req: Request, code: u16, v: Value) {
    respond(req, code, "application/json; charset=utf-8", v.to_string().into_bytes());
}

fn header(req: &Request, name: &str) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

fn read_body(req: &mut Request, limit: u64) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    req.as_reader()
        .take(limit + 1)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    if buf.len() as u64 > limit {
        return Err("File terlalu besar.".into());
    }
    Ok(buf)
}

/// Reads a JSON request body; malformed JSON becomes an empty object.
fn read_json(req: &mut Request, limit: u64) -> Result<Value, String> {
    let body = read_body(req, limit)?;
    Ok(serde_json::from_slice(&body).unwrap_or_else(|_| json!({})))
}

fn handle(mut req: Request, port: u16, cfg: &Mutex<Config>, dir: &Path, last_seen: &AtomicU64) {
    let path = req.url().split('?').next().unwrap_or("/").to_string();
    let method = req.method().clone();

    // Only answer requests addressed to this app (blocks DNS-rebinding tricks).
    let host = header(&req, "Host").unwrap_or_default();
    if host != format!("127.0.0.1:{port}") && host != format!("localhost:{port}") {
        return respond_json(req, 403, json!({"error": "forbidden"}));
    }
    // State-changing calls must carry a custom header, which other websites
    // cannot send to this server without a CORS preflight we never approve.
    let mutating = !matches!(method, Method::Get | Method::Head);
    if mutating && path.starts_with("/api/") && header(&req, "X-Terra").as_deref() != Some("1") {
        return respond_json(req, 403, json!({"error": "forbidden"}));
    }

    match (method, path.as_str()) {
        (Method::Get, "/") | (Method::Get, "/index.html") => {
            respond(req, 200, "text/html; charset=utf-8", PAGE.as_bytes().to_vec())
        }
        // Fonts ship inside the exe so the app looks the same offline.
        (Method::Get, "/fonts/archivo.woff2") => {
            respond(req, 200, "font/woff2", include_bytes!("../assets/archivo.woff2").to_vec())
        }
        (Method::Get, "/fonts/familjen.woff2") => {
            respond(req, 200, "font/woff2", include_bytes!("../assets/familjen.woff2").to_vec())
        }
        (Method::Get, "/logo.png") => {
            respond(req, 200, "image/png", include_bytes!("../assets/logo.png").to_vec())
        }
        (Method::Get, "/api/ping") => respond(req, 200, "text/plain", PING_ID.as_bytes().to_vec()),
        (Method::Post, "/api/heartbeat") => {
            last_seen.store(now(), Ordering::Relaxed);
            respond_json(req, 200, json!({"ok": true}))
        }
        (Method::Get, "/api/config") => {
            let c = cfg.lock().unwrap().clone();
            let hint: String = c.api_key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
            respond_json(
                req,
                200,
                json!({
                    "has_key": !c.api_key.is_empty(),
                    "has_gemini": !c.gemini_key.is_empty(),
                    "gemini_hint": c.gemini_key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect::<String>(),
                    "gemini_model": c.gemini_model,
                    "has_image": !c.image_key.is_empty(),
                    "image_hint": c.image_key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect::<String>(),
                    "image_model": gemini::image_model(&c),
                    "thumb_dir": eleven::out_dir(dir).display().to_string(),
                    "provider": active_provider(&c),
                    "provider_choice": c.provider,
                    "has_eleven": !c.eleven_key.is_empty(),
                    "eleven_hint": c.eleven_key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect::<String>(),
                    "eleven_voice": c.eleven_voice,
                    "eleven_voice_name": c.eleven_voice_name,
                    "eleven_model": c.eleven_model,
                    "audio_dir": eleven::out_dir(dir).display().to_string(),
                    "yt_client_id": c.yt_client_id,
                    "has_yt_secret": !c.yt_secret.is_empty(),
                    "yt_connected": !c.yt_refresh.is_empty(),
                    "yt_channel": c.yt_channel,
                    "yt_synced": c.yt_synced,
                    "key_hint": hint,
                    "model": c.model,
                    "effort": c.effort,
                    "profile": c.profile,
                    "dir": dir.display().to_string(),
                }),
            )
        }
        (Method::Post, "/api/config") => {
            let v = match read_json(&mut req, 256 * 1024) {
                Ok(v) => v,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            let mut c = cfg.lock().unwrap();
            if v["clear_key"].as_bool() == Some(true) {
                c.api_key.clear();
            }
            if v["clear_yt"].as_bool() == Some(true) {
                c.yt_refresh.clear();
                c.yt_channel.clear();
            }
            if let Some(x) = v["yt_client_id"].as_str().map(str::trim).filter(|x| x.len() < 200 && x.ends_with(".apps.googleusercontent.com")) {
                c.yt_client_id = x.to_string();
            }
            if let Some(x) = v["yt_secret"].as_str().map(str::trim).filter(|x| !x.is_empty() && x.len() < 200) {
                c.yt_secret = x.to_string();
            }
            if v["clear_eleven"].as_bool() == Some(true) {
                c.eleven_key.clear();
            }
            if let Some(k) = v["eleven_key"].as_str().map(str::trim).filter(|k| !k.is_empty()) {
                c.eleven_key = k.to_string();
            }
            let safe_id = |x: &str| x.len() < 80 && x.chars().all(|ch| ch.is_ascii_alphanumeric() || "-_".contains(ch));
            if let Some(x) = v["eleven_voice"].as_str().filter(|x| safe_id(x)) {
                c.eleven_voice = x.to_string();
                c.eleven_voice_name = v["eleven_voice_name"].as_str().unwrap_or("").chars().take(80).collect();
            }
            if let Some(x) = v["eleven_model"].as_str().filter(|x| safe_id(x)) {
                c.eleven_model = x.to_string();
            }
            if v["clear_gemini"].as_bool() == Some(true) {
                c.gemini_key.clear();
            }
            if let Some(k) = v["gemini_key"].as_str().map(str::trim).filter(|k| !k.is_empty()) {
                c.gemini_key = k.to_string();
            }
            if let Some(m) = v["gemini_model"].as_str().filter(|m| m.is_empty() || (m.len() < 80 && m.chars().all(|ch| ch.is_ascii_alphanumeric() || "-._".contains(ch)))) {
                c.gemini_model = m.to_string();
            }
            if v["clear_image"].as_bool() == Some(true) {
                c.image_key.clear();
            }
            if let Some(k) = v["image_key"].as_str().map(str::trim).filter(|k| !k.is_empty() && k.len() < 200) {
                c.image_key = k.to_string();
            }
            if let Some(m) = v["image_model"].as_str().filter(|m| m.is_empty() || gemini::safe_model(m)) {
                c.image_model = m.to_string();
            }
            if let Some(p) = v["provider"].as_str().filter(|p| ["", "manual", "claude", "gemini"].contains(p)) {
                c.provider = p.to_string();
            }
            if let Some(k) = v["api_key"].as_str() {
                let k = k.trim();
                if !k.is_empty() {
                    c.api_key = k.to_string();
                }
            }
            if let Some(m) = v["model"].as_str() {
                if MODELS.contains(&m) {
                    c.model = m.to_string();
                }
            }
            if let Some(e) = v["effort"].as_str() {
                if ["low", "medium", "high"].contains(&e) {
                    c.effort = e.to_string();
                }
            }
            if v["profile"].is_object() {
                c.profile = v["profile"].clone();
            }
            let saved = save_config(dir, &c);
            drop(c);
            match saved {
                Ok(()) => respond_json(req, 200, json!({"ok": true})),
                Err(e) => respond_json(req, 500, json!({"error": format!("Gagal menyimpan pengaturan: {e}")})),
            }
        }
        (Method::Post, "/api/generate") => {
            let v = match read_json(&mut req, 512 * 1024) {
                Ok(v) => v,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            let c = cfg.lock().unwrap().clone();
            let (code, out) = generate(&c, v["prompt"].as_str().unwrap_or(""), v["test"].as_bool().unwrap_or(false));
            respond_json(req, code, out)
        }
        (Method::Get, "/api/data") => respond_json(req, 200, load_data(dir)),
        (Method::Post, "/api/gemini-models") => {
            let v = match read_json(&mut req, 64 * 1024) {
                Ok(v) => v,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            let key = v["key"].as_str().map(str::trim).filter(|k| !k.is_empty()).map(String::from)
                .unwrap_or_else(|| cfg.lock().unwrap().gemini_key.clone());
            if key.is_empty() {
                return respond_json(req, 400, json!({"error": "API key Gemini belum diisi."}));
            }
            match gemini::models(&key) {
                Ok(list) => {
                    let def = gemini::pick_default(&list);
                    respond_json(req, 200, json!({"models": list, "default": def}))
                }
                Err((code, body)) => respond_json(req, 502, json!({"error": gemini::friendly(code, &body)})),
            }
        }
        (Method::Post, "/api/image-models") => {
            let key = cfg.lock().unwrap().image_key.clone();
            if key.is_empty() {
                return respond_json(req, 400, json!({"error": "Kunci Gemini untuk gambar belum diisi."}));
            }
            match gemini::image_models(&key) {
                Ok(list) => respond_json(req, 200, json!({"models": list})),
                Err((code, body)) => respond_json(req, 502, json!({"error": gemini::friendly_image(code, &body)})),
            }
        }
        // One image per call, saved under gen/ so a restart does not cost a new generation.
        (Method::Post, "/api/image") => {
            let v = match read_json(&mut req, 64 * 1024) {
                Ok(v) => v,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            let c = cfg.lock().unwrap().clone();
            if c.image_key.is_empty() {
                return respond_json(req, 400, json!({"error": "Kunci Gemini untuk gambar belum dipasang. Buka Pengaturan, bagian Gambar thumbnail."}));
            }
            let prompt = v["prompt"].as_str().unwrap_or("").trim().to_string();
            if prompt.is_empty() || prompt.len() > 6000 {
                return respond_json(req, 400, json!({"error": "Prompt gambar kosong atau terlalu panjang."}));
            }
            let slot = v["slot"].as_u64().unwrap_or(0).min(9);
            let stem = match v["id"].as_str() {
                Some("test") => "test".to_string(),
                Some(id) => match thumb_stem(id) {
                    Some(s) => s,
                    None => return respond_json(req, 400, json!({"error": "id tidak valid"})),
                },
                None => "free".to_string(),
            };
            let model = gemini::image_model(&c);
            match gemini::image(&c.image_key, &model, &prompt) {
                Ok((bytes, ext)) => {
                    let gdir = dir.join("gen");
                    let _ = fs::create_dir_all(&gdir);
                    for old in ["png", "jpg", "webp"] {
                        let _ = fs::remove_file(gdir.join(format!("{stem}-{slot}.{old}")));
                    }
                    let name = format!("{stem}-{slot}.{ext}");
                    match fs::write(gdir.join(&name), &bytes) {
                        Ok(()) => respond_json(req, 200, json!({"file": name, "v": now(), "model": model})),
                        Err(e) => respond_json(req, 500, json!({"error": format!("Gagal menyimpan gambar: {e}")})),
                    }
                }
                Err(e) => respond_json(req, 502, json!({"error": e, "model": model})),
            }
        }
        (Method::Get, p) if p.starts_with("/api/gen/") => {
            let name = percent_decode(&p["/api/gen/".len()..]);
            let ct = match name.rsplit_once('.') {
                Some((stem, ext)) if !stem.is_empty() && stem.len() <= 48 && stem.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') => match ext {
                    "png" => Some("image/png"),
                    "jpg" => Some("image/jpeg"),
                    "webp" => Some("image/webp"),
                    _ => None,
                },
                _ => None,
            };
            match ct.and_then(|ct| fs::read(dir.join("gen").join(&name)).ok().map(|b| (b, ct))) {
                Some((bytes, ct)) => respond(req, 200, ct, bytes),
                None => respond_json(req, 404, json!({"error": "not found"})),
            }
        }
        // The finished 1280x720 thumbnail, composed in the window, goes to Downloads for upload in YouTube Studio.
        (Method::Post, "/api/save-jpg") => {
            let name = query_param(req.url(), "name").unwrap_or_default();
            let body = match read_body(&mut req, 8 * 1024 * 1024) {
                Ok(b) => b,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            if !body.starts_with(&[0xFF, 0xD8, 0xFF]) {
                return respond_json(req, 400, json!({"error": "Bukan gambar JPG."}));
            }
            let path = eleven::out_dir(dir).join(format!("{}.jpg", file_slug(&name, "thumbnail")));
            match fs::write(&path, &body) {
                Ok(()) => respond_json(req, 200, json!({"path": path.display().to_string()})),
                Err(e) => respond_json(req, 500, json!({"error": format!("Gagal menyimpan JPG: {e}")})),
            }
        }
        (Method::Post, "/api/eleven/info") => {
            let v = match read_json(&mut req, 64 * 1024) {
                Ok(v) => v,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            let key = v["key"].as_str().map(str::trim).filter(|k| !k.is_empty()).map(String::from)
                .unwrap_or_else(|| cfg.lock().unwrap().eleven_key.clone());
            if key.is_empty() {
                return respond_json(req, 400, json!({"error": "API key ElevenLabs belum diisi."}));
            }
            match eleven::info(&key) {
                Ok(info) => respond_json(req, 200, info),
                Err(e) => respond_json(req, 502, json!({"error": e})),
            }
        }
        (Method::Post, "/api/eleven/tts") => {
            let v = match read_json(&mut req, 512 * 1024) {
                Ok(v) => v,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            let c = cfg.lock().unwrap().clone();
            let voice = v["voice"].as_str().filter(|x| !x.is_empty()).unwrap_or(&c.eleven_voice).to_string();
            let parts: Vec<String> = v["parts"].as_array().into_iter().flatten().filter_map(|p| p.as_str().map(String::from)).collect();
            match eleven::speak(&c, dir, v["title"].as_str().unwrap_or("voice over"), &parts, &voice) {
                Ok(out) => respond_json(req, 200, out),
                Err(e) => respond_json(req, 502, json!({"error": e})),
            }
        }
        (Method::Post, "/api/yt/connect") => {
            let c = cfg.lock().unwrap().clone();
            match youtube::auth_url(&c, port) {
                Ok(url) => {
                    open_browser(&url);
                    respond_json(req, 200, json!({"url": url}))
                }
                Err(e) => respond_json(req, 400, json!({"error": e})),
            }
        }
        (Method::Get, "/oauth/callback") => {
            let q = req.url().split_once('?').map(|(_, q)| q.to_string()).unwrap_or_default();
            let get = |k: &str| q.split('&').find_map(|kv| kv.split_once('=').filter(|(a, _)| *a == k).map(|(_, b)| percent_decode(b)));
            let result = match (get("code"), get("state"), get("error")) {
                (_, _, Some(err)) => Err(format!("Google menolak izin ({err}).")),
                (Some(code), Some(state), None) => {
                    let c = cfg.lock().unwrap().clone();
                    youtube::finish(&c, port, &code, &state).and_then(|refresh| {
                        let mut c = cfg.lock().unwrap();
                        c.yt_refresh = refresh;
                        save_config(dir, &c)
                    })
                }
                _ => Err("Balasan Google tidak lengkap. Coba hubungkan lagi.".into()),
            };
            let (title, msg) = match &result {
                Ok(()) => ("Terhubung", "YouTube sudah terhubung ke Terra Studio. Tutup tab ini dan kembali ke aplikasi, lalu klik Tarik data sekarang.".to_string()),
                Err(e) => ("Gagal", e.clone()),
            };
            let html = format!("<!doctype html><meta charset=utf-8><title>Terra Studio</title><body style=\"font:18px system-ui;margin:15vh auto;max-width:520px;padding:0 16px;color:#1C1F1A\"><h1 style=\"font-size:24px\">{title}</h1><p>{}</p></body>", msg.replace('<', "&lt;"));
            respond(req, 200, "text/html; charset=utf-8", html.into_bytes())
        }
        (Method::Post, "/api/yt/sync") => {
            let c = cfg.lock().unwrap().clone();
            match youtube::sync(&c, dir) {
                Ok((channel, summary)) => {
                    let mut c = cfg.lock().unwrap();
                    c.yt_channel = channel;
                    c.yt_synced = now();
                    let _ = save_config(dir, &c);
                    respond_json(req, 200, summary)
                }
                Err(e) => respond_json(req, 400, json!({"error": e})),
            }
        }
        (Method::Post, "/api/open-folder") => {
            let d = eleven::out_dir(dir);
            #[cfg(windows)]
            let _ = Command::new("explorer.exe").arg(&d).spawn();
            #[cfg(not(windows))]
            let _ = Command::new("xdg-open").arg(&d).spawn();
            respond_json(req, 200, json!({"ok": true}))
        }
        // Backup: profile, settings (never the API key), Studio data, plans and AI results.
        (Method::Get, "/api/backup") => {
            let c = cfg.lock().unwrap().clone();
            respond_json(req, 200, json!({
                "app": "terra-studio", "version": 1, "created": now(),
                "settings": {"model": c.model, "effort": c.effort, "profile": c.profile},
                "data": load_data(dir),
                "last": load_json(&dir.join("last.json")),
            }))
        }
        (Method::Post, "/api/restore") => {
            let v = match read_json(&mut req, MAX_UPLOAD) {
                Ok(v) => v,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            if v["app"] != "terra-studio" {
                return respond_json(req, 400, json!({"error": "File ini bukan backup Terra Studio."}));
            }
            let result = (|| -> Result<(), String> {
                if v["data"].is_object() {
                    let _g = DATA_JSON_LOCK.lock().unwrap();
                    write_atomic(&dir.join("data.json"), v["data"].to_string().as_bytes()).map_err(|e| e.to_string())?;
                }
                if v["last"].is_object() {
                    let _g = LAST_JSON_LOCK.lock().unwrap();
                    write_atomic(&dir.join("last.json"), v["last"].to_string().as_bytes()).map_err(|e| e.to_string())?;
                }
                let mut c = cfg.lock().unwrap();
                let st = &v["settings"];
                if let Some(m) = st["model"].as_str().filter(|m| MODELS.contains(m)) {
                    c.model = m.to_string();
                }
                if let Some(e) = st["effort"].as_str().filter(|e| ["low", "medium", "high"].contains(e)) {
                    c.effort = e.to_string();
                }
                if st["profile"].is_object() {
                    c.profile = st["profile"].clone();
                }
                save_config(dir, &c)
            })();
            match result {
                Ok(()) => respond_json(req, 200, json!({"ok": true})),
                Err(e) => respond_json(req, 500, json!({"error": format!("Gagal memulihkan backup: {e}")})),
            }
        }
        // Last AI result per tool, so reopening the app does not cost a new call.
        (Method::Get, "/api/last") => respond_json(req, 200, load_json(&dir.join("last.json"))),
        (Method::Post, "/api/last") => {
            let v = match read_json(&mut req, 2 * 1024 * 1024) {
                Ok(v) => v,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            let Some(key) = v["key"].as_str().filter(|k| k.len() <= 32) else {
                return respond_json(req, 400, json!({"error": "bad key"}));
            };
            let _guard = LAST_JSON_LOCK.lock().unwrap();
            let path = dir.join("last.json");
            let mut all = load_json(&path);
            all[key] = v["value"].clone();
            match write_atomic(&path, all.to_string().as_bytes()) {
                Ok(()) => respond_json(req, 200, json!({"ok": true})),
                Err(e) => respond_json(req, 500, json!({"error": e.to_string()})),
            }
        }
        (Method::Delete, "/api/data") => {
            let _guard = DATA_JSON_LOCK.lock().unwrap();
            let _ = fs::remove_file(dir.join("data.json"));
            respond_json(req, 200, json!({"ok": true}))
        }
        (Method::Post, "/api/import") => {
            let name = header(&req, "X-Filename").map(|s| percent_decode(&s)).unwrap_or_default();
            let body = match read_body(&mut req, MAX_UPLOAD) {
                Ok(b) => b,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            match import(&body, &name, dir) {
                Ok(found) => respond_json(req, 200, json!({"found": found})),
                Err(e) => respond_json(req, 400, json!({"error": e})),
            }
        }
        (Method::Post, "/api/thumb") | (Method::Delete, "/api/thumb") => {
            let id = query_param(req.url(), "id").unwrap_or_default();
            let Some(stem) = thumb_stem(&id) else {
                return respond_json(req, 400, json!({"error": "id tidak valid"}));
            };
            let tdir = dir.join("thumbs");
            for ext in ["jpg", "png", "webp"] {
                let _ = fs::remove_file(tdir.join(format!("{stem}.{ext}")));
            }
            if *req.method() == Method::Delete {
                return respond_json(req, 200, json!({"ok": true}));
            }
            let body = match read_body(&mut req, 8 * 1024 * 1024) {
                Ok(b) => b,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            let ext = match body.get(..12) {
                Some(h) if h.starts_with(&[0xFF, 0xD8, 0xFF]) => "jpg",
                Some(h) if h.starts_with(b"\x89PNG") => "png",
                Some(h) if h.starts_with(b"RIFF") && &h[8..12] == b"WEBP" => "webp",
                _ => return respond_json(req, 400, json!({"error": "Pilih gambar JPG, PNG, atau WebP."})),
            };
            let _ = fs::create_dir_all(&tdir);
            match fs::write(tdir.join(format!("{stem}.{ext}")), &body) {
                Ok(()) => respond_json(req, 200, json!({"ok": true})),
                Err(e) => respond_json(req, 500, json!({"error": format!("Gagal menyimpan gambar: {e}")})),
            }
        }
        (Method::Get, p) if p.starts_with("/api/thumb/") => {
            let id = percent_decode(&p["/api/thumb/".len()..]);
            let found = thumb_stem(&id).and_then(|stem| {
                [("jpg", "image/jpeg"), ("png", "image/png"), ("webp", "image/webp")]
                    .iter()
                    .find_map(|(ext, ct)| fs::read(dir.join("thumbs").join(format!("{stem}.{ext}"))).ok().map(|b| (b, *ct)))
            });
            match found {
                Some((bytes, ct)) => respond(req, 200, ct, bytes),
                None => respond_json(req, 404, json!({"error": "not found"})),
            }
        }
        // The window tells us its theme so the title bar can match it.
        (Method::Post, "/api/skin") => {
            let skin = query_param(req.url(), "skin").unwrap_or_default();
            #[cfg(windows)]
            native::tint_title(&skin);
            #[cfg(not(windows))]
            let _ = skin;
            respond_json(req, 200, json!({"ok": true}))
        }
        (Method::Post, "/api/show") => {
            #[cfg(windows)]
            let shown = native::show();
            #[cfg(not(windows))]
            let shown = false;
            respond_json(req, 200, json!({"shown": shown}));
        }
        (Method::Post, "/api/quit") => {
            respond_json(req, 200, json!({"ok": true}));
            std::thread::sleep(Duration::from_millis(300));
            std::process::exit(0);
        }
        (Method::Get, p) if p.starts_with("/api/audio/") => {
            let name = percent_decode(&p["/api/audio/".len()..]);
            let ok = name.ends_with(".mp3") && !name.contains(['/', '\\']) && !name.contains("..");
            match ok.then(|| fs::read(eleven::out_dir(dir).join(&name)).ok()).flatten() {
                Some(bytes) => respond(req, 200, "audio/mpeg", bytes),
                None => respond_json(req, 404, json!({"error": "not found"})),
            }
        }
        _ => respond_json(req, 404, json!({"error": "not found"})),
    }
}

/// Plan ids become file names, so only short alphanumeric ids are accepted.
fn thumb_stem(id: &str) -> Option<String> {
    (!id.is_empty() && id.len() <= 40 && id.chars().all(|c| c.is_ascii_alphanumeric())).then(|| id.to_string())
}

/// Lowercase words joined by dashes, safe as a Windows file name.
fn file_slug(s: &str, fallback: &str) -> String {
    let slug = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|w| !w.is_empty())
        .take(10)
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() { fallback.to_string() } else { slug }
}

/// Standard and URL-safe base64; whitespace and padding are skipped.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u8);
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn query_param(url: &str, key: &str) -> Option<String> {
    url.split_once('?')?.1.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| percent_decode(v))
    })
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// Claude API

/// The provider used for generation: an explicit choice wins, otherwise
/// whichever key is saved (Claude first). Empty means copy-paste mode.
fn active_provider(c: &Config) -> &'static str {
    match c.provider.as_str() {
        "manual" => "",
        "claude" if !c.api_key.is_empty() => "claude",
        "gemini" if !c.gemini_key.is_empty() => "gemini",
        _ if !c.api_key.is_empty() => "claude",
        _ if !c.gemini_key.is_empty() => "gemini",
        _ => "",
    }
}

fn generate(c: &Config, prompt: &str, test: bool) -> (u16, Value) {
    if active_provider(c) == "gemini" {
        return gemini::generate(c, prompt, test);
    }
    if c.api_key.is_empty() {
        return (400, json!({"error": "API key belum diisi. Buka tab Pengaturan dan tempel API key Anthropic Anda."}));
    }
    if prompt.trim().is_empty() {
        return (400, json!({"error": "Permintaan kosong."}));
    }
    let model = if MODELS.contains(&c.model.as_str()) { c.model.as_str() } else { MODELS[1] };
    let effort = if test { "low" } else if ["low", "medium", "high"].contains(&c.effort.as_str()) { c.effort.as_str() } else { "medium" };
    let mut body = json!({
        "model": model,
        "max_tokens": if test { 1024 } else { 16000 },
        "output_config": {"effort": effort},
        "messages": [{"role": "user", "content": prompt}],
    });
    // Server-side fallback reroutes a safety refusal to another model instead
    // of failing. Haiku has no fallback, so it is sent only for Opus/Sonnet.
    let use_fallback = model != "claude-haiku-5-5";
    if use_fallback {
        body["fallbacks"] = json!("default");
    }

    let mut result = call_claude(&c.api_key, &body, use_fallback);
    if let Err((400, ref msg)) = result {
        if msg.contains("fallback") {
            if let Some(obj) = body.as_object_mut() {
                obj.remove("fallbacks");
            }
            result = call_claude(&c.api_key, &body, false);
        }
    }

    match result {
        Ok(resp) => {
            let stop = resp["stop_reason"].as_str().unwrap_or("");
            if stop == "refusal" {
                return (422, json!({"error": "Claude menolak permintaan ini. Ubah topik atau kata-katanya, lalu coba lagi.", "model": resp["model"], "usage": usage(&resp)}));
            }
            let text: String = resp["content"]
                .as_array()
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter(|b| b["type"] == "text")
                        .filter_map(|b| b["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default();
            if text.trim().is_empty() {
                let why = if stop == "max_tokens" {
                    "Jawaban terpotong sebelum selesai. Coba lagi, atau turunkan Kedalaman berpikir di Pengaturan."
                } else {
                    "Claude tidak mengirim jawaban. Coba lagi."
                };
                return (502, json!({"error": why, "model": resp["model"], "usage": usage(&resp)}));
            }
            (
                200,
                json!({
                    "text": text,
                    "model": resp["model"],
                    "stop_reason": stop,
                    "usage": usage(&resp),
                }),
            )
        }
        Err((code, msg)) => {
            let api_msg = serde_json::from_str::<Value>(&msg)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(String::from))
                .unwrap_or(msg.clone());
            let lower = api_msg.to_lowercase();
            let friendly = match code {
                0 => format!("Tidak bisa terhubung ke api.anthropic.com. Periksa koneksi internet. ({api_msg})"),
                401 => "API key ditolak. Periksa kunci di tab Pengaturan.".to_string(),
                403 => format!("Akses ditolak oleh Anthropic. Pesan asli: {api_msg}"),
                _ if lower.contains("credit") || lower.contains("billing") => {
                    "Saldo API habis. Isi saldo di console.anthropic.com, bagian Billing.".to_string()
                }
                429 => "Batas pemakaian tercapai. Tunggu sebentar lalu coba lagi.".to_string(),
                500..=599 => "Server Claude sedang sibuk. Coba lagi beberapa saat lagi.".to_string(),
                _ => format!("Permintaan gagal (kode {code}). Pesan asli: {api_msg}"),
            };
            (502, json!({"error": friendly}))
        }
    }
}

/// Token counts for cost display. With fallbacks, every attempt is billed, so
/// the per-attempt list is summed when present.
fn usage(resp: &Value) -> Value {
    let sum = |key: &str| -> u64 {
        match resp["usage"]["iterations"].as_array() {
            Some(list) if !list.is_empty() => list.iter().map(|it| it[key].as_u64().unwrap_or(0)).sum(),
            _ => resp["usage"][key].as_u64().unwrap_or(0),
        }
    };
    json!({"input_tokens": sum("input_tokens"), "output_tokens": sum("output_tokens")})
}

fn call_claude(key: &str, body: &Value, fallback: bool) -> Result<Value, (u16, String)> {
    // Native certificate store, so antivirus HTTPS scanning or a company proxy
    // whose root certificate is installed in Windows still works.
    let agent = ureq::AgentBuilder::new()
        .try_proxy_from_env(true)
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(600))
        .build();
    let mut r = agent
        .post(API_URL)
        .set("x-api-key", key)
        .set("anthropic-version", API_VERSION)
        .set("content-type", "application/json");
    if fallback {
        r = r.set("anthropic-beta", "server-side-fallback-2026-07-01");
    }
    match r.send_json(body.clone()) {
        Ok(resp) => resp.into_json::<Value>().map_err(|e| (0, e.to_string())),
        Err(ureq::Error::Status(code, resp)) => Err((code, resp.into_string().unwrap_or_default())),
        Err(e) => Err((0, e.to_string())),
    }
}

// Gemini API

mod gemini {
    use super::*;
    const BASE: &str = "https://generativelanguage.googleapis.com/v1beta";

    fn agent() -> ureq::Agent {
        ureq::AgentBuilder::new()
            .try_proxy_from_env(true)
            .timeout_connect(Duration::from_secs(20))
            .timeout_read(Duration::from_secs(600))
            .build()
    }

    pub fn friendly(code: u16, body: &str) -> String {
        let msg = serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(String::from))
            .unwrap_or_else(|| body.chars().take(300).collect());
        let lower = msg.to_lowercase();
        match code {
            0 => format!("Tidak bisa terhubung ke Google Gemini. Periksa koneksi internet. ({msg})"),
            400 if lower.contains("api key") || lower.contains("api_key") => {
                "API key Gemini ditolak. Periksa kunci di tab Pengaturan.".into()
            }
            401 | 403 => format!("Akses ke Gemini ditolak. Pesan asli: {msg}"),
            404 => "Model Gemini itu tidak tersedia. Pilih model lain di Pengaturan.".into(),
            429 => "Kuota gratis Gemini sedang habis. Tunggu sebentar (atau sampai besok), lalu coba lagi.".into(),
            500..=599 => "Server Gemini sedang sibuk. Coba lagi beberapa saat lagi.".into(),
            _ => format!("Permintaan ke Gemini gagal (kode {code}). Pesan asli: {msg}"),
        }
    }

    /// Text models the key can use, newest first.
    pub fn models(key: &str) -> Result<Vec<String>, (u16, String)> {
        let resp = agent()
            .get(&format!("{BASE}/models?pageSize=200"))
            .set("x-goog-api-key", key)
            .call();
        let v: Value = match resp {
            Ok(r) => r.into_json().map_err(|e| (0, e.to_string()))?,
            Err(ureq::Error::Status(code, r)) => return Err((code, r.into_string().unwrap_or_default())),
            Err(e) => return Err((0, e.to_string())),
        };
        let mut out: Vec<String> = v["models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| {
                m["supportedGenerationMethods"].as_array().map_or(false, |a| a.iter().any(|x| x == "generateContent"))
            })
            .filter_map(|m| m["name"].as_str().map(|n| n.trim_start_matches("models/").to_string()))
            .filter(|n| n.starts_with("gemini") && !["tts", "image", "live", "audio", "embedding", "robotics", "computer"].iter().any(|x| n.contains(x)))
            .collect();
        out.sort_by(|a, b| version_key(b).partial_cmp(&version_key(a)).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(b)));
        Ok(out)
    }

    /// Sort key: version number, then stable before preview/experimental.
    fn version_key(name: &str) -> (f64, u8) {
        let ver = name
            .split('-')
            .nth(1)
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        let stable = !(name.contains("preview") || name.contains("exp"));
        (ver, stable as u8)
    }

    /// Newest stable Flash (not Lite), else newest Flash, else the first model.
    pub fn pick_default(list: &[String]) -> Option<String> {
        let flash = |n: &&String| n.contains("flash") && !n.contains("lite");
        list.iter()
            .filter(flash)
            .find(|n| !(n.contains("preview") || n.contains("exp")))
            .or_else(|| list.iter().find(flash))
            .or_else(|| list.first())
            .cloned()
    }

    pub const IMAGE_DEFAULT: &str = "gemini-3.1-flash-image";

    /// Development builds can point image calls at a local stand-in server.
    fn api_root() -> String {
        #[cfg(debug_assertions)]
        if let Ok(b) = std::env::var("TERRA_GEMINI_BASE") {
            return b;
        }
        "https://generativelanguage.googleapis.com".to_string()
    }

    pub fn safe_model(m: &str) -> bool {
        !m.is_empty() && m.len() < 80 && m.chars().all(|ch| ch.is_ascii_alphanumeric() || "-._".contains(ch))
    }

    pub fn image_model(c: &Config) -> String {
        if c.image_model.is_empty() { IMAGE_DEFAULT.to_string() } else { c.image_model.clone() }
    }

    /// Image errors are mostly about billing, which the text errors above do not explain.
    pub fn friendly_image(code: u16, body: &str) -> String {
        let msg = serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(String::from))
            .unwrap_or_else(|| body.chars().take(300).collect());
        let lower = msg.to_lowercase();
        match code {
            402 => "Saldo prabayar Gemini habis. Isi ulang di Google AI Studio, menu Billing, lalu coba lagi.".into(),
            429 if lower.contains("free_tier") || lower.contains("free tier") || lower.contains("limit: 0") => {
                "Kunci ini masih di tingkat gratis, dan gambar hanya tersedia di tingkat berbayar. Di Google AI Studio klik Set up billing untuk proyek kunci ini.".into()
            }
            429 => format!("Batas pemakaian Gemini tercapai. Tunggu sebentar, lalu coba lagi. Pesan asli: {msg}"),
            403 if lower.contains("billing") => {
                "Penagihan belum aktif untuk kunci ini. Di Google AI Studio klik Set up billing, lalu coba lagi.".into()
            }
            404 => "Model gambar itu tidak tersedia untuk kunci ini. Pilih model lain di Pengaturan.".into(),
            _ => friendly(code, body),
        }
    }

    /// Image models the key can use.
    pub fn image_models(key: &str) -> Result<Vec<String>, (u16, String)> {
        let resp = agent().get(&format!("{BASE}/models?pageSize=200")).set("x-goog-api-key", key).call();
        let v: Value = match resp {
            Ok(r) => r.into_json().map_err(|e| (0, e.to_string()))?,
            Err(ureq::Error::Status(code, r)) => return Err((code, r.into_string().unwrap_or_default())),
            Err(e) => return Err((0, e.to_string())),
        };
        let mut out: Vec<String> = v["models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| m["supportedGenerationMethods"].as_array().map_or(false, |a| a.iter().any(|x| x == "generateContent")))
            .filter_map(|m| m["name"].as_str().map(|n| n.trim_start_matches("models/").to_string()))
            .filter(|n| n.starts_with("gemini") && (n.contains("image") || n.contains("banana")))
            .collect();
        out.sort();
        Ok(out)
    }

    /// The final image of a generateContent reply, with its file extension.
    /// Draft images the model marks as "thought" are skipped.
    pub fn image_part(v: &Value) -> Option<(Vec<u8>, &'static str)> {
        v["candidates"][0]["content"]["parts"].as_array()?.iter().rev().filter(|p| p["thought"].as_bool() != Some(true)).find_map(|p| {
            let d = if p["inlineData"].is_object() { &p["inlineData"] } else { &p["inline_data"] };
            let mime = d["mimeType"].as_str().or_else(|| d["mime_type"].as_str()).unwrap_or("image/png");
            let ext = match mime {
                "image/jpeg" => "jpg",
                "image/webp" => "webp",
                _ => "png",
            };
            let bytes = base64_decode(d["data"].as_str()?)?;
            (!bytes.is_empty()).then_some((bytes, ext))
        })
    }

    /// One 16:9 image. Google renamed the size options between API versions,
    /// so a request the server rejects as malformed is retried in the older shape.
    pub fn image(key: &str, model: &str, prompt: &str) -> Result<(Vec<u8>, &'static str), String> {
        let contents = json!([{"role": "user", "parts": [{"text": prompt}]}]);
        let root = api_root();
        let shapes = [
            (format!("{root}/v1"), json!({"responseModalities": ["TEXT", "IMAGE"], "responseFormat": {"image": {"aspectRatio": "16:9", "imageSize": "1K"}}})),
            (format!("{root}/v1beta"), json!({"responseModalities": ["TEXT", "IMAGE"], "imageConfig": {"aspectRatio": "16:9", "imageSize": "1K"}})),
        ];
        let mut first_err = None;
        for (base, config) in shapes {
            let resp = agent()
                .post(&format!("{base}/models/{model}:generateContent"))
                .set("x-goog-api-key", key)
                .set("content-type", "application/json")
                .send_json(json!({"contents": contents, "generationConfig": config}));
            let v: Value = match resp {
                Ok(r) => r.into_json().map_err(|e| friendly(0, &e.to_string()))?,
                Err(ureq::Error::Status(code, r)) => {
                    let body = r.into_string().unwrap_or_default();
                    let lower = body.to_lowercase();
                    let key_problem = lower.contains("api key") || lower.contains("api_key");
                    if (code == 400 || code == 404) && !key_problem {
                        first_err.get_or_insert(friendly_image(code, &body));
                        continue;
                    }
                    return Err(friendly_image(code, &body));
                }
                Err(e) => return Err(friendly(0, &e.to_string())),
            };
            return image_part(&v).ok_or_else(|| {
                let finish = v["candidates"][0]["finishReason"].as_str().unwrap_or("");
                if finish.contains("SAFETY") || finish.contains("PROHIBITED") || v["promptFeedback"]["blockReason"].is_string() {
                    "Gemini menolak membuat gambar ini. Klik Buat ulang, atau ubah konsepnya.".to_string()
                } else {
                    "Gemini tidak mengirim gambar. Klik Buat ulang.".to_string()
                }
            });
        }
        Err(first_err.unwrap_or_else(|| "Gemini tidak mengirim gambar. Coba lagi.".to_string()))
    }

    pub fn generate(c: &Config, prompt: &str, test: bool) -> (u16, Value) {
        if prompt.trim().is_empty() {
            return (400, json!({"error": "Permintaan kosong."}));
        }
        let model = if c.gemini_model.is_empty() {
            match models(&c.gemini_key) {
                Ok(list) => match pick_default(&list) {
                    Some(m) => m,
                    None => return (502, json!({"error": "Tidak ada model Gemini yang bisa dipakai dengan kunci ini."})),
                },
                Err((code, body)) => return (502, json!({"error": friendly(code, &body)})),
            }
        } else {
            c.gemini_model.clone()
        };
        let mut config = json!({"maxOutputTokens": if test { 256 } else { 16000 }});
        if !test {
            config["responseMimeType"] = json!("application/json");
        }
        let body = json!({
            "contents": [{"role": "user", "parts": [{"text": prompt}]}],
            "generationConfig": config,
        });
        let resp = agent()
            .post(&format!("{BASE}/models/{model}:generateContent"))
            .set("x-goog-api-key", &c.gemini_key)
            .set("content-type", "application/json")
            .send_json(body);
        let v: Value = match resp {
            Ok(r) => match r.into_json() {
                Ok(v) => v,
                Err(e) => return (502, json!({"error": friendly(0, &e.to_string())})),
            },
            Err(ureq::Error::Status(code, r)) => {
                return (502, json!({"error": friendly(code, &r.into_string().unwrap_or_default())}))
            }
            Err(e) => return (502, json!({"error": friendly(0, &e.to_string())})),
        };
        let cand = &v["candidates"][0];
        let finish = cand["finishReason"].as_str().unwrap_or("");
        let text: String = cand["content"]["parts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["thought"].as_bool() != Some(true))
            .filter_map(|p| p["text"].as_str())
            .collect();
        let usage = json!({
            "input_tokens": v["usageMetadata"]["promptTokenCount"].as_u64().unwrap_or(0),
            "output_tokens": v["usageMetadata"]["candidatesTokenCount"].as_u64().unwrap_or(0),
        });
        if text.trim().is_empty() {
            let why = match finish {
                "MAX_TOKENS" => "Jawaban Gemini terpotong sebelum selesai. Coba lagi.".to_string(),
                "SAFETY" | "PROHIBITED_CONTENT" | "BLOCKLIST" | "RECITATION" => {
                    "Gemini menolak permintaan ini. Ubah topik atau kata-katanya, lalu coba lagi.".to_string()
                }
                _ if v["promptFeedback"]["blockReason"].is_string() => {
                    "Gemini menolak permintaan ini. Ubah topik atau kata-katanya, lalu coba lagi.".to_string()
                }
                _ => "Gemini tidak mengirim jawaban. Coba lagi.".to_string(),
            };
            return (502, json!({"error": why, "provider": "gemini", "model": model, "usage": usage}));
        }
        (200, json!({"text": text, "provider": "gemini", "model": model, "stop_reason": finish, "usage": usage}))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn reads_image_part_in_both_spellings() {
            let a = json!({"candidates": [{"content": {"parts": [{"text": "ok"}, {"inlineData": {"mimeType": "image/jpeg", "data": "/9j/4A=="}}]}}]});
            assert_eq!(image_part(&a), Some((vec![0xFF, 0xD8, 0xFF, 0xE0], "jpg")));
            let b = json!({"candidates": [{"content": {"parts": [{"inline_data": {"mime_type": "image/png", "data": "iVBORw=="}}]}}]});
            assert_eq!(image_part(&b), Some((vec![0x89, b'P', b'N', b'G'], "png")));
            assert_eq!(image_part(&json!({"candidates": [{"content": {"parts": [{"text": "no"}]}}]})), None);
            let c = json!({"candidates": [{"content": {"parts": [{"thought": true, "inlineData": {"mimeType": "image/png", "data": "AAAA"}}, {"inlineData": {"mimeType": "image/webp", "data": "UklG"}}]}}]});
            assert_eq!(image_part(&c), Some((b"RIF".to_vec(), "webp")));
        }
        #[test]
        fn billing_errors_are_explained() {
            let free = r#"{"error":{"message":"Quota exceeded for metric generate_content_free_tier_requests, limit: 0"}}"#;
            assert!(friendly_image(429, free).contains("Set up billing"));
            assert!(friendly_image(402, "{}").contains("Saldo"));
        }
        #[test]
        fn picks_newest_stable_flash() {
            let mut l: Vec<String> = ["gemini-2.5-flash", "gemini-3.0-flash-preview", "gemini-3.0-pro", "gemini-2.5-flash-lite", "gemini-3.0-flash"]
                .iter().map(|s| s.to_string()).collect();
            l.sort_by(|a, b| version_key(b).partial_cmp(&version_key(a)).unwrap().then(a.cmp(b)));
            assert_eq!(pick_default(&l).as_deref(), Some("gemini-3.0-flash"));
        }
    }
}

// ElevenLabs text to speech

mod eleven {
    use super::*;
    const BASE: &str = "https://api.elevenlabs.io";
    /// Characters per request; well under every model's limit, and short
    /// enough that one failed chunk is cheap to retry.
    const CHUNK: usize = 2400;
    pub const DEFAULT_MODEL: &str = "eleven_multilingual_v2";

    fn agent() -> ureq::Agent {
        ureq::AgentBuilder::new()
            .try_proxy_from_env(true)
            .timeout_connect(Duration::from_secs(20))
            .timeout_read(Duration::from_secs(300))
            .build()
    }

    fn friendly(code: u16, body: &str) -> String {
        let detail = serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|v| v["detail"]["message"].as_str().or(v["detail"].as_str()).map(String::from))
            .unwrap_or_else(|| body.chars().take(200).collect());
        let lower = detail.to_lowercase();
        match code {
            0 => format!("Tidak bisa terhubung ke ElevenLabs. Periksa koneksi internet. ({detail})"),
            401 => "API key ElevenLabs ditolak atau tidak punya izin ini. Periksa kunci di Pengaturan.".into(),
            _ if code == 402 || lower.contains("quota") || lower.contains("credits") => {
                "Kuota karakter ElevenLabs habis untuk periode ini.".into()
            }
            404 => "Suara ElevenLabs tidak ditemukan. Pilih suara lain di Pengaturan.".into(),
            429 => "ElevenLabs sedang sibuk atau batas permintaan tercapai. Coba lagi sebentar.".into(),
            _ => format!("ElevenLabs gagal (kode {code}): {detail}"),
        }
    }

    fn get(key: &str, path: &str) -> Result<Value, String> {
        match agent().get(&format!("{BASE}{path}")).set("xi-api-key", key).call() {
            Ok(r) => r.into_json().map_err(|e| friendly(0, &e.to_string())),
            Err(ureq::Error::Status(code, r)) => Err(friendly(code, &r.into_string().unwrap_or_default())),
            Err(e) => Err(friendly(0, &e.to_string())),
        }
    }

    /// Remaining characters, voices and TTS models on this account.
    pub fn info(key: &str) -> Result<Value, String> {
        let sub = get(key, "/v1/user/subscription")?;
        let voices = get(key, "/v1/voices")?;
        let models = get(key, "/v1/models").unwrap_or(json!([]));
        let voices: Vec<Value> = voices["voices"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| {
                let labels: Vec<&str> = v["labels"].as_object().into_iter().flatten().filter_map(|(_, x)| x.as_str()).take(3).collect();
                json!({"id": v["voice_id"], "name": v["name"], "desc": labels.join(", "), "preview": v["preview_url"]})
            })
            .collect();
        let models: Vec<Value> = models
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| m["can_do_text_to_speech"].as_bool() != Some(false))
            .map(|m| json!({"id": m["model_id"], "name": m["name"]}))
            .collect();
        let used = sub["character_count"].as_u64().unwrap_or(0);
        let limit = sub["character_limit"].as_u64().unwrap_or(0);
        Ok(json!({"tier": sub["tier"], "used": used, "limit": limit, "left": limit.saturating_sub(used),
            "reset": sub["next_character_count_reset_unix"], "voices": voices, "models": models}))
    }

    /// Splits text at paragraph, then sentence, boundaries into pieces of at most CHUNK characters.
    pub fn chunks(parts: &[String]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut cur = String::new();
        let push = |piece: &str, cur: &mut String, out: &mut Vec<String>| {
            if !cur.is_empty() && cur.chars().count() + piece.chars().count() + 2 > CHUNK {
                out.push(std::mem::take(cur));
            }
            if !cur.is_empty() {
                cur.push_str("\n\n");
            }
            cur.push_str(piece);
        };
        for part in parts {
            for para in part.split("\n").map(str::trim).filter(|p| !p.is_empty()) {
                if para.chars().count() <= CHUNK {
                    push(para, &mut cur, &mut out);
                    continue;
                }
                let mut sentence = String::new();
                for ch in para.chars() {
                    sentence.push(ch);
                    if matches!(ch, '.' | '!' | '?') {
                        push(sentence.trim(), &mut cur, &mut out);
                        sentence.clear();
                    }
                }
                if !sentence.trim().is_empty() {
                    push(sentence.trim(), &mut cur, &mut out);
                }
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        out
    }

    pub fn out_dir(app: &Path) -> PathBuf {
        let base = std::env::var_os("USERPROFILE")
            .map(|h| PathBuf::from(h).join("Downloads"))
            .unwrap_or_else(|| app.join("audio"));
        let _ = fs::create_dir_all(&base);
        base
    }

    fn file_name(title: &str) -> String {
        let slug: String = title
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .take(8)
            .collect::<Vec<_>>()
            .join("-");
        let slug = if slug.is_empty() { "voice-over".to_string() } else { slug };
        format!("{slug}-{}.mp3", now())
    }

    pub fn speak(c: &Config, dir: &Path, title: &str, parts: &[String], voice: &str) -> Result<Value, String> {
        if c.eleven_key.is_empty() {
            return Err("API key ElevenLabs belum diisi. Buka Pengaturan, bagian Suara.".into());
        }
        if voice.is_empty() {
            return Err("Pilih suara ElevenLabs dulu di Pengaturan.".into());
        }
        let pieces = chunks(parts);
        if pieces.is_empty() {
            return Err("Naskahnya kosong.".into());
        }
        let model = if c.eleven_model.is_empty() { DEFAULT_MODEL } else { c.eleven_model.as_str() };
        let mut audio: Vec<u8> = Vec::new();
        for (i, text) in pieces.iter().enumerate() {
            let mut body = json!({"text": text, "model_id": model});
            if i > 0 {
                body["previous_text"] = json!(pieces[i - 1].chars().rev().take(500).collect::<String>().chars().rev().collect::<String>());
            }
            if let Some(next) = pieces.get(i + 1) {
                body["next_text"] = json!(next.chars().take(500).collect::<String>());
            }
            let resp = agent()
                .post(&format!("{BASE}/v1/text-to-speech/{voice}?output_format=mp3_44100_128"))
                .set("xi-api-key", &c.eleven_key)
                .set("content-type", "application/json")
                .send_json(body);
            match resp {
                Ok(r) => {
                    let mut buf = Vec::new();
                    r.into_reader().take(50 * 1024 * 1024).read_to_end(&mut buf).map_err(|e| friendly(0, &e.to_string()))?;
                    audio.extend_from_slice(&buf);
                }
                Err(ureq::Error::Status(code, r)) => {
                    let msg = friendly(code, &r.into_string().unwrap_or_default());
                    return Err(if i == 0 { msg } else { format!("{msg} (berhenti di bagian {} dari {})", i + 1, pieces.len()) });
                }
                Err(e) => return Err(friendly(0, &e.to_string())),
            }
        }
        let name = file_name(title);
        let path = out_dir(dir).join(&name);
        fs::write(&path, &audio).map_err(|e| format!("Gagal menyimpan MP3: {e}"))?;
        let chars: usize = pieces.iter().map(|p| p.chars().count()).sum();
        Ok(json!({"file": name, "path": path.display().to_string(), "chars": chars, "chunks": pieces.len(), "bytes": audio.len()}))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn chunks_respect_limit_and_keep_text() {
            let long = "This is a sentence. ".repeat(400);
            let parts = vec!["Short intro.".to_string(), long.clone(), "Outro.".to_string()];
            let c = chunks(&parts);
            assert!(c.len() > 2);
            assert!(c.iter().all(|x| x.chars().count() <= CHUNK));
            let joined: String = c.join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(joined.starts_with("Short intro.") && joined.ends_with("Outro."));
            assert_eq!(joined.matches("sentence").count(), 400);
        }
        #[test]
        fn file_names_are_safe() {
            let n = file_name("Megistotherium: the Butcher / ../x");
            assert!(n.starts_with("megistotherium-the-butcher-x-") && n.ends_with(".mp3"));
        }
    }
}

// YouTube Studio data

/// First start on a PC: pick up files shipped next to the exe, so the user does not
/// have to paste the profile or import the Studio export by hand.
/// - profil-*.json fills an empty channel profile.
/// - *.zip / *.csv from a YouTube Studio export are imported when no data exists yet.
fn first_run_setup(dir: &Path, c: &mut Config) {
    let Some(exe_dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) else { return };
    let Ok(entries) = fs::read_dir(&exe_dir) else { return };
    let files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    let name_of = |p: &PathBuf| p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_lowercase();
    let profile_empty = c.profile["name"].as_str().map_or(true, |n| n.trim().is_empty());
    if profile_empty {
        if let Some(p) = files.iter().find(|p| { let n = name_of(p); n.starts_with("profil") && n.ends_with(".json") }) {
            if let Some(v) = fs::read(p).ok().filter(|b| b.len() < 64 * 1024).and_then(|b| serde_json::from_slice::<Value>(&b).ok()).filter(Value::is_object) {
                c.profile = v;
                let _ = save_config(dir, c);
            }
        }
    }
    if !dir.join("data.json").exists() {
        for p in files.iter().filter(|p| { let n = name_of(p); n.starts_with("content_") && (n.ends_with(".zip") || n.ends_with(".csv")) }) {
            if let Ok(b) = fs::read(p) {
                let _ = import(&b, &name_of(p), dir);
            }
        }
    }
}

fn import(bytes: &[u8], filename: &str, dir: &Path) -> Result<Vec<String>, String> {
    let _guard = DATA_JSON_LOCK.lock().unwrap();
    let mut data = load_data(dir);
    if !data.is_object() {
        data = json!({});
    }
    let mut found = Vec::new();
    if bytes.starts_with(b"PK") {
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|_| "File .zip tidak bisa dibuka.".to_string())?;
        for i in 0..zip.len() {
            let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
            if !f.name().to_lowercase().ends_with(".csv") {
                continue;
            }
            let mut raw = Vec::new();
            (&mut f).take(MAX_CSV + 1).read_to_end(&mut raw).map_err(|e| e.to_string())?;
            if raw.len() as u64 > MAX_CSV {
                return Err("Isi file .zip terlalu besar.".into());
            }
            if let Some(label) = parse_csv(&String::from_utf8_lossy(&raw), &mut data) {
                found.push(label);
            }
        }
    } else if let Some(label) = parse_csv(&String::from_utf8_lossy(bytes), &mut data) {
        found.push(label);
    }
    if found.is_empty() {
        return Ok(found);
    }
    if let Some(range) = date_range(filename) {
        data["range"] = json!(range);
    }
    data["imported_at"] = json!(now() * 1000);
    let bytes = serde_json::to_vec(&data).map_err(|e| e.to_string())?;
    write_atomic(&dir.join("data.json"), &bytes).map_err(|e| format!("Gagal menyimpan data: {e}"))?;
    Ok(found)
}

/// "Content 2024-08-25_2026-11-07 Channel.zip" -> "2024-08-25 sampai 2026-11-07"
fn date_range(name: &str) -> Option<String> {
    let b = name.as_bytes();
    let is_date = |s: &[u8]| {
        s.len() == 10
            && s.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() })
    };
    (0..b.len().saturating_sub(20)).find_map(|i| {
        let (a, sep, c) = (&b[i..i + 10], b[i + 10], &b[i + 11..i + 21]);
        (is_date(a) && sep == b'_' && is_date(c)).then(|| {
            format!("{} sampai {}", String::from_utf8_lossy(a), String::from_utf8_lossy(c))
        })
    })
}

fn num(s: &str) -> f64 {
    s.trim().replace(',', "").trim_end_matches('%').parse::<f64>().unwrap_or(0.0)
}

fn opt_num(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() { None } else { t.replace(',', "").trim_end_matches('%').parse::<f64>().ok() }
}

/// "Mar 14, 2026" -> "2026-03-14"; ISO dates pass through.
fn iso_date(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 10 && s.as_bytes()[4] == b'-' {
        return s.get(..10).unwrap_or("").to_string();
    }
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let parts: Vec<&str> = s.split(|c: char| c == ' ' || c == ',').filter(|p| !p.is_empty()).collect();
    if parts.len() == 3 {
        let m = MONTHS.iter().position(|m| parts[0].to_lowercase().starts_with(m));
        if let (Some(m), Ok(d), Ok(y)) = (m, parts[1].parse::<u32>(), parts[2].parse::<u32>()) {
            return format!("{y:04}-{:02}-{d:02}", m + 1);
        }
    }
    String::new()
}

fn parse_csv(text: &str, data: &mut Value) -> Option<String> {
    let text = text.trim_start_matches('\u{feff}');
    let mut rdr = csv::ReaderBuilder::new().flexible(true).from_reader(text.as_bytes());
    let headers: Vec<String> = rdr.headers().ok()?.iter().map(|h| h.trim().to_string()).collect();
    let rows: Vec<Vec<String>> = rdr
        .records()
        .filter_map(|r| r.ok())
        .map(|r| r.iter().map(|c| c.to_string()).collect())
        .collect();
    let lower: Vec<String> = headers.iter().map(|h| h.to_lowercase()).collect();
    let find = |pred: &dyn Fn(&str) -> bool| lower.iter().position(|h| pred(h));
    fn cell(row: &[String], idx: Option<usize>) -> &str {
        idx.and_then(|i| row.get(i)).map(|s| s.as_str()).unwrap_or("")
    }

    let views = find(&|h| h == "views");
    let hours = find(&|h| h.starts_with("watch time"));
    let subs = find(&|h| h == "subscribers");
    let revenue = find(&|h| h.starts_with("estimated revenue"));
    let impressions = find(&|h| h.contains("impressions") && !h.contains("click"));
    let ctr = find(&|h| h.contains("click-through"));
    let first = lower.first()?.as_str();

    match first {
        "content" => {
            let title = find(&|h| h == "video title");
            let published = find(&|h| h.starts_with("video publish"));
            let duration = find(&|h| h == "duration");
            let mut videos = Vec::new();
            for row in &rows {
                let id = cell(row, Some(0));
                let rec = json!({
                    "t": cell(row, title).lines().next().unwrap_or("").trim().chars().take(140).collect::<String>(),
                    "d": iso_date(cell(row, published)),
                    "dur": num(cell(row, duration)),
                    "v": num(cell(row, views)),
                    "h": (num(cell(row, hours)) * 10.0).round() / 10.0,
                    "s": num(cell(row, subs)),
                    "rev": num(cell(row, revenue)),
                    "imp": num(cell(row, impressions)),
                    "ctr": num(cell(row, ctr)),
                });
                if id.eq_ignore_ascii_case("total") {
                    let mut t = json!({"views": rec["v"], "hours": rec["h"], "subs": rec["s"], "imp": rec["imp"], "ctr": rec["ctr"]});
                    if revenue.is_some() {
                        t["revenue"] = rec["rev"].clone();
                    }
                    data["totals"] = t;
                } else if !id.is_empty() {
                    videos.push(rec);
                }
            }
            if let Some(i) = revenue {
                let h = &headers[i];
                // Only a short alphabetic code (IDR, USD) is kept, because the UI
                // inserts it into the page.
                let code: String = match (h.find('('), h.rfind(')')) {
                    (Some(a), Some(b)) if a < b => h[a + 1..b].trim().to_string(),
                    _ => String::new(),
                };
                if !code.is_empty() && code.len() <= 5 && code.chars().all(|c| c.is_ascii_alphabetic()) {
                    data["currency"] = json!(code.to_ascii_uppercase());
                }
            }
            let n = videos.len();
            data["videos"] = json!(videos);
            Some(format!("{n} video"))
        }
        "date" if headers.len() == 2 && views.is_some() => {
            let mut daily: Vec<(String, f64)> = rows
                .iter()
                .map(|r| (iso_date(cell(r, Some(0))), num(cell(r, views))))
                .filter(|(d, _)| !d.is_empty())
                .collect();
            daily.sort_by(|a, b| a.0.cmp(&b.0));
            let n = daily.len();
            data["daily"] = json!(daily.into_iter().map(|(d, v)| json!([d, v])).collect::<Vec<_>>());
            Some(format!("views harian ({n} hari)"))
        }
        "traffic source" | "geography" => {
            let mut list: Vec<Value> = rows
                .iter()
                .filter(|r| !cell(r, Some(0)).eq_ignore_ascii_case("total") && !cell(r, Some(0)).is_empty())
                .map(|r| {
                    json!({
                        "name": cell(r, Some(0)),
                        "views": num(cell(r, views)),
                        "hours": num(cell(r, hours)),
                        "rev": num(cell(r, revenue)),
                        "imp": num(cell(r, impressions)),
                        "ctr": opt_num(cell(r, ctr)),
                    })
                })
                .collect();
            list.sort_by(|a, b| b["views"].as_f64().unwrap_or(0.0).total_cmp(&a["views"].as_f64().unwrap_or(0.0)));
            if first == "geography" {
                data["geo"] = json!(list);
                Some("geografi".into())
            } else {
                data["traffic"] = json!(list);
                Some("sumber traffic".into())
            }
        }
        f if f.starts_with("viewer age") || f.starts_with("viewer gender") => {
            data["age"] = json!({"headers": headers, "rows": rows});
            Some("umur dan gender".into())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn currency_header_is_sanitised() {
        let csv = |h: &str| format!("Content,Video title,Views,{h}\nTotal,,10,5\nabc,Hi,10,5\n");
        let mut d = json!({});
        parse_csv(&csv("Estimated revenue (IDR)"), &mut d).unwrap();
        assert_eq!(d["currency"], "IDR");
        let mut d = json!({});
        parse_csv(&csv("Estimated revenue (<img src=x onerror=alert(1)>)"), &mut d).unwrap();
        assert!(d["currency"].is_null());
        let mut d = json!({});
        parse_csv(&csv("Estimated revenue) (USD"), &mut d).unwrap();
        assert!(d["currency"].is_null());
        assert_eq!(d["videos"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn hex_round_trip_and_plain_config() {
        assert_eq!(from_hex(&to_hex(&[0, 15, 255])), Some(vec![0, 15, 255]));
        assert_eq!(from_hex("abc"), None);
        let dir = std::env::temp_dir().join(format!("ts-test-{}", now()));
        fs::create_dir_all(&dir).unwrap();
        let mut c = Config::default();
        c.api_key = "sk-ant-x".into();
        save_config(&dir, &c).unwrap();
        assert_eq!(load_config(&dir).api_key, "sk-ant-x");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn usage_sums_fallback_attempts() {
        let r = json!({"usage": {"input_tokens": 5, "output_tokens": 7,
            "iterations": [{"input_tokens": 100, "output_tokens": 0}, {"input_tokens": 5, "output_tokens": 7}]}});
        assert_eq!(usage(&r), json!({"input_tokens": 105, "output_tokens": 7}));
        assert_eq!(usage(&json!({"usage": {"input_tokens": 1, "output_tokens": 2}})), json!({"input_tokens": 1, "output_tokens": 2}));
    }

    #[test]
    fn dates() {
        assert_eq!(iso_date("Mar 14, 2026"), "2026-03-14");
        assert_eq!(iso_date("2024-08-25"), "2024-08-25");
        assert_eq!(
            date_range("Content 2024-08-25_2026-11-07 My Channel.zip").as_deref(),
            Some("2024-08-25 sampai 2026-11-07")
        );
        assert_eq!(percent_decode("Content%202024.zip"), "Content 2024.zip");
        assert_eq!(iso_date("2024-08-2é"), "");
        assert_eq!(percent_decode("a%20"), "a ");
        assert_eq!(percent_decode("%e2%82"), "\u{fffd}");
        assert_eq!(percent_decode("x%%é"), "x%%é");
    }
}

/// YouTube Analytics through Google OAuth. The user creates an OAuth client of type
/// "Desktop app" in their own Google Cloud project; Google allows any loopback port
/// for that type, so the redirect lands on this app's own local server.
mod youtube {
    use super::*;
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    const SCOPES: &str = "https://www.googleapis.com/auth/youtube.readonly https://www.googleapis.com/auth/yt-analytics.readonly https://www.googleapis.com/auth/yt-analytics-monetary.readonly";
    static STATE: Mutex<String> = Mutex::new(String::new());

    fn agent() -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .try_proxy_from_env(true)
            .build()
    }

    fn random_hex() -> String {
        (0..2)
            .map(|i| {
                let mut h = RandomState::new().build_hasher();
                h.write_u64(now() ^ i);
                format!("{:016x}", h.finish())
            })
            .collect()
    }

    fn redirect(port: u16) -> String {
        format!("http://127.0.0.1:{port}/oauth/callback")
    }

    fn enc(s: &str) -> String {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
                _ => format!("%{b:02X}"),
            })
            .collect()
    }

    fn friendly(code: u16, body: &str) -> String {
        let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let msg = v["error"]["message"].as_str().or(v["error_description"].as_str()).or(v["error"].as_str()).unwrap_or("").to_string();
        let low = msg.to_lowercase();
        match code {
            0 => format!("Tidak bisa terhubung ke Google. Periksa koneksi internet. ({body})"),
            400 if low.contains("invalid_grant") || low.contains("expired") || low.contains("revoked") => {
                "Izin YouTube sudah kedaluwarsa atau dicabut. Klik Hubungkan YouTube lagi.".into()
            }
            401 => "Client ID atau Client secret ditolak Google. Periksa lagi di Pengaturan.".into(),
            403 if low.contains("has not been used") || low.contains("disabled") => {
                "API YouTube belum diaktifkan di Google Cloud. Aktifkan YouTube Data API v3 dan YouTube Analytics API (langkah 2).".into()
            }
            403 => format!("Google menolak akses: {msg}"),
            _ => format!("Google gagal (kode {code}): {msg}"),
        }
    }

    fn post_form(url: &str, form: &[(&str, &str)]) -> Result<Value, String> {
        match agent().post(url).send_form(form) {
            Ok(r) => r.into_json().map_err(|e| friendly(0, &e.to_string())),
            Err(ureq::Error::Status(code, r)) => Err(friendly(code, &r.into_string().unwrap_or_default())),
            Err(e) => Err(friendly(0, &e.to_string())),
        }
    }

    fn get(token: &str, url: &str) -> Result<Value, (u16, String)> {
        match agent().get(url).set("Authorization", &format!("Bearer {token}")).call() {
            Ok(r) => r.into_json().map_err(|e| (0, friendly(0, &e.to_string()))),
            Err(ureq::Error::Status(code, r)) => Err((code, friendly(code, &r.into_string().unwrap_or_default()))),
            Err(e) => Err((0, friendly(0, &e.to_string()))),
        }
    }

    pub fn auth_url(c: &Config, port: u16) -> Result<String, String> {
        if c.yt_client_id.is_empty() || c.yt_secret.is_empty() {
            return Err("Isi dan simpan dulu Client ID dan Client secret dari Google Cloud.".into());
        }
        let state = random_hex();
        *STATE.lock().unwrap() = state.clone();
        Ok(format!(
            "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent&state={state}",
            enc(&c.yt_client_id),
            enc(&redirect(port)),
            enc(SCOPES)
        ))
    }

    /// Exchanges the one-time code for a long-lived refresh token.
    pub fn finish(c: &Config, port: u16, code: &str, state: &str) -> Result<String, String> {
        {
            let mut s = STATE.lock().unwrap();
            if s.is_empty() || *s != state {
                return Err("Sesi login tidak cocok. Klik Hubungkan YouTube lagi dari aplikasi.".into());
            }
            s.clear();
        }
        let r = redirect(port);
        let v = post_form(
            "https://oauth2.googleapis.com/token",
            &[("code", code), ("client_id", &c.yt_client_id), ("client_secret", &c.yt_secret), ("redirect_uri", &r), ("grant_type", "authorization_code")],
        )?;
        v["refresh_token"].as_str().map(String::from).ok_or_else(|| "Google tidak memberi izin jangka panjang. Klik Hubungkan YouTube lagi.".into())
    }

    fn access_token(c: &Config) -> Result<String, String> {
        if c.yt_refresh.is_empty() {
            return Err("YouTube belum terhubung. Klik Hubungkan YouTube dulu.".into());
        }
        let v = post_form(
            "https://oauth2.googleapis.com/token",
            &[("refresh_token", &c.yt_refresh), ("client_id", &c.yt_client_id), ("client_secret", &c.yt_secret), ("grant_type", "refresh_token")],
        )?;
        v["access_token"].as_str().map(String::from).ok_or_else(|| "Google tidak memberi token akses.".into())
    }

    /// Days since 1970-01-01 -> "YYYY-MM-DD" (proleptic Gregorian).
    pub fn ymd(days: i64) -> String {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + i64::from(m <= 2);
        format!("{y:04}-{m:02}-{d:02}")
    }

    /// ISO 8601 duration "PT1H2M3S" -> seconds.
    pub fn iso_secs(s: &str) -> u64 {
        let (mut total, mut n) = (0u64, 0u64);
        for ch in s.chars() {
            match ch {
                '0'..='9' => n = n * 10 + ch.to_digit(10).unwrap() as u64,
                'H' => { total += n * 3600; n = 0 }
                'M' => { total += n * 60; n = 0 }
                'S' => { total += n; n = 0 }
                'D' => { total += n * 86_400; n = 0 }
                _ => n = 0,
            }
        }
        total
    }

    const YTA: &str = "https://youtubeanalytics.googleapis.com/v2/reports?ids=channel%3D%3DMINE&currency=IDR";

    /// Runs a report with revenue; if the channel or token has no revenue access, runs it again without.
    fn report(token: &str, q: &str, with_rev: &str, without_rev: &str) -> Result<(Value, bool), String> {
        match get(token, &format!("{YTA}&{q}&metrics={}", enc(with_rev))) {
            Ok(v) => Ok((v, true)),
            Err((403, _)) => get(token, &format!("{YTA}&{q}&metrics={}", enc(without_rev))).map(|v| (v, false)).map_err(|e| e.1),
            Err(e) => Err(e.1),
        }
    }

    fn col(v: &Value, name: &str) -> Option<usize> {
        v["columnHeaders"].as_array()?.iter().position(|h| h["name"] == name)
    }

    /// Pulls channel, daily views, totals and per-video stats into data.json.
    pub fn sync(c: &Config, dir: &Path) -> Result<(String, Value), String> {
        let token = access_token(c)?;
        let ch = get(&token, "https://www.googleapis.com/youtube/v3/channels?part=snippet,statistics&mine=true").map_err(|e| e.1)?;
        let ch = ch["items"].get(0).cloned().ok_or("Akun Google ini tidak punya channel YouTube. Login dengan akun pemilik channel.")?;
        let channel = ch["snippet"]["title"].as_str().unwrap_or("").to_string();
        let start = ch["snippet"]["publishedAt"].as_str().and_then(|s| s.get(..10)).unwrap_or("2015-01-01").to_string();
        let end = ymd((now() / 86_400) as i64 - 1);
        let range = format!("startDate={start}&endDate={end}");

        let daily = get(&token, &format!("{YTA}&{range}&metrics=views&dimensions=day&sort=day")).map_err(|e| e.1)?;
        let daily: Vec<Value> = daily["rows"].as_array().into_iter().flatten().map(|r| json!([r[0], r[1].as_f64().unwrap_or(0.0) as i64])).collect();

        let (tot, has_rev) = report(
            &token,
            &range,
            "views,estimatedMinutesWatched,subscribersGained,subscribersLost,estimatedRevenue",
            "views,estimatedMinutesWatched,subscribersGained,subscribersLost",
        )?;
        let t = tot["rows"].get(0).cloned().unwrap_or(json!([0, 0, 0, 0, 0]));
        let f = |i: usize| t.get(i).and_then(Value::as_f64).unwrap_or(0.0);

        let (vids, _) = report(
            &token,
            &format!("{range}&dimensions=video&sort=-views&maxResults=200"),
            "views,estimatedMinutesWatched,subscribersGained,estimatedRevenue",
            "views,estimatedMinutesWatched,subscribersGained",
        )?;
        let rows: Vec<Value> = vids["rows"].as_array().cloned().unwrap_or_default();
        let ids: Vec<String> = rows.iter().filter_map(|r| r[0].as_str().map(String::from)).collect();
        let mut meta: std::collections::HashMap<String, (String, String, u64)> = Default::default();
        for batch in ids.chunks(50) {
            let v = get(&token, &format!("https://www.googleapis.com/youtube/v3/videos?part=snippet,contentDetails&id={}", batch.join(","))).map_err(|e| e.1)?;
            for it in v["items"].as_array().into_iter().flatten() {
                meta.insert(
                    it["id"].as_str().unwrap_or("").into(),
                    (
                        it["snippet"]["title"].as_str().unwrap_or("").into(),
                        it["snippet"]["publishedAt"].as_str().and_then(|s| s.get(..10)).unwrap_or("").into(),
                        iso_secs(it["contentDetails"]["duration"].as_str().unwrap_or("")),
                    ),
                );
            }
        }

        let _guard = DATA_JSON_LOCK.lock().unwrap();
        let old = load_data(dir);
        // The Analytics API has no impressions or CTR; keep them from the last Studio export.
        let old_by_title: std::collections::HashMap<String, Value> = old["videos"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v["t"].as_str().map(|t| (t.to_string(), v.clone())))
            .collect();
        let (iv, im, is, ir) = (col(&vids, "views"), col(&vids, "estimatedMinutesWatched"), col(&vids, "subscribersGained"), col(&vids, "estimatedRevenue"));
        let num_at = |r: &Value, i: Option<usize>| i.and_then(|i| r.get(i)).and_then(Value::as_f64).unwrap_or(0.0);
        let videos: Vec<Value> = rows
            .iter()
            .filter_map(|r| {
                let (title, date, dur) = meta.get(r[0].as_str()?)?.clone();
                let mut v = json!({
                    "t": title, "d": date, "dur": dur,
                    "v": num_at(r, iv) as i64,
                    "h": (num_at(r, im) / 60.0 * 10.0).round() / 10.0,
                    "s": num_at(r, is) as i64,
                });
                if ir.is_some() {
                    v["rev"] = json!(num_at(r, ir));
                }
                if let Some(o) = old_by_title.get(&title) {
                    for k in ["imp", "ctr"] {
                        if !o[k].is_null() {
                            v[k] = o[k].clone();
                        }
                    }
                }
                Some(v)
            })
            .collect();

        let mut totals = json!({
            "views": f(0) as i64,
            "hours": (f(1) / 60.0 * 10.0).round() / 10.0,
            "subs": (f(2) - f(3)) as i64,
        });
        if has_rev {
            totals["revenue"] = json!(f(4));
        }
        for k in ["imp", "ctr"] {
            if !old["totals"][k].is_null() {
                totals[k] = old["totals"][k].clone();
            }
        }
        let mut data = if old.is_object() { old } else { json!({}) };
        data["daily"] = json!(daily);
        data["videos"] = json!(videos);
        data["totals"] = totals;
        data["currency"] = json!("IDR");
        data["range"] = json!(format!("{start} sampai {end}"));
        data["imported_at"] = json!(now() * 1000);
        data["source"] = json!("youtube-api");
        let bytes = serde_json::to_vec(&data).map_err(|e| e.to_string())?;
        write_atomic(&dir.join("data.json"), &bytes).map_err(|e| format!("Gagal menyimpan data: {e}"))?;
        let summary = json!({"channel": channel, "videos": videos.len(), "days": daily.len(), "revenue": has_rev, "range": data["range"]});
        Ok((channel, summary))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn dates_and_durations() {
            assert_eq!(ymd(0), "1970-01-01");
            assert_eq!(ymd(20_765), "2026-11-08");
            assert_eq!(ymd(11_016), "2000-02-29");
            assert_eq!(iso_secs("PT8M36S"), 516);
            assert_eq!(iso_secs("PT1H0M5S"), 3605);
            assert_eq!(iso_secs("PT59S"), 59);
        }
        #[test]
        fn url_encoding() {
            assert_eq!(enc("http://127.0.0.1:47815/oauth/callback"), "http%3A%2F%2F127.0.0.1%3A47815%2Foauth%2Fcallback");
        }
    }
}

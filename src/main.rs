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

mod clip;

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
    /// KIE.ai: one prepaid key for Claude text, images and video clips.
    #[serde(skip_serializing_if = "String::is_empty")]
    kie_key: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    kie_key_dpapi: String,
    kie_model: String,
    /// "gemini" (default) or "kie": who makes thumbnail images.
    image_provider: String,
    kie_image_model: String,
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
    /// Google ended the permission (every 7 days while the Google app is in Testing).
    yt_expired: bool,
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
            kie_key: String::new(),
            kie_key_dpapi: String::new(),
            kie_model: String::new(),
            image_provider: String::new(),
            kie_image_model: String::new(),
            yt_client_id: String::new(),
            yt_secret: String::new(),
            yt_secret_dpapi: String::new(),
            yt_refresh: String::new(),
            yt_refresh_dpapi: String::new(),
            yt_channel: String::new(),
            yt_synced: 0,
            yt_expired: false,
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
        || !c.kie_key.is_empty() || !c.yt_secret.is_empty() || !c.yt_refresh.is_empty();
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
    if !c.kie_key_dpapi.is_empty() {
        c.kie_key = open(&c.kie_key_dpapi);
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
    disk.kie_key_dpapi.clear();
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
    if !c.kie_key.is_empty() {
        if let Some(enc) = secret::protect(c.kie_key.as_bytes()) {
            disk.kie_key_dpapi = to_hex(&enc);
            disk.kie_key.clear();
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
        show_error("Terra Studio could not start: ports 47815 to 47818 are in use by another program.");
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
        let dark = skin == "batu";
        let (bg, ink) = if dark { ((0x08, 0x07, 0x06), (0xF4, 0xEC, 0xDD)) } else { ((0xED, 0xED, 0xEB), (0x14, 0x14, 0x14)) };
        let colorref = |(r, g, b): (u32, u32, u32)| r | (g << 8) | (b << 16);
        let set = |attr: u32, v: u32| unsafe {
            DwmSetWindowAttribute(hwnd, attr, &v as *const u32 as *const std::ffi::c_void, 4);
        };
        set(20, dark as u32); // DWMWA_USE_IMMERSIVE_DARK_MODE
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
            tint_title("terang");
        }
        let _ = std::fs::create_dir_all(&data_dir);
        let mut ctx = WebContext::new(Some(data_dir));
        let url = format!("http://127.0.0.1:{port}/");
        let webview = WebViewBuilder::with_web_context(&mut ctx)
            .with_url(&url)
            .with_background_color((237, 237, 235, 255))
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
        return Err("The file is too large.".into());
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
                    "has_kie": !c.kie_key.is_empty(),
                    "kie_hint": c.kie_key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect::<String>(),
                    "kie_model": kie::text_model(&c),
                    "image_provider": kie::image_provider(&c),
                    "kie_image_model": kie::image_model(&c),
                    "pack_dir": pack::root(dir).display().to_string(),
                    "yt_client_id": c.yt_client_id,
                    "has_yt_secret": !c.yt_secret.is_empty(),
                    "yt_connected": !c.yt_refresh.is_empty(),
                    "yt_channel": c.yt_channel,
                    "yt_synced": c.yt_synced,
                    "yt_expired": c.yt_expired && c.yt_refresh.is_empty(),
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
                c.yt_expired = false;
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
            if v["clear_kie"].as_bool() == Some(true) {
                c.kie_key.clear();
            }
            if let Some(k) = v["kie_key"].as_str().map(str::trim).filter(|k| !k.is_empty() && k.len() < 200) {
                c.kie_key = k.to_string();
            }
            if let Some(m) = v["kie_model"].as_str().filter(|m| m.is_empty() || gemini::safe_model(m)) {
                c.kie_model = m.to_string();
            }
            if let Some(m) = v["kie_image_model"].as_str().filter(|m| m.is_empty() || gemini::safe_model(m)) {
                c.kie_image_model = m.to_string();
            }
            if let Some(p) = v["image_provider"].as_str().filter(|p| ["gemini", "kie"].contains(p)) {
                c.image_provider = p.to_string();
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
            if let Some(p) = v["provider"].as_str().filter(|p| ["", "manual", "claude", "gemini", "kie"].contains(p)) {
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
                Err(e) => respond_json(req, 500, json!({"error": format!("Could not save settings: {e}")})),
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
                return respond_json(req, 400, json!({"error": "The Gemini API key is empty."}));
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
                return respond_json(req, 400, json!({"error": "The Gemini key for images is empty."}));
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
            let via_kie = kie::image_provider(&c) == "kie";
            if via_kie && c.kie_key.is_empty() {
                return respond_json(req, 400, json!({"error": "The KIE.ai key is not set. Open Settings, section KIE.ai."}));
            }
            if !via_kie && c.image_key.is_empty() {
                return respond_json(req, 400, json!({"error": "The Gemini key for images is not set. Open Settings, section Thumbnail images."}));
            }
            let prompt = v["prompt"].as_str().unwrap_or("").trim().to_string();
            if prompt.is_empty() || prompt.len() > 6000 {
                return respond_json(req, 400, json!({"error": "The image prompt is empty or too long."}));
            }
            let slot = v["slot"].as_u64().unwrap_or(0).min(9);
            let stem = match v["id"].as_str() {
                Some("test") => "test".to_string(),
                Some(id) => match thumb_stem(id) {
                    Some(s) => s,
                    None => return respond_json(req, 400, json!({"error": "invalid id"})),
                },
                None => "free".to_string(),
            };
            let model = if via_kie { kie::image_model(&c) } else { gemini::image_model(&c) };
            let made = if via_kie { kie::image(&c.kie_key, &model, &prompt) } else { gemini::image(&c.image_key, &model, &prompt) };
            match made {
                Ok((bytes, ext)) => {
                    let gdir = dir.join("gen");
                    let _ = fs::create_dir_all(&gdir);
                    for old in ["png", "jpg", "webp"] {
                        let _ = fs::remove_file(gdir.join(format!("{stem}-{slot}.{old}")));
                    }
                    let name = format!("{stem}-{slot}.{ext}");
                    match fs::write(gdir.join(&name), &bytes) {
                        Ok(()) => respond_json(req, 200, json!({"file": name, "v": now(), "model": model, "provider": if via_kie { "kie" } else { "gemini" }})),
                        Err(e) => respond_json(req, 500, json!({"error": format!("Could not save the image: {e}")})),
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
                return respond_json(req, 400, json!({"error": "Not a JPG image."}));
            }
            // With folder=<video folder> it goes into 06 Thumbnail; with pack=<slug> into the old flat edit pack folder.
            if let Some(f) = query_param(req.url(), "folder").filter(|f| !f.is_empty()) {
                let d = match video::ensure(dir, &f) {
                    Ok(d) => d,
                    Err((code, e)) => return respond_json(req, code, json!({"error": e})),
                };
                return match video::save_new(&d.join(video::SUBS[5]), &format!("{}.jpg", thumb_file_stem(&name)), &body) {
                    Ok(path) => respond_json(req, 200, json!({"path": path.display().to_string(), "folder": f,
                        "file": path.file_name().map(|n| n.to_string_lossy().into_owned())})),
                    Err(e) => respond_json(req, 500, json!({"error": format!("Could not save the JPG: {}", video::why(&e))})),
                };
            }
            let path = match query_param(req.url(), "pack") {
                Some(p) => {
                    let Some(slug) = pack::slug(&p) else {
                        return respond_json(req, 400, json!({"error": "The folder name is not valid."}));
                    };
                    let d = pack::root(dir).join(slug);
                    let _ = fs::create_dir_all(&d);
                    for old in ["png", "webp"] {
                        let _ = fs::remove_file(d.join(format!("thumbnail.{old}")));
                    }
                    d.join("thumbnail.jpg")
                }
                None => eleven::out_dir(dir).join(format!("{}.jpg", file_slug(&name, "thumbnail"))),
            };
            match fs::write(&path, &body) {
                Ok(()) => respond_json(req, 200, json!({"path": path.display().to_string()})),
                Err(e) => respond_json(req, 500, json!({"error": format!("Could not save the JPG: {e}")})),
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
                return respond_json(req, 400, json!({"error": "The ElevenLabs API key is empty."}));
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
            // The folder is checked before ElevenLabs is paid, not after.
            let target = match video::from_req(dir, &v) {
                Ok(t) => t,
                Err((code, e)) => return respond_json(req, code, json!({"error": e})),
            };
            let spoken = match eleven::speak(&c, &parts, &voice) {
                Ok(s) => s,
                Err(e) => return respond_json(req, 502, json!({"error": e})),
            };
            let saved = match target {
                Some((name, d)) => {
                    let _g = video::LOCK.lock().unwrap();
                    video::save_take(&d, &name, &spoken)
                }
                None => eleven::save_flat(dir, v["title"].as_str().unwrap_or("voice over"), &spoken).map_err(|e| (500, e)),
            };
            match saved {
                Ok(out) => respond_json(req, 200, out),
                Err((code, e)) => respond_json(req, code, json!({"error": e})),
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
                (_, _, Some(err)) => Err(format!("Google refused permission ({err}).")),
                (Some(code), Some(state), None) => {
                    let c = cfg.lock().unwrap().clone();
                    youtube::finish(&c, port, &code, &state).and_then(|refresh| {
                        let mut c = cfg.lock().unwrap();
                        c.yt_refresh = refresh;
                        c.yt_expired = false;
                        save_config(dir, &c)
                    })
                }
                _ => Err("Google's reply was incomplete. Try connecting again.".into()),
            };
            let (title, msg) = match &result {
                Ok(()) => ("Connected", "YouTube is now connected to Terra Studio. Close this tab, go back to the app and click Pull data now.".to_string()),
                Err(e) => ("Failed", e.clone()),
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
                Err(e) if e.starts_with(youtube::EXPIRED) => {
                    // The saved permission no longer works, so drop it; Connect again asks Google for a new one.
                    let mut c = cfg.lock().unwrap();
                    c.yt_refresh.clear();
                    c.yt_expired = true;
                    let _ = save_config(dir, &c);
                    respond_json(req, 400, json!({"error": e, "expired": true}))
                }
                Err(e) => respond_json(req, 400, json!({"error": e})),
            }
        }
        (Method::Post, "/api/open-folder") => {
            open_folder(&eleven::out_dir(dir));
            respond_json(req, 200, json!({"ok": true}))
        }
        (Method::Post, "/api/video/folder") => {
            let v = match read_json(&mut req, 64 * 1024) {
                Ok(v) => v,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            let root = pack::root(dir);
            let name = match v["name"].as_str().filter(|n| !n.is_empty()) {
                Some(n) => n.to_string(),
                None if v["title"].is_string() => {
                    let base = video::folder_name(v["date"].as_str().unwrap_or(""), v["title"].as_str().unwrap_or(""));
                    if v["new"] == true { video::unique_name(&base, |n| root.join(n).exists()) } else { base }
                }
                None => return respond_json(req, 400, json!({"error": "Send the video folder name, or a title and upload date."})),
            };
            let d = match video::dir(dir, &name) {
                Ok(d) => d,
                Err((code, e)) => return respond_json(req, code, json!({"error": e})),
            };
            if v["create"] != false {
                if let Err((code, e)) = video::ensure(dir, &name) {
                    return respond_json(req, code, json!({"error": e}));
                }
            }
            respond_json(req, 200, video::summary(&d, &name))
        }
        (Method::Post, "/api/video/open") => {
            let v = match read_json(&mut req, 64 * 1024) {
                Ok(v) => v,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            let d = match video::ensure(dir, v["name"].as_str().unwrap_or("")) {
                Ok(d) => d,
                Err((code, e)) => return respond_json(req, code, json!({"error": e})),
            };
            let target = match v["sub"].as_str().filter(|s| !s.is_empty()) {
                None => d,
                Some(s) if video::SUBS.contains(&s) => d.join(s),
                Some(s) if s == video::OLDER || s == format!("{}/{}", video::NARR, video::OLDER) => d.join(video::NARR).join(video::OLDER),
                Some(_) => return respond_json(req, 400, json!({"error": "That subfolder does not exist."})),
            };
            open_folder(&target);
            respond_json(req, 200, json!({"ok": true, "dir": target.display().to_string()}))
        }
        (Method::Post, "/api/video/takes") | (Method::Post, "/api/video/take") => {
            let v = match read_json(&mut req, 64 * 1024) {
                Ok(v) => v,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            let name = v["name"].as_str().unwrap_or("").to_string();
            let d = match video::dir(dir, &name) {
                Ok(d) => d,
                Err((code, e)) => return respond_json(req, code, json!({"error": e})),
            };
            let _g = video::LOCK.lock().unwrap();
            if path == "/api/video/take" {
                if let Err((code, e)) = video::use_take(&d, v["file"].as_str().unwrap_or("")) {
                    return respond_json(req, code, json!({"error": e}));
                }
            }
            respond_json(req, 200, video::takes(&d, &name))
        }
        (Method::Post, "/api/video/migrate") => {
            let v = match read_json(&mut req, 2 * 1024 * 1024) {
                Ok(v) => v,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            let Some(videos) = v["videos"].as_array().filter(|a| a.len() <= 1000) else {
                return respond_json(req, 400, json!({"error": "Send the list of videos (at most 1000)."}));
            };
            let root = pack::root(dir);
            let _g = video::LOCK.lock().unwrap();
            let (moves, errors) = video::plan(&root, &eleven::out_dir(dir), videos);
            let mut out = if v["dry"] == false {
                video::run(&root, &moves)
            } else {
                json!({"dry": true, "count": moves.len(), "moves": moves.iter().map(|m| m.json(&root)).collect::<Vec<_>>()})
            };
            out["errors"] = json!(errors);
            respond_json(req, 200, out)
        }
        (Method::Post, "/api/edit/scan") | (Method::Post, "/api/edit/premiere") => {
            let v = match read_json(&mut req, 8 * 1024 * 1024) {
                Ok(v) => v,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            let root = pack::root(dir);
            let out = if path == "/api/edit/scan" { edit::scan(&root, &v) } else { edit::premiere(&root, &v) };
            match out {
                Ok(out) => respond_json(req, 200, out),
                Err((code, e)) => respond_json(req, code, json!({"error": e})),
            }
        }
        (Method::Post, "/api/edit/library") => {
            let lib = edit::library(&pack::root(dir));
            if let Err(e) = fs::create_dir_all(&lib) {
                return respond_json(req, 500, json!({"error": format!("Could not make the Sound library folder: {}", video::why(&e))}));
            }
            open_folder(&lib);
            respond_json(req, 200, json!({"ok": true, "dir": lib.display().to_string()}))
        }
        (Method::Get, "/api/clip/status") => respond_json(req, 200, clip::status()),
        (Method::Post, "/api/clip/setup") => {
            clip::start_setup();
            respond_json(req, 200, clip::status())
        }
        (Method::Post, "/api/clip/render") => {
            let v = match read_json(&mut req, 256 * 1024) {
                Ok(v) => v,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            match clip::start(dir, &v) {
                Ok(id) => respond_json(req, 200, json!({"job": id})),
                Err((code, e)) => respond_json(req, code, json!({"error": e})),
            }
        }
        (Method::Post, "/api/tools/run") => {
            let v = match read_json(&mut req, 256 * 1024) {
                Ok(v) => v,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            match clip::tool(dir, &v) {
                Ok(id) => respond_json(req, 200, json!({"job": id})),
                Err((code, e)) => respond_json(req, code, json!({"error": e})),
            }
        }
        (Method::Get, p) if p.starts_with("/api/tools/thumb/") => match clip::thumb(&p["/api/tools/thumb/".len()..]) {
            Some(b) => respond(req, 200, "image/png", b),
            None => respond_json(req, 404, json!({"error": "No picture."})),
        },
        (Method::Get, p) if p.starts_with("/api/clip/job/") => match p["/api/clip/job/".len()..].parse().ok().and_then(clip::job) {
            Some(j) => respond_json(req, 200, j),
            None => respond_json(req, 404, json!({"error": "This clip job is gone. Make it again."})),
        },
        (Method::Get, p) if p.starts_with("/api/clip/still/") => match p["/api/clip/still/".len()..].parse().ok().and_then(clip::still) {
            Some(b) => respond(req, 200, "image/png", b),
            None => respond_json(req, 404, json!({"error": "This preview is gone."})),
        },
        (Method::Post, "/api/kie/test") => {
            let v = match read_json(&mut req, 64 * 1024) {
                Ok(v) => v,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            let key = v["key"].as_str().map(str::trim).filter(|k| !k.is_empty() && k.len() < 200).map(String::from)
                .unwrap_or_else(|| cfg.lock().unwrap().kie_key.clone());
            if key.is_empty() {
                return respond_json(req, 400, json!({"error": "The KIE.ai key is empty."}));
            }
            // Reading the balance is free and proves the key works.
            match kie::credits(&key) {
                Ok(n) => respond_json(req, 200, json!({"ok": true, "credits": n, "usd": kie::usd(n)})),
                Err(e) => respond_json(req, 502, json!({"ok": false, "error": e})),
            }
        }
        (Method::Get, "/api/kie/credits") => {
            let key = cfg.lock().unwrap().kie_key.clone();
            if key.is_empty() {
                return respond_json(req, 400, json!({"error": "The KIE.ai key is empty."}));
            }
            match kie::credits(&key) {
                Ok(n) => respond_json(req, 200, json!({"credits": n, "usd": kie::usd(n)})),
                Err(e) => respond_json(req, 502, json!({"error": e})),
            }
        }
        (Method::Post, "/api/kie/clip") => {
            let v = match read_json(&mut req, 64 * 1024) {
                Ok(v) => v,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            let c = cfg.lock().unwrap().clone();
            match pack::clip(&c, dir, &v) {
                Ok(out) => respond_json(req, 200, out),
                Err((code, e)) => respond_json(req, code, json!({"error": e})),
            }
        }
        (Method::Post, "/api/pack/voice") => {
            let v = match read_json(&mut req, 512 * 1024) {
                Ok(v) => v,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            let c = cfg.lock().unwrap().clone();
            match pack::voice(&c, dir, &v) {
                Ok(out) => respond_json(req, 200, out),
                Err((code, e)) => respond_json(req, code, json!({"error": e})),
            }
        }
        (Method::Post, "/api/pack/finish") => {
            let v = match read_json(&mut req, 4 * 1024 * 1024) {
                Ok(v) => v,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            match pack::finish(dir, &v) {
                Ok(out) => respond_json(req, 200, out),
                Err((code, e)) => respond_json(req, code, json!({"error": e})),
            }
        }
        (Method::Post, "/api/pack/open") => {
            let v = match read_json(&mut req, 64 * 1024) {
                Ok(v) => v,
                Err(e) => return respond_json(req, 400, json!({"error": e})),
            };
            let d = match v["folder"].as_str().filter(|f| !f.is_empty()) {
                Some(f) => match video::dir(dir, f) {
                    Ok(d) => d,
                    Err((code, e)) => return respond_json(req, code, json!({"error": e})),
                },
                None => {
                    let Some(slug) = pack::slug(v["slug"].as_str().unwrap_or("")) else {
                        return respond_json(req, 400, json!({"error": "The folder name is not valid."}));
                    };
                    pack::root(dir).join(&slug)
                }
            };
            if !d.is_dir() {
                return respond_json(req, 404, json!({"error": "This edit pack folder does not exist yet."}));
            }
            open_folder(&d);
            respond_json(req, 200, json!({"ok": true, "dir": d.display().to_string()}))
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
                return respond_json(req, 400, json!({"error": "This file is not a Terra Studio backup."}));
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
                Err(e) => respond_json(req, 500, json!({"error": format!("Could not restore the backup: {e}")})),
            }
        }
        // Last AI result per tool, so reopening the app does not cost a new call.
        (Method::Get, "/api/last") => {
            // Ideas that Claude Desktop sent (Terra Studio for Claude) are taken in here, once.
            let _guard = LAST_JSON_LOCK.lock().unwrap();
            let path = dir.join("last.json");
            let mut all = load_json(&path);
            if take_claude_inbox(dir, &mut all) > 0 {
                let _ = write_atomic(&path, all.to_string().as_bytes());
            }
            respond_json(req, 200, all)
        }
        // Opens TerraStudio.mcpb (next to the exe) so Claude Desktop offers to install it.
        (Method::Post, "/api/claude/install") => {
            let Some(f) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("TerraStudio.mcpb"))).filter(|f| f.exists()) else {
                return respond_json(req, 404, json!({"error": "TerraStudio.mcpb is missing next to TerraStudio.exe. Run the 2.9 installer again."}));
            };
            open_folder(&f);
            respond_json(req, 200, json!({"ok": true, "file": f.display().to_string()}))
        }
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
                return respond_json(req, 400, json!({"error": "invalid id"}));
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
                _ => return respond_json(req, 400, json!({"error": "Choose a JPG, PNG or WebP image."})),
            };
            let _ = fs::create_dir_all(&tdir);
            match fs::write(tdir.join(format!("{stem}.{ext}")), &body) {
                Ok(()) => respond_json(req, 200, json!({"ok": true})),
                Err(e) => respond_json(req, 500, json!({"error": format!("Could not save the image: {e}")})),
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
        // The images in a video's 06 Thumbnail: the list, newest first, and each file. Read only.
        (Method::Get, p) if p.starts_with("/api/vthumbs/") => {
            let name = percent_decode(&p["/api/vthumbs/".len()..]);
            match video::dir(dir, &name) {
                Ok(d) => respond_json(req, 200, video::thumbs(&d.join(video::SUBS[5]), &name)),
                Err((code, e)) => respond_json(req, code, json!({"error": e})),
            }
        }
        (Method::Get, p) if p.starts_with("/api/vthumb/") => {
            let found = vthumb_in(&eleven::out_dir(dir), &p["/api/vthumb/".len()..])
                .filter(|(f, _)| fs::symlink_metadata(f).is_ok_and(|m| m.is_file()))
                .and_then(|(f, ct)| fs::read(f).ok().map(|b| (b, ct)));
            match found {
                Some((bytes, ct)) => respond(req, 200, ct, bytes),
                None => respond_json(req, 404, json!({"error": "not found"})),
            }
        }
        (Method::Get, p) if p.starts_with("/api/audio/") => {
            match audio_path(dir, &p["/api/audio/".len()..]).and_then(|f| fs::read(f).ok()) {
                Some(bytes) => respond(req, 200, "audio/mpeg", bytes),
                None => respond_json(req, 404, json!({"error": "not found"})),
            }
        }
        _ => respond_json(req, 404, json!({"error": "not found"})),
    }
}

/// "/api/audio/<file>" is an MP3 loose in Downloads (before 2.5); "/api/audio/<video folder>/<file>"
/// is a take or act in that video's 02 Narration or its "older takes".
fn audio_path(app: &Path, rest: &str) -> Option<PathBuf> {
    audio_in(&eleven::out_dir(app), rest)
}

fn audio_in(downloads: &Path, rest: &str) -> Option<PathBuf> {
    let parts: Vec<String> = rest.split('/').map(percent_decode).collect();
    let mp3 = |f: &str| video::valid_file(f) && f.to_ascii_lowercase().ends_with(".mp3");
    match parts.as_slice() {
        [f] if mp3(f) => Some(downloads.join(f)),
        [folder, f] if mp3(f) && video::valid_name(folder) => {
            let narr = pack::root_in(downloads).join(folder).join(video::NARR);
            [narr.join(f), narr.join(video::OLDER).join(f)].into_iter().find(|p| p.is_file())
        }
        _ => None,
    }
}

/// "<video folder>/<file>": an image in that video's 06 Thumbnail, with its content type.
fn vthumb_in(downloads: &Path, rest: &str) -> Option<(PathBuf, &'static str)> {
    let parts: Vec<String> = rest.split('/').map(percent_decode).collect();
    match parts.as_slice() {
        [folder, f] if video::valid_name(folder) && video::valid_file(f) => {
            let ct = video::image_type(f)?;
            Some((pack::root_in(downloads).join(folder).join(video::SUBS[5]).join(f), ct))
        }
        _ => None,
    }
}

/// Moves ideas from claude-inbox.json (written by TerraMCP) to the front of the saved Ideas list.
/// Returns how many were added. The inbox file is removed once read.
fn take_claude_inbox(dir: &Path, all: &mut Value) -> usize {
    let inbox = dir.join("claude-inbox.json");
    let taking = dir.join("claude-inbox.taking");
    if fs::rename(&inbox, &taking).is_err() {
        return 0;
    }
    let got = load_json(&taking);
    let _ = fs::remove_file(&taking);
    let new: Vec<Value> = got["ideas"].as_array().cloned().unwrap_or_default();
    if !all.is_object() {
        *all = json!({});
    }
    let old = all["ideas"]["data"].clone();
    let mut list: Vec<Value> = old["ideas"].as_array().cloned().unwrap_or_default();
    let mut want = old["want"].as_u64().unwrap_or(list.len() as u64);
    let mut n = 0;
    for x in new.into_iter().rev() {
        let t = x["title"].as_str().unwrap_or("").trim().to_lowercase();
        if t.is_empty() || list.iter().any(|y| y["title"].as_str().unwrap_or("").trim().to_lowercase() == t) {
            continue;
        }
        list.insert(0, x);
        want += 1;
        n += 1;
    }
    if n > 0 {
        all["ideas"] = json!({"at": now() * 1000, "data": {"ideas": list, "want": want}});
    }
    n
}

fn open_folder(d: &Path) {
    #[cfg(windows)]
    let _ = Command::new("explorer.exe").arg(d).spawn();
    #[cfg(not(windows))]
    let _ = Command::new("xdg-open").arg(d).spawn();
}

/// Plan ids become file names, so only short alphanumeric ids are accepted.
fn thumb_stem(id: &str) -> Option<String> {
    (!id.is_empty() && id.len() <= 40 && id.chars().all(|c| c.is_ascii_alphanumeric())).then(|| id.to_string())
}

/// "<title> thumbnail 2" -> "<title slug>-thumbnail-2": the title is cut, never the number.
fn thumb_file_stem(name: &str) -> String {
    let name = name.trim();
    let (head, n) = match name.rsplit_once(' ') {
        Some((h, d)) if !d.is_empty() && d.len() <= 3 && d.bytes().all(|b| b.is_ascii_digit()) => (h.trim_end(), Some(d)),
        _ => (name, None),
    };
    let head = head.strip_suffix("thumbnail").unwrap_or(head);
    let title = file_slug(head, "video");
    format!("{title}-thumbnail{}", n.map(|d| format!("-{d}")).unwrap_or_default())
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
        "kie" if !c.kie_key.is_empty() => "kie",
        _ if !c.api_key.is_empty() => "claude",
        _ if !c.gemini_key.is_empty() => "gemini",
        _ if !c.kie_key.is_empty() => "kie",
        _ => "",
    }
}

fn generate(c: &Config, prompt: &str, test: bool) -> (u16, Value) {
    if active_provider(c) == "gemini" {
        return gemini::generate(c, prompt, test);
    }
    if active_provider(c) == "kie" {
        return kie::generate(c, prompt, test);
    }
    if c.api_key.is_empty() {
        return (400, json!({"error": "The API key is empty. Open Settings and paste your Anthropic API key."}));
    }
    if prompt.trim().is_empty() {
        return (400, json!({"error": "The request is empty."}));
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
                return (422, json!({"error": "Claude declined this request. Change the topic or the wording, then try again.", "model": resp["model"], "usage": usage(&resp)}));
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
                    "The answer was cut off before it finished. Try again, or lower Thinking depth in Settings."
                } else {
                    "Claude sent no answer. Try again."
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
                0 => format!("Could not reach api.anthropic.com. Check your internet connection. ({api_msg})"),
                401 => "The API key was rejected. Check the key in Settings.".to_string(),
                403 => format!("Anthropic denied access. Original message: {api_msg}"),
                _ if lower.contains("credit") || lower.contains("billing") => {
                    "The API credit has run out. Add credit at console.anthropic.com, under Billing.".to_string()
                }
                429 => "Usage limit reached. Wait a moment, then try again.".to_string(),
                500..=599 => "Claude's servers are busy. Try again in a little while.".to_string(),
                _ => format!("The request failed (code {code}). Original message: {api_msg}"),
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
            0 => format!("Could not reach Google Gemini. Check your internet connection. ({msg})"),
            400 if lower.contains("api key") || lower.contains("api_key") => {
                "The Gemini API key was rejected. Check the key in Settings.".into()
            }
            401 | 403 => format!("Gemini denied access. Original message: {msg}"),
            404 => "That Gemini model is not available. Choose another model in Settings.".into(),
            429 => "The free Gemini quota is used up. Wait a while (or until tomorrow), then try again.".into(),
            500..=599 => "Gemini's servers are busy. Try again in a little while.".into(),
            _ => format!("The Gemini request failed (code {code}). Original message: {msg}"),
        }
    }

    /// Text models the key can use, newest first.
    pub fn models(key: &str) -> Result<Vec<String>, (u16, String)> {
        let resp = agent()
            .get(&format!("{BASE}/models?pageSize=200"))
            .set("x-goog-api-key", key)
            .call();
        let v: Value = match resp {
            Ok(r) => r.into_json().map_err(|_| (502, "KIE.ai sent a reply this app could not read. Try again.".to_string()))?,
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
            402 => "The Gemini prepaid credit has run out. Top up in Google AI Studio, under Billing, then try again.".into(),
            429 if lower.contains("free_tier") || lower.contains("free tier") || lower.contains("limit: 0") => {
                "This key is on the free tier, and images are only available on the paid tier. In Google AI Studio click Set up billing for this key's project.".into()
            }
            429 => format!("Gemini usage limit reached. Wait a moment, then try again. Original message: {msg}"),
            403 if lower.contains("billing") => {
                "Billing is not active for this key. In Google AI Studio click Set up billing, then try again.".into()
            }
            404 => "That image model is not available for this key. Choose another model in Settings.".into(),
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
                    "Gemini declined to make this image. Click Redo, or change the concept.".to_string()
                } else {
                    "Gemini sent no image. Click Redo.".to_string()
                }
            });
        }
        Err(first_err.unwrap_or_else(|| "Gemini sent no image. Try again.".to_string()))
    }

    pub fn generate(c: &Config, prompt: &str, test: bool) -> (u16, Value) {
        if prompt.trim().is_empty() {
            return (400, json!({"error": "The request is empty."}));
        }
        let model = if c.gemini_model.is_empty() {
            match models(&c.gemini_key) {
                Ok(list) => match pick_default(&list) {
                    Some(m) => m,
                    None => return (502, json!({"error": "No Gemini model can be used with this key."})),
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
                "MAX_TOKENS" => "Gemini's answer was cut off before it finished. Try again.".to_string(),
                "SAFETY" | "PROHIBITED_CONTENT" | "BLOCKLIST" | "RECITATION" => {
                    "Gemini declined this request. Change the topic or the wording, then try again.".to_string()
                }
                _ if v["promptFeedback"]["blockReason"].is_string() => {
                    "Gemini declined this request. Change the topic or the wording, then try again.".to_string()
                }
                _ => "Gemini sent no answer. Try again.".to_string(),
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
            assert!(friendly_image(402, "{}").contains("prepaid credit"));
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
    /// Development builds can point voice calls at a local stand-in server.
    fn base() -> String {
        #[cfg(debug_assertions)]
        if let Ok(b) = std::env::var("TERRA_ELEVEN_BASE") {
            return b;
        }
        "https://api.elevenlabs.io".to_string()
    }
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
            0 => format!("Could not reach ElevenLabs. Check your internet connection. ({detail})"),
            // ElevenLabs reports a used-up quota with code 401, the same code as a bad key.
            _ if code == 402 || lower.contains("quota") || lower.contains("credits") => {
                "The ElevenLabs character quota is used up for this period.".into()
            }
            401 => "The ElevenLabs API key was rejected or lacks this permission. Check the key in Settings.".into(),
            404 => "That ElevenLabs voice was not found. Choose another voice in Settings.".into(),
            429 => "ElevenLabs is busy or the request limit was reached. Try again shortly.".into(),
            _ => format!("ElevenLabs failed (code {code}): {detail}"),
        }
    }

    fn get(key: &str, path: &str) -> Result<Value, String> {
        match agent().get(&format!("{}{path}", base())).set("xi-api-key", key).call() {
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

    /// First eight words of the title, as used in voice over file names since 2.0.
    pub fn title_slug(title: &str) -> String {
        title
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .take(8)
            .collect::<Vec<_>>()
            .join("-")
    }

    fn file_name(title: &str) -> String {
        let slug = title_slug(title);
        let slug = if slug.is_empty() { "voice-over".to_string() } else { slug };
        format!("{slug}-{}.mp3", now())
    }

    pub struct Spoken {
        pub audio: Vec<u8>,
        pub chars: usize,
        pub chunks: usize,
    }

    /// The old behaviour: one MP3 loose in Downloads.
    pub fn save_flat(dir: &Path, title: &str, s: &Spoken) -> Result<Value, String> {
        let name = file_name(title);
        let path = out_dir(dir).join(&name);
        fs::write(&path, &s.audio).map_err(|e| format!("Could not save the MP3: {e}"))?;
        Ok(json!({"file": name, "path": path.display().to_string(), "chars": s.chars, "chunks": s.chunks, "bytes": s.audio.len(),
            "seconds": mp3_seconds(&s.audio), "at": now(), "folder": null,
            "url": format!("/api/audio/{}", video::enc(&name))}))
    }

    pub fn speak(c: &Config, parts: &[String], voice: &str) -> Result<Spoken, String> {
        if c.eleven_key.is_empty() {
            return Err("The ElevenLabs API key is empty. Open Settings, section Voice.".into());
        }
        if voice.is_empty() {
            return Err("Choose an ElevenLabs voice in Settings first.".into());
        }
        let pieces = chunks(parts);
        if pieces.is_empty() {
            return Err("The script is empty.".into());
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
                .post(&format!("{}/v1/text-to-speech/{voice}?output_format=mp3_44100_128", base()))
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
                    return Err(if i == 0 { msg } else { format!("{msg} (stopped at part {} of {})", i + 1, pieces.len()) });
                }
                Err(e) => return Err(friendly(0, &e.to_string())),
            }
        }
        let chars: usize = pieces.iter().map(|p| p.chars().count()).sum();
        Ok(Spoken { audio, chars, chunks: pieces.len() })
    }

    /// Audio with the time of every character, for subtitles that match the voice.
    pub struct Timed {
        pub audio: Vec<u8>,
        pub chars: Vec<String>,
        pub starts: Vec<f64>,
        pub ends: Vec<f64>,
        pub seconds: f64,
        pub count: usize,
        pub chunks: usize,
    }

    /// Length of a constant bitrate MP3, read from its first frame header.
    pub fn mp3_seconds(b: &[u8]) -> Option<f64> {
        mp3_seconds_len(b, b.len())
    }

    /// Same, from the start of a file and its full size, so a long file need not be read whole.
    pub fn mp3_seconds_len(b: &[u8], total: usize) -> Option<f64> {
        const V1: [u32; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
        const V2: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
        let mut i = 0;
        if b.len() > 10 && &b[..3] == b"ID3" {
            i = 10 + b[6..10].iter().fold(0usize, |n, x| (n << 7) | (*x & 0x7F) as usize);
        }
        let stop = b.len().min(i + 65536);
        while i + 4 <= stop {
            if b[i] == 0xFF && b[i + 1] & 0xE0 == 0xE0 {
                let (version, layer, rate) = ((b[i + 1] >> 3) & 3, (b[i + 1] >> 1) & 3, (b[i + 2] >> 4) as usize);
                if layer == 1 && version != 1 && (1..15).contains(&rate) {
                    let kbps = if version == 3 { V1[rate] } else { V2[rate] };
                    return Some((total.max(b.len()) - i) as f64 * 8.0 / (kbps as f64 * 1000.0));
                }
            }
            i += 1;
        }
        None
    }

    /// Same voice, model and chunking as speak(), through the endpoint that also returns timing.
    /// Each chunk's times start at zero, so they are shifted by the audio that came before.
    pub fn speak_timed(c: &Config, text: &str) -> Result<Timed, String> {
        if c.eleven_key.is_empty() {
            return Err("The ElevenLabs API key is empty. Open Settings, section Voice.".into());
        }
        if c.eleven_voice.is_empty() {
            return Err("Choose an ElevenLabs voice in Settings first.".into());
        }
        let pieces = chunks(&[text.to_string()]);
        if pieces.is_empty() {
            return Err("The script is empty.".into());
        }
        let model = if c.eleven_model.is_empty() { DEFAULT_MODEL } else { c.eleven_model.as_str() };
        let mut out = Timed { audio: Vec::new(), chars: Vec::new(), starts: Vec::new(), ends: Vec::new(), seconds: 0.0, count: 0, chunks: pieces.len() };
        for (i, text) in pieces.iter().enumerate() {
            let mut body = json!({"text": text, "model_id": model});
            if i > 0 {
                body["previous_text"] = json!(pieces[i - 1].chars().rev().take(500).collect::<String>().chars().rev().collect::<String>());
            }
            if let Some(next) = pieces.get(i + 1) {
                body["next_text"] = json!(next.chars().take(500).collect::<String>());
            }
            let resp = agent()
                .post(&format!("{}/v1/text-to-speech/{}/with-timestamps?output_format=mp3_44100_128", base(), c.eleven_voice))
                .set("xi-api-key", &c.eleven_key)
                .set("content-type", "application/json")
                .send_json(body);
            let v: Value = match resp {
                Ok(r) => r.into_json().map_err(|e| friendly(0, &e.to_string()))?,
                Err(ureq::Error::Status(code, r)) => {
                    let msg = friendly(code, &r.into_string().unwrap_or_default());
                    return Err(if i == 0 { msg } else { format!("{msg} (stopped at part {} of {})", i + 1, pieces.len()) });
                }
                Err(e) => return Err(friendly(0, &e.to_string())),
            };
            let audio = v["audio_base64"].as_str().and_then(base64_decode).filter(|a| !a.is_empty())
                .ok_or_else(|| "ElevenLabs sent no audio. Try again.".to_string())?;
            let al = if v["alignment"].is_object() { &v["alignment"] } else { &v["normalized_alignment"] };
            let times = |key: &str| -> Vec<f64> { al[key].as_array().into_iter().flatten().map(|x| x.as_f64().unwrap_or(0.0)).collect() };
            let (starts, ends) = (times("character_start_times_seconds"), times("character_end_times_seconds"));
            let chars: Vec<String> = al["characters"].as_array().into_iter().flatten().map(|x| x.as_str().unwrap_or("").to_string()).collect();
            let n = chars.len().min(starts.len()).min(ends.len());
            let offset = out.seconds;
            if i > 0 {
                // A chunk always starts a new paragraph, so the subtitle breaks there too.
                out.chars.push("\n".to_string());
                out.starts.push(offset);
                out.ends.push(offset);
            }
            out.chars.extend(chars.into_iter().take(n));
            out.starts.extend(starts.iter().take(n).map(|t| t + offset));
            out.ends.extend(ends.iter().take(n).map(|t| t + offset));
            let spoken = ends.iter().take(n).cloned().fold(0.0, f64::max);
            out.seconds += mp3_seconds(&audio).unwrap_or(spoken).max(spoken);
            out.count += text.chars().count();
            out.audio.extend_from_slice(&audio);
        }
        Ok(out)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn quota_is_not_reported_as_a_bad_key() {
            assert!(friendly(401, r#"{"detail":{"status":"quota_exceeded","message":"This request exceeds your quota."}}"#).contains("quota is used up"));
            assert!(friendly(401, r#"{"detail":{"status":"invalid_api_key","message":"Invalid API key"}}"#).contains("rejected"));
        }
        #[test]
        fn mp3_length_from_first_frame() {
            // MPEG-1 Layer III, 128 kbps, 44.1 kHz: 16,000 bytes per second.
            let mut b = vec![0xFF, 0xFB, 0x90, 0x00];
            b.resize(32_000, 0);
            assert_eq!(mp3_seconds(&b), Some(2.0));
            let mut tagged = b"ID3\x04\x00\x00\x00\x00\x00\x0A".to_vec();
            tagged.extend_from_slice(&[0u8; 10]);
            tagged.extend_from_slice(&b);
            assert_eq!(mp3_seconds(&tagged), Some(2.0));
            assert_eq!(mp3_seconds(b"not audio"), None);
        }
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

// KIE.ai: Claude text, images and video clips through one prepaid key

mod kie {
    use super::*;
    pub const TEXT_DEFAULT: &str = "claude-sonnet-5-5";
    pub const IMAGE_DEFAULT: &str = "nano-banana-2";

    /// Development builds can point KIE calls at a local stand-in server.
    fn base() -> String {
        #[cfg(debug_assertions)]
        if let Ok(b) = std::env::var("TERRA_KIE_BASE") {
            return b;
        }
        "https://api.kie.ai".to_string()
    }

    fn agent() -> ureq::Agent {
        ureq::AgentBuilder::new()
            .try_proxy_from_env(true)
            .timeout_connect(Duration::from_secs(20))
            .timeout_read(Duration::from_secs(600))
            .build()
    }

    /// Short read timeout so a stalled poll cannot outlast the task time limit.
    fn poll_agent() -> ureq::Agent {
        ureq::AgentBuilder::new()
            .try_proxy_from_env(true)
            .timeout_connect(Duration::from_secs(20))
            .timeout_read(Duration::from_secs(30))
            .redirects(0)
            .build()
    }

    pub fn text_model(c: &Config) -> String {
        if c.kie_model.is_empty() { TEXT_DEFAULT.to_string() } else { c.kie_model.clone() }
    }

    pub fn image_model(c: &Config) -> String {
        if c.kie_image_model.is_empty() { IMAGE_DEFAULT.to_string() } else { c.kie_image_model.clone() }
    }

    pub fn image_provider(c: &Config) -> &'static str {
        if c.image_provider == "kie" { "kie" } else { "gemini" }
    }

    /// KIE's published rate: one credit is about half a US cent.
    pub fn usd(credits: f64) -> f64 {
        (credits * 0.5).round() / 100.0
    }

    pub fn friendly(code: u16, msg: &str) -> String {
        let msg: String = msg.chars().take(300).collect();
        let said = said(&msg);
        match code {
            0 => format!("Could not reach KIE.ai. Check your internet connection. ({msg})"),
            401 => format!("The KIE.ai key was rejected. Check the key in Settings.{said}"),
            402 => format!("The KIE.ai credit has run out. Top up at kie.ai, then try again.{said}"),
            404 => format!("KIE.ai does not know this model. Check the model name in Settings.{said}"),
            408 => format!("KIE.ai took too long to answer. Try again.{said}"),
            422 => format!("KIE.ai did not accept the request. Original message: {msg}"),
            429 => format!("Too many requests to KIE.ai. Wait a moment, then try again.{said}"),
            455 => format!("KIE.ai is under maintenance. Try again later.{said}"),
            501 => format!("KIE.ai could not make this. Try again, or change the wording. Original message: {msg}"),
            502 if msg.starts_with("KIE.ai sent a reply") => msg,
            500..=599 => format!("KIE.ai's servers are busy. Try again in a little while.{said}"),
            _ => format!("The KIE.ai request failed (code {code}). Original message: {msg}"),
        }
    }

    /// KIE's own words, trimmed, for the end of a friendly sentence. Long tokens are cut out
    /// because some replies echo the key back.
    fn said(msg: &str) -> String {
        let t = msg.trim();
        if t.is_empty() || t.starts_with('<') {
            return String::new();
        }
        let clean: Vec<String> = t
            .split_whitespace()
            .map(|w| {
                let run = w.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').count();
                if run >= 20 { "[hidden]".to_string() } else { w.to_string() }
            })
            .collect();
        let mut s: String = clean.join(" ").chars().take(160).collect();
        if s.chars().count() == 160 {
            s.push('…');
        }
        format!(" KIE.ai said: \"{s}\"")
    }

    /// The error inside a reply body, if any. KIE answers HTTP 200 with its own
    /// "code" on many failures, and the Claude route can answer in Anthropic's error shape.
    fn body_error(v: &Value, http: u16) -> Option<(u16, String)> {
        let code = v["code"].as_u64().or_else(|| v["code"].as_str().and_then(|s| s.parse().ok()));
        if let Some(code) = code.filter(|c| *c != 200) {
            let msg = v["msg"].as_str().or(v["message"].as_str()).unwrap_or("");
            return Some((code.min(999) as u16, msg.to_string()));
        }
        if !v["error"].is_null() {
            let msg = v["error"]["message"].as_str().or(v["error"].as_str()).unwrap_or("");
            return Some((if http == 200 { 500 } else { http }, msg.to_string()));
        }
        None
    }

    fn send(req: ureq::Request, key: &str, body: Option<Value>) -> Result<Value, (u16, String)> {
        let req = req.set("Authorization", &format!("Bearer {key}"));
        let resp = match body {
            Some(b) => req.set("content-type", "application/json").send_json(b),
            None => req.call(),
        };
        match resp {
            Ok(r) => {
                let v: Value = r.into_json().map_err(|e| (0, e.to_string()))?;
                match body_error(&v, 200) {
                    Some(e) => Err(e),
                    None => Ok(v),
                }
            }
            Err(ureq::Error::Status(code, r)) => {
                let text = r.into_string().unwrap_or_default();
                let parsed = serde_json::from_str::<Value>(&text).ok().and_then(|v| body_error(&v, code));
                Err(parsed.unwrap_or((code, text)))
            }
            Err(e) => Err((0, e.to_string())),
        }
    }

    /// Remaining credits on the account. Free, so it doubles as the key test.
    pub fn credits(key: &str) -> Result<f64, String> {
        let v = send(agent().get(&format!("{}/api/v1/chat/credit", base())), key, None).map_err(|(c, m)| friendly(c, &m))?;
        v["data"].as_f64().ok_or_else(|| "KIE.ai sent no credit balance. Try again.".to_string())
    }

    pub fn generate(c: &Config, prompt: &str, test: bool) -> (u16, Value) {
        if c.kie_key.is_empty() {
            return (400, json!({"error": "The KIE.ai key is empty. Open Settings and paste your KIE.ai key."}));
        }
        if prompt.trim().is_empty() {
            return (400, json!({"error": "The request is empty."}));
        }
        let model = text_model(c);
        // KIE streams by default and stops at 4,096 tokens unless told otherwise.
        let body = json!({
            "model": model,
            "max_tokens": if test { 256 } else { 16000 },
            "stream": false,
            "messages": [{"role": "user", "content": [{"type": "text", "text": prompt}]}],
        });
        let resp = match send(agent().post(&format!("{}/claude/v1/messages", base())), &c.kie_key, Some(body)) {
            Ok(v) => v,
            Err((code, msg)) => return (502, json!({"error": friendly(code, &msg), "provider": "kie"})),
        };
        let stop = resp["stop_reason"].as_str().unwrap_or("");
        let text: String = resp["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect();
        let model = resp["model"].as_str().map(String::from).unwrap_or(model);
        let credits = resp["credits_consumed"].clone();
        if stop == "refusal" {
            return (422, json!({"error": "Claude declined this request. Change the topic or the wording, then try again.",
                "provider": "kie", "model": model, "usage": usage(&resp), "credits": credits}));
        }
        if text.trim().is_empty() {
            let why = if stop == "max_tokens" {
                "The answer was cut off before it finished. Try again."
            } else {
                "KIE.ai sent no answer. Try again."
            };
            return (502, json!({"error": why, "provider": "kie", "model": model, "usage": usage(&resp), "credits": credits}));
        }
        (200, json!({"text": text, "provider": "kie", "model": model, "stop_reason": stop, "usage": usage(&resp), "credits": credits}))
    }

    /// The first result URL of a finished task record.
    pub fn result_url(data: &Value) -> Option<String> {
        // resultJson is JSON inside a string; an object is accepted too in case KIE changes it.
        let parsed = match &data["resultJson"] {
            Value::String(s) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
            other => other.clone(),
        };
        for k in ["resultUrls", "originUrls", "videoUrls", "imageUrls"] {
            if let Some(u) = parsed[k][0].as_str().filter(|u| u.starts_with("http")) {
                return Some(u.to_string());
            }
        }
        // Some models put the file elsewhere in the record; any https file link will do.
        let mut found = None;
        find_url(&parsed, &mut found);
        if found.is_none() {
            find_url(&data["response"], &mut found);
        }
        found
    }

    fn find_url(v: &Value, out: &mut Option<String>) {
        if out.is_some() {
            return;
        }
        match v {
            Value::String(s) if s.starts_with("https://") && [".mp4", ".mov", ".webm", ".jpg", ".jpeg", ".png", ".webp"].iter().any(|e| s.split('?').next().unwrap_or("").to_lowercase().ends_with(e)) => {
                *out = Some(s.clone())
            }
            Value::String(s) if s.trim_start().starts_with('{') || s.trim_start().starts_with('[') => {
                if let Ok(inner) = serde_json::from_str::<Value>(s) {
                    find_url(&inner, out)
                }
            }
            Value::Array(a) => a.iter().for_each(|x| find_url(x, out)),
            Value::Object(o) => o.values().for_each(|x| find_url(x, out)),
            _ => {}
        }
    }

    /// Tasks that may already be paid for but gave no file (no link yet, or too slow). Asking again for the
    /// same model and input picks the task up instead of paying for a new one.
    static OPEN: Mutex<Vec<(u64, String, u64)>> = Mutex::new(Vec::new());

    fn task_key(model: &str, input: &Value) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (model, input.to_string()).hash(&mut h);
        h.finish()
    }

    fn open_task(k: u64, id: Option<&str>) {
        let mut g = OPEN.lock().unwrap_or_else(|e| e.into_inner());
        g.retain(|(key, _, at)| *key != k && now() < at + 86_400);
        if let Some(id) = id {
            g.push((k, id.to_string(), now()));
        }
    }

    /// Creates a task (or resumes an unfinished one), waits for it and returns the first result URL with the credits it used.
    fn run_task(key: &str, model: &str, input: Value, max_secs: u64, what: &str) -> Result<(String, Value), String> {
        let tk = task_key(model, &input);
        let resumed = OPEN.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(k, _, at)| *k == tk && now() < at + 86_400).map(|(_, id, _)| id.clone());
        let id = match resumed {
            Some(id) => id,
            None => {
                let made = send(poll_agent().post(&format!("{}/api/v1/jobs/createTask", base())), key, Some(json!({"model": model, "input": input})))
                    .map_err(|(c, m)| friendly(c, &m))?;
                made["data"]["taskId"].as_str().filter(|s| !s.is_empty() && s.len() < 200 && s.chars().all(|ch| ch.is_ascii_alphanumeric() || "-_".contains(ch)))
                    .ok_or_else(|| "KIE.ai did not start the task. Try again.".to_string())?
                    .to_string()
            }
        };
        let r = poll_task(key, &id, max_secs, what);
        match &r {
            Ok(_) => open_task(tk, None),
            Err((true, _)) => open_task(tk, Some(&id)),
            Err((false, _)) => open_task(tk, None),
        }
        r.map_err(|(_, e)| e)
    }

    /// Err((true, _)) means the task may still deliver, so a retry should look it up again.
    fn poll_task(key: &str, id: &str, max_secs: u64, what: &str) -> Result<(String, Value), (bool, String)> {
        let started = std::time::Instant::now();
        let mut wait = 2000u64;
        let mut misses = 0;
        let mut empty = 0;
        loop {
            std::thread::sleep(Duration::from_millis(wait));
            wait = (wait + 500).min(5000);
            match send(poll_agent().get(&format!("{}/api/v1/jobs/recordInfo?taskId={id}", base())), key, None) {
                Ok(v) => {
                    misses = 0;
                    let d = &v["data"];
                    match d["state"].as_str().unwrap_or("") {
                        "success" => {
                            if let Some(u) = result_url(d) {
                                return Ok((u, d["creditsConsumed"].clone()));
                            }
                            // The finished record can arrive a moment before its file link, so look a few more times.
                            empty += 1;
                            if empty > 8 {
                                return Err((true, format!("KIE.ai says the {what} is finished but sent no file link yet. Try again: the app asks for this same {what} again, so it is not paid twice.")));
                            }
                        }
                        "fail" => {
                            let why: String = d["failMsg"].as_str().unwrap_or("").chars().take(300).collect();
                            return Err((false, if why.is_empty() {
                                format!("KIE.ai could not make this {what}. Try again, or change the wording.")
                            } else {
                                format!("KIE.ai could not make this {what}. Original message: {why}")
                            }));
                        }
                        _ => {}
                    }
                }
                // A new task can be unknown for a moment, and one dropped poll should not lose a paid task.
                Err((code, msg)) => {
                    misses += 1;
                    if !matches!(code, 0 | 404 | 422 | 429 | 500..=599) || misses > 5 {
                        return Err((code == 0 || code >= 500, friendly(code, &msg)));
                    }
                }
            }
            if started.elapsed().as_secs() >= max_secs {
                return Err((true, format!("KIE.ai did not finish the {what} within {} minutes. Try again in a little while: the app picks up the same {what}, so it is not paid twice.", max_secs / 60)));
            }
        }
    }

    /// Result files live on another host, so the key is not sent with the download.
    fn download(url: &str, limit: u64) -> Result<Vec<u8>, String> {
        let trusted = |u: &str| {
            u.starts_with("https://")
                || (cfg!(debug_assertions) && u.strip_prefix("http://127.0.0.1:").map_or(false, |r| r.starts_with(|c: char| c.is_ascii_digit())))
        };
        // Redirects are followed by hand so every hop is checked, not only the first URL.
        let dl = ureq::AgentBuilder::new().try_proxy_from_env(true).timeout_connect(Duration::from_secs(20))
            .timeout_read(Duration::from_secs(120)).redirects(0).build();
        let mut cur = url.to_string();
        let mut hops = 0;
        let resp = loop {
            if !trusted(&cur) {
                return Err("KIE.ai sent a download address this app does not trust.".into());
            }
            match dl.get(&cur).call() {
                Ok(r) if (300..400).contains(&r.status()) && hops < 5 => {
                    cur = r.header("location").unwrap_or("").to_string();
                    hops += 1;
                }
                Ok(r) if (300..400).contains(&r.status()) => return Err("KIE.ai sent a download address this app does not trust.".into()),
                Ok(r) => break r,
                Err(ureq::Error::Status(code, _)) => return Err(format!("Could not download the result from KIE.ai (code {code}). Try again.")),
                Err(e) => return Err(friendly(0, &e.to_string())),
            }
        };
        let mut buf = Vec::new();
        resp.into_reader().take(limit + 1).read_to_end(&mut buf).map_err(|e| friendly(0, &e.to_string()))?;
        if buf.len() as u64 > limit {
            return Err("The file from KIE.ai is too large.".into());
        }
        Ok(buf)
    }

    /// One 16:9 image at 1K, about 8 credits with nano-banana-2.
    pub fn image(key: &str, model: &str, prompt: &str) -> Result<(Vec<u8>, &'static str), String> {
        let input = json!({"prompt": prompt, "aspect_ratio": "16:9", "resolution": "1K", "output_format": "jpg"});
        let (url, _) = run_task(key, model, input, 240, "image")?;
        let bytes = download(&url, 25 * 1024 * 1024)?;
        let ext = match bytes.get(..12) {
            Some(h) if h.starts_with(&[0xFF, 0xD8, 0xFF]) => "jpg",
            Some(h) if h.starts_with(b"\x89PNG") => "png",
            Some(h) if h.starts_with(b"RIFF") && &h[8..12] == b"WEBP" => "webp",
            _ => return Err("KIE.ai sent a file that is not an image. Click Redo.".into()),
        };
        Ok((bytes, ext))
    }

    /// One 8 second 1080p clip with Veo 3.1 Lite, about 35 credits.
    pub fn clip(key: &str, prompt: &str) -> Result<(Vec<u8>, Value), String> {
        let input = json!({"prompt": prompt, "model": "veo3_lite", "duration": 8, "aspect_ratio": "16:9",
            "resolution": "1080p", "generation_type": "TEXT_2_VIDEO"});
        let (url, credits) = run_task(key, "veo-3-1", input, 600, "clip")?;
        let bytes = download(&url, 300 * 1024 * 1024)?;
        if bytes.get(4..8) != Some(b"ftyp") {
            return Err("KIE.ai sent a file that is not an MP4 video. Try again.".into());
        }
        Ok((bytes, credits))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn body_codes_become_plain_messages() {
            assert_eq!(body_error(&json!({"code": 200, "msg": "success", "data": 5}), 200), None);
            assert_eq!(body_error(&json!({"code": 402, "msg": "Insufficient credits"}), 200), Some((402, "Insufficient credits".into())));
            assert_eq!(body_error(&json!({"code": "401", "msg": "bad"}), 200), Some((401, "bad".into())));
            assert_eq!(body_error(&json!({"type": "error", "error": {"type": "x", "message": "boom"}}), 400), Some((400, "boom".into())));
            assert_eq!(body_error(&json!({"content": [], "usage": {}}), 200), None);
            assert!(friendly(401, "").contains("rejected"));
            assert!(friendly(402, "").contains("Top up"));
            assert!(friendly(429, "").contains("Wait"));
            assert!(friendly(455, "").contains("maintenance"));
            assert!(friendly(501, "nsfw").contains("nsfw"));
            let m = friendly(401, "You do not have access permissions");
            assert!(m.contains("rejected") && m.ends_with("KIE.ai said: \"You do not have access permissions\""));
            let k = friendly(401, "Invalid key 3f9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c");
            assert!(k.contains("[hidden]") && !k.contains("3f9a8b7c6d5e"));
            assert_eq!(friendly(401, "  "), "The KIE.ai key was rejected. Check the key in Settings.");
        }
        #[test]
        fn result_json_is_a_string() {
            let d = json!({"state": "success", "resultJson": "{\"resultUrls\":[\"https://x.example/a.jpg\"],\"resultObject\":null}"});
            assert_eq!(result_url(&d).as_deref(), Some("https://x.example/a.jpg"));
            assert_eq!(result_url(&json!({"resultJson": {"resultUrls": ["https://y/b.png"]}})).as_deref(), Some("https://y/b.png"));
            assert_eq!(result_url(&json!({"resultJson": "{\"resultUrls\":[]}"})), None);
            assert_eq!(result_url(&json!({"resultJson": "{\"resultUrls\":[],\"originUrls\":[\"https://z/c.mp4\"]}"})).as_deref(), Some("https://z/c.mp4"));
            assert_eq!(result_url(&json!({"resultJson": "", "response": {"resultUrls": ["https://z/d.mp4"]}})).as_deref(), Some("https://z/d.mp4"));
            assert_eq!(result_url(&json!({"resultJson": {"result": {"video": {"url": "https://z/e.mp4?sig=1"}}}})).as_deref(), Some("https://z/e.mp4?sig=1"));
            assert_eq!(result_url(&json!({"resultJson": "not json"})), None);
        }
        #[test]
        fn defaults_and_dollars() {
            let mut c = Config::default();
            assert_eq!(text_model(&c), "claude-sonnet-5-5");
            assert_eq!(image_model(&c), "nano-banana-2");
            assert_eq!(image_provider(&c), "gemini");
            assert_eq!(active_provider(&c), "");
            c.kie_key = "k".into();
            assert_eq!(active_provider(&c), "kie");
            c.api_key = "a".into();
            assert_eq!(active_provider(&c), "claude");
            c.provider = "kie".into();
            assert_eq!(active_provider(&c), "kie");
            assert_eq!(usd(1840.0), 9.2);
            assert_eq!(usd(8.0), 0.04);
        }
    }
}

// Edit pack: one folder per video with voice over per act, subtitles, text files and the thumbnail

mod pack {
    use super::*;
    const CUE_CHARS: usize = 84;
    const CUE_SECS: f64 = 7.0;
    const LINE_CHARS: usize = 42;
    const TEXT_EXT: [&str; 5] = ["txt", "md", "json", "csv", "srt"];

    #[derive(Debug, Clone, PartialEq)]
    pub struct Cue {
        pub start: f64,
        pub end: f64,
        pub text: String,
    }

    type Fail = (u16, String);

    pub fn root(app: &Path) -> PathBuf {
        root_in(&eleven::out_dir(app))
    }

    pub fn root_in(downloads: &Path) -> PathBuf {
        downloads.join("Terra Studio")
    }

    /// Names Windows reserves for devices; a file or folder may not use them.
    pub fn reserved(stem: &str) -> bool {
        let s = stem.to_ascii_lowercase();
        ["con", "prn", "aux", "nul"].contains(&s.as_str())
            || (s.len() == 4 && (s.starts_with("com") || s.starts_with("lpt")) && s.as_bytes()[3].is_ascii_digit())
    }

    /// Folder name for one video: lowercase letters, digits and single hyphens.
    pub fn slug(s: &str) -> Option<String> {
        let mut out = String::new();
        for c in s.trim().chars() {
            if c.is_ascii_alphanumeric() {
                out.push(c.to_ascii_lowercase());
            } else if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
            if out.len() >= 60 {
                break;
            }
        }
        let out = out.trim_end_matches('-').to_string();
        (!out.is_empty() && !reserved(&out)).then_some(out)
    }

    /// Accepts a file name only if it is already safe; nothing is rewritten.
    pub fn safe_file(name: &str) -> Option<String> {
        let ok = !name.is_empty()
            && name.len() <= 80
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
            && !name.contains("..")
            && !name.starts_with('.')
            && !name.ends_with('.')
            && !reserved(name.split('.').next().unwrap_or(""));
        ok.then(|| name.to_string())
    }

    fn round3(x: f64) -> f64 {
        (x * 1000.0).round() / 1000.0
    }

    /// Subtitle lines from per-character timing: a new cue at each sentence end,
    /// and earlier when a cue would pass CUE_CHARS characters or CUE_SECS seconds.
    pub fn build_cues(chars: &[String], starts: &[f64], ends: &[f64]) -> Vec<Cue> {
        struct Word {
            text: String,
            start: f64,
            end: f64,
            stop: bool,
        }
        let n = chars.len().min(starts.len()).min(ends.len());
        let mut words: Vec<Word> = Vec::new();
        let mut cur: Option<Word> = None;
        for i in 0..n {
            let ch = &chars[i];
            if ch.chars().all(char::is_whitespace) {
                if let Some(w) = cur.take() {
                    words.push(w);
                }
                if ch.contains('\n') {
                    if let Some(last) = words.last_mut() {
                        last.stop = true;
                    }
                }
                continue;
            }
            match cur.as_mut() {
                Some(w) => {
                    w.text.push_str(ch);
                    w.end = w.end.max(ends[i]);
                }
                None => cur = Some(Word { text: ch.clone(), start: starts[i], end: ends[i].max(starts[i]), stop: false }),
            }
        }
        words.extend(cur);
        let mut cues: Vec<Cue> = Vec::new();
        let mut text = String::new();
        let (mut s, mut e) = (0.0f64, 0.0f64);
        let flush = |text: &mut String, s: f64, e: f64, cues: &mut Vec<Cue>| {
            if text.is_empty() {
                return;
            }
            let start = round3(s.max(cues.last().map_or(0.0, |c| c.end)));
            cues.push(Cue { start, end: round3(e.max(start + 0.05)), text: std::mem::take(text) });
        };
        for w in words {
            let closes = w.stop || w.text.trim_end_matches(['"', '\'', ')', ']', '\u{201d}', '\u{2019}']).ends_with(['.', '!', '?', '\u{2026}']);
            if !text.is_empty() && (text.chars().count() + 1 + w.text.chars().count() > CUE_CHARS || w.end - s > CUE_SECS) {
                flush(&mut text, s, e, &mut cues);
            }
            if text.is_empty() {
                s = w.start;
            } else {
                text.push(' ');
            }
            text.push_str(&w.text);
            e = w.end;
            if closes {
                flush(&mut text, s, e, &mut cues);
            }
        }
        flush(&mut text, s, e, &mut cues);
        cues
    }

    fn stamp(t: f64) -> String {
        let ms = (t.max(0.0) * 1000.0).round() as u64;
        format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
    }

    /// At most two lines, split at the word gap that makes them most equal.
    fn two_lines(text: &str) -> String {
        let words: Vec<&str> = text.split_whitespace().collect();
        let joined = words.join(" ");
        let total = joined.chars().count();
        if total <= LINE_CHARS || words.len() < 2 {
            return joined;
        }
        let mut best = (usize::MAX, 1);
        let mut left = 0;
        for (i, w) in words.iter().enumerate().take(words.len() - 1) {
            left += w.chars().count() + usize::from(i > 0);
            let right = total - left - 1;
            let longest = left.max(right);
            if longest < best.0 {
                best = (longest, i + 1);
            }
        }
        format!("{}\r\n{}", words[..best.1].join(" "), words[best.1..].join(" "))
    }

    /// SubRip text. Empty cues are dropped and the rest are numbered from 1.
    pub fn srt(cues: &[Cue]) -> String {
        let mut out = String::new();
        let mut n = 0;
        for c in cues {
            let body = two_lines(&c.text);
            if body.is_empty() {
                continue;
            }
            n += 1;
            out.push_str(&format!("{n}\r\n{} --> {}\r\n{body}\r\n\r\n", stamp(c.start), stamp(c.end.max(c.start))));
        }
        out
    }

    /// Where pack files go: the video folder with its numbered subfolders, or the old flat slug folder.
    struct Dest {
        slug: String,
        dir: PathBuf,
        video: Option<String>,
    }

    impl Dest {
        fn sub(&self, s: &str) -> PathBuf {
            if self.video.is_some() { self.dir.join(s) } else { self.dir.clone() }
        }
        fn json(&self) -> Value {
            json!({"slug": self.slug, "folder": self.video, "dir": self.dir.display().to_string()})
        }
        fn files(&self) -> (Vec<String>, Vec<String>) {
            if self.video.is_none() {
                let l = listing(&self.dir);
                return (l.clone(), l);
            }
            let mut names = Vec::new();
            let mut paths = Vec::new();
            for s in video::SUBS {
                for f in video::files(&self.dir.join(s)) {
                    paths.push(format!("{s}/{f}"));
                    names.push(f);
                }
            }
            names.sort();
            names.dedup();
            (names, paths)
        }
    }

    fn folder(app: &Path, v: &Value) -> Result<Dest, Fail> {
        let s = slug(v["slug"].as_str().unwrap_or("")).or_else(|| slug(v["title"].as_str().unwrap_or("")));
        if let Some((name, d)) = video::from_req(app, v)? {
            return Ok(Dest { slug: s.unwrap_or_default(), dir: d, video: Some(name) });
        }
        let name = s.ok_or((400, "The folder name is not valid.".to_string()))?;
        let d = root(app).join(&name);
        fs::create_dir_all(&d).map_err(|e| (500, format!("Could not create the edit pack folder: {e}")))?;
        Ok(Dest { slug: name, dir: d, video: None })
    }

    /// Every file now in the folder, sorted by name.
    fn listing(d: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(d)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_file())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| !n.ends_with(".tmp"))
            .collect();
        names.sort();
        names
    }

    /// Voice over for one act, with the subtitle lines that match it.
    pub fn voice(c: &Config, app: &Path, v: &Value) -> Result<Value, Fail> {
        let text = v["text"].as_str().unwrap_or("").trim();
        if text.is_empty() {
            return Err((400, "This act has no text.".into()));
        }
        if text.chars().count() > 60_000 {
            return Err((400, "This act is too long for one voice over.".into()));
        }
        let Some(index) = v["index"].as_u64().filter(|i| *i < 99) else {
            return Err((400, "The act number is not valid.".into()));
        };
        let dest = folder(app, v)?;
        let d = dest.sub(video::NARR);
        let n = index + 1;
        let prefix = format!("{n:02}-act-{n}");
        // The act name often starts with "Act 1" already, which the prefix repeats.
        let label = file_slug(v["name"].as_str().unwrap_or(""), "");
        let label = label.strip_prefix(&format!("act-{n}")).unwrap_or(&label).trim_matches('-');
        let label: String = label.chars().take(40).collect();
        let label = label.trim_matches('-');
        let file = if label.is_empty() { format!("{prefix}.mp3") } else { format!("{prefix}-{label}.mp3") };
        let timed = eleven::speak_timed(c, text).map_err(|e| (502, e))?;
        // A renamed act would otherwise leave two files for the same act.
        // In a video folder the replaced act goes to "older takes" instead of being deleted.
        let older = d.join(video::OLDER);
        for old in listing(&d) {
            if old.ends_with(".mp3") && (old == format!("{prefix}.mp3") || old.starts_with(&format!("{prefix}-"))) {
                if dest.video.is_some() {
                    let _ = fs::create_dir_all(&older);
                    let _ = fs::rename(d.join(&old), video::free_path(&older, &old, |p| p.exists()));
                } else {
                    let _ = fs::remove_file(d.join(old));
                }
            }
        }
        let path = d.join(&file);
        write_atomic(&path, &timed.audio).map_err(|e| (500, format!("Could not save the MP3: {e}")))?;
        let cues: Vec<Value> = build_cues(&timed.chars, &timed.starts, &timed.ends)
            .into_iter()
            .map(|c| json!({"start": c.start, "end": c.end, "text": c.text}))
            .collect();
        let mut out = dest.json();
        for (k, x) in [("file", json!(file)), ("path", json!(path.display().to_string())), ("seconds", json!(round3(timed.seconds))),
            ("chars", json!(timed.count)), ("chunks", json!(timed.chunks)), ("cues", json!(cues))] {
            out[k] = x;
        }
        Ok(out)
    }

    /// The plan thumbnail (by plan id) or a generated image (by its file name under gen/).
    fn thumb_source(app: &Path, id: &str) -> Option<(PathBuf, &'static str)> {
        if let Some(stem) = thumb_stem(id) {
            return ["jpg", "png", "webp"].into_iter().map(|ext| (app.join("thumbs").join(format!("{stem}.{ext}")), ext)).find(|(p, _)| p.is_file());
        }
        let (stem, ext) = id.rsplit_once('.')?;
        let ext = ["jpg", "png", "webp"].into_iter().find(|e| *e == ext)?;
        let ok = !stem.is_empty() && stem.len() <= 48 && stem.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        let p = app.join("gen").join(id);
        (ok && p.is_file()).then_some((p, ext))
    }

    /// Writes the text files, the subtitle file and the thumbnail into the pack folder.
    pub fn finish(app: &Path, v: &Value) -> Result<Value, Fail> {
        let empty = Vec::new();
        let files = v["files"].as_array().unwrap_or(&empty);
        if files.len() > 40 {
            return Err((400, "Too many files for one edit pack.".into()));
        }
        let mut texts: Vec<(String, &str)> = Vec::new();
        for f in files {
            let raw = f["name"].as_str().unwrap_or("");
            let name = safe_file(raw)
                .filter(|n| n.rsplit_once('.').is_some_and(|(_, ext)| TEXT_EXT.contains(&ext.to_ascii_lowercase().as_str())))
                .ok_or_else(|| (400, format!("The file name is not allowed: {}", raw.chars().take(80).collect::<String>())))?;
            texts.push((name, f["text"].as_str().unwrap_or("")));
        }
        let cues: Vec<Cue> = v["cues"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .take(20_000)
            .filter_map(|c| Some(Cue { start: c["start"].as_f64()?, end: c["end"].as_f64()?, text: c["text"].as_str()?.to_string() }))
            .collect();
        let dest = folder(app, v)?;
        let save = |sub: &str, file: &str, data: &[u8]| {
            write_atomic(&dest.sub(sub).join(file), data).map_err(|e| (500, format!("Could not save {file}: {e}")))
        };
        for (file, text) in &texts {
            save(video::sub_for(file).unwrap_or(video::SUBS[0]), file, text.as_bytes())?;
        }
        if !cues.is_empty() {
            save(video::NARR, "subtitle.srt", srt(&cues).as_bytes())?;
        }
        let mut thumbnail = Value::Null;
        if let Some(id) = v["thumb"].as_str().filter(|s| !s.is_empty()) {
            if let Some((src, ext)) = thumb_source(app, id) {
                // The picture keeps its real type; a PNG named .jpg confuses some editors.
                let bytes = fs::read(&src).map_err(|e| (500, format!("Could not read the thumbnail: {e}")))?;
                let td = dest.sub(video::SUBS[5]);
                let file = if dest.video.is_some() {
                    // 06 Thumbnail holds Sigit's own files too, so the pack only adds a picture there, never replaces one.
                    let p = video::save_new(&td, &format!("pack-thumbnail.{ext}"), &bytes)
                        .map_err(|e| (500, format!("Could not save the thumbnail: {}", video::why(&e))))?;
                    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                } else {
                    let file = format!("thumbnail.{ext}");
                    for old in ["jpg", "png", "webp"] {
                        let _ = fs::remove_file(td.join(format!("thumbnail.{old}")));
                    }
                    save(video::SUBS[5], &file, &bytes)?;
                    file
                };
                thumbnail = json!(file);
            }
        }
        let (files, paths) = dest.files();
        let mut out = dest.json();
        out["files"] = json!(files);
        out["paths"] = json!(paths);
        out["thumbnail"] = thumbnail;
        Ok(out)
    }

    /// One Veo clip saved into the pack folder. Spends KIE credits.
    pub fn clip(c: &Config, app: &Path, v: &Value) -> Result<Value, Fail> {
        if c.kie_key.is_empty() {
            return Err((400, "The KIE.ai key is not set. Open Settings, section KIE.ai.".into()));
        }
        let prompt = v["prompt"].as_str().unwrap_or("").trim();
        if prompt.is_empty() || prompt.chars().count() > 4000 {
            return Err((400, "The clip prompt is empty or too long.".into()));
        }
        let dest = folder(app, v)?;
        let label: String = file_slug(v["name"].as_str().unwrap_or(""), "clip").chars().take(40).collect();
        let file = format!("clip-{}.mp4", label.trim_matches('-'));
        let (bytes, credits) = kie::clip(&c.kie_key, prompt).map_err(|e| (502, e))?;
        let cd = dest.sub(video::SUBS[3]);
        let path = if dest.video.is_some() {
            video::save_new(&cd, &file, &bytes).map_err(|e| (500, format!("Could not save the clip: {}", video::why(&e))))?
        } else {
            let p = cd.join(&file);
            write_atomic(&p, &bytes).map_err(|e| (500, format!("Could not save the clip: {e}")))?;
            p
        };
        let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(file);
        let mut out = dest.json();
        for (k, x) in [("file", json!(file)), ("path", json!(path.display().to_string())), ("bytes", json!(bytes.len())), ("credits", credits)] {
            out[k] = x;
        }
        Ok(out)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn slugs_and_file_names_cannot_leave_the_folder() {
            assert_eq!(slug("Smilodon populator: the cat!").as_deref(), Some("smilodon-populator-the-cat"));
            assert_eq!(slug("../../Windows/System32").as_deref(), Some("windows-system32"));
            assert_eq!(slug("..\\..\\x").as_deref(), Some("x"));
            assert_eq!(slug("  --  "), None);
            assert_eq!(slug(".."), None);
            assert_eq!(slug("CON"), None);
            assert_eq!(slug("com1"), None);
            assert!(slug(&"a".repeat(300)).unwrap().len() <= 60);
            assert!(slug("é/ü").is_none());
            for good in ["script.txt", "flow-prompts.txt", "01-act-1.mp3", "SEO.md"] {
                assert_eq!(safe_file(good).as_deref(), Some(good));
            }
            for bad in ["", ".", "..", "../x.txt", "a/b.txt", "a\\b.txt", "a..txt", ".hidden", "x.txt.", "a b.txt", "c:x.txt",
                "nul.txt", "COM1.txt", "é.txt", "x.txt\0", "x\n.txt"] {
                assert_eq!(safe_file(bad), None, "{bad:?}");
            }
            assert_eq!(safe_file(&format!("{}.txt", "a".repeat(90))), None);
        }

        fn timing(text: &str, per_char: f64) -> (Vec<String>, Vec<f64>, Vec<f64>) {
            let chars: Vec<String> = text.chars().map(String::from).collect();
            let starts: Vec<f64> = (0..chars.len()).map(|i| i as f64 * per_char).collect();
            let ends: Vec<f64> = starts.iter().map(|s| s + per_char).collect();
            (chars, starts, ends)
        }

        #[test]
        fn cues_break_at_sentences_and_stay_short() {
            let text = "The cat waited. It was patient! Was it hungry? Yes.";
            let (c, s, e) = timing(text, 0.05);
            let cues = build_cues(&c, &s, &e);
            let lines: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
            assert_eq!(lines, ["The cat waited.", "It was patient!", "Was it hungry?", "Yes."]);
            assert_eq!(cues[0].start, 0.0);
            assert_eq!(cues[0].end, 0.75);
            assert_eq!(cues[1].start, 0.8);

            let long = "word ".repeat(100) + "end.";
            let (c, s, e) = timing(&long, 0.01);
            let cues = build_cues(&c, &s, &e);
            assert!(cues.len() > 5);
            assert!(cues.iter().all(|c| c.text.chars().count() <= CUE_CHARS));
            assert_eq!(cues.iter().map(|c| c.text.split(' ').count()).sum::<usize>(), 101);
            assert!(cues.windows(2).all(|w| w[0].end <= w[1].start && w[0].start < w[0].end));

            // Slow speech: the seven second limit cuts before the character limit does.
            let (c, s, e) = timing("one two three four five six seven eight nine ten", 0.5);
            let cues = build_cues(&c, &s, &e);
            assert!(cues.len() >= 3);
            assert!(cues.iter().all(|c| c.end - c.start <= CUE_SECS + 0.001));

            // A line break ends a cue even without punctuation; quotes after a full stop still end it.
            let (c, s, e) = timing("Title line\nHe said \"run.\" They ran", 0.05);
            let lines: Vec<String> = build_cues(&c, &s, &e).into_iter().map(|c| c.text).collect();
            assert_eq!(lines, ["Title line", "He said \"run.\"", "They ran"]);
            assert!(build_cues(&[], &[], &[]).is_empty());
        }

        #[test]
        fn srt_is_numbered_timed_and_wrapped() {
            assert_eq!(stamp(0.0), "00:00:00,000");
            assert_eq!(stamp(3723.4567), "01:02:03,457");
            assert_eq!(stamp(59.9996), "00:01:00,000");
            let cues = vec![
                Cue { start: 0.0, end: 1.5, text: "Short line.".into() },
                Cue { start: 1.5, end: 1.6, text: "   ".into() },
                Cue { start: 2.0, end: 8.25, text: "This cue is long enough that it has to be split into two balanced lines here.".into() },
            ];
            let out = srt(&cues);
            assert!(out.starts_with("1\r\n00:00:00,000 --> 00:00:01,500\r\nShort line.\r\n\r\n2\r\n00:00:02,000 --> 00:00:08,250\r\n"));
            let block: Vec<&str> = out.split("\r\n\r\n").nth(1).unwrap().split("\r\n").collect();
            assert_eq!(block.len(), 4);
            assert!(block[2].chars().count() <= LINE_CHARS && block[3].chars().count() <= LINE_CHARS);
            assert_eq!(format!("{} {}", block[2], block[3]), cues[2].text);
            assert_eq!(two_lines("a  b\nc"), "a b c");
            assert_eq!(srt(&[]), "");
        }
    }
}

// One folder per video: Downloads\Terra Studio\<date title>\ with numbered subfolders.
// Files are only ever moved or copied here, never deleted.

mod video {
    use super::*;
    use std::collections::{HashMap, HashSet};

    pub const SUBS: [&str; 8] = ["01 Script", "02 Narration", "03 Images", "04 Video clips", "05 Infographics", "06 Thumbnail", "07 Upload", "08 Premiere"];
    pub const NARR: &str = "02 Narration";
    pub const OLDER: &str = "older takes";
    const MAX: usize = 80;
    const ILLEGAL: &str = "<>:\"/\\|?*";
    /// Moving takes and migrating run one at a time.
    pub static LOCK: Mutex<()> = Mutex::new(());

    type Fail = (u16, String);

    /// A Windows-safe folder name. Idempotent, so a name is valid exactly when sanitize leaves it unchanged.
    pub fn sanitize(raw: &str) -> String {
        let clean: String = raw.chars().map(|c| if c.is_control() || ILLEGAL.contains(c) { ' ' } else { c }).collect();
        let mut out: String = clean.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX).collect();
        out = out.trim_end_matches(['.', ' ']).to_string();
        if out.is_empty() {
            out = "Untitled video".into();
        }
        if pack::reserved(out.split('.').next().unwrap_or("").trim_end()) {
            if out.chars().count() >= MAX {
                out = out.chars().take(MAX - 1).collect::<String>().trim_end_matches(['.', ' ']).to_string();
            }
            out = format!("_{out}");
        }
        out
    }

    pub fn valid_name(name: &str) -> bool {
        !name.is_empty() && name.len() <= 4 * MAX && sanitize(name) == name
    }

    /// "<YYYY-MM-DD> <title>"; a missing or malformed date is left out.
    pub fn folder_name(date: &str, title: &str) -> String {
        let d = date.trim();
        let ok = d.len() == 10 && d.bytes().enumerate().all(|(i, b)| if i == 4 || i == 7 { b == b'-' } else { b.is_ascii_digit() });
        let t = sanitize(title);
        if ok { sanitize(&format!("{d} {t}")) } else { t }
    }

    /// The base name, or "<base> (2)", "(3)"... when a folder with that name already exists.
    pub fn unique_name(base: &str, taken: impl Fn(&str) -> bool) -> String {
        if !taken(base) {
            return base.to_string();
        }
        (2..)
            .map(|n| {
                let tail = format!(" ({n})");
                let head: String = base.chars().take(MAX - tail.chars().count()).collect();
                sanitize(&format!("{}{tail}", head.trim_end()))
            })
            .find(|c| !taken(c))
            .unwrap()
    }

    /// A file name that is safe to join to a folder: one plain component, no stream or device names.
    pub fn valid_file(name: &str) -> bool {
        !name.is_empty()
            && name.chars().count() <= 160
            && !name.chars().any(|c| c.is_control() || ILLEGAL.contains(c))
            && !name.starts_with(['.', ' '])
            && !name.ends_with(['.', ' '])
            && !name.contains("..")
            && !pack::reserved(name.split('.').next().unwrap_or("").trim_end())
    }

    /// dir/file, or dir/"stem (2).ext", "(3)"... when the name is taken. Never overwrites.
    pub fn free_path(dir: &Path, file: &str, taken: impl Fn(&Path) -> bool) -> PathBuf {
        let first = dir.join(file);
        if !taken(&first) {
            return first;
        }
        let (stem, ext) = match file.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
            _ => (file, String::new()),
        };
        (2..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !taken(p)).unwrap()
    }

    /// Writes bytes as file, or "file (2)", "(3)"...; a file that already holds the same bytes is reused. Never overwrites.
    pub fn save_new(dir: &Path, file: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
        let same = |p: &Path| fs::metadata(p).is_ok_and(|m| m.len() == bytes.len() as u64) && fs::read(p).is_ok_and(|b| b == bytes);
        let p = free_path(dir, file, |p| p.exists() && !same(p));
        if !p.exists() {
            write_atomic(&p, bytes)?;
        }
        Ok(p)
    }

    /// Windows words its own error text in the system language, so the reason is said in English here.
    pub fn why(e: &std::io::Error) -> String {
        use std::io::ErrorKind as K;
        let what = match e.kind() {
            K::PermissionDenied => "Windows denied access. The file may be open in another program, or the folder is protected",
            K::NotFound => "the file or folder was not found",
            K::AlreadyExists => "a file with that name already exists",
            K::StorageFull => "the drive is full",
            K::ReadOnlyFilesystem => "the drive is read-only",
            K::InvalidFilename => "Windows does not accept this file name",
            _ => "Windows reported an error",
        };
        match e.raw_os_error() {
            Some(c) => format!("{what} (error {c})."),
            None => format!("{what}."),
        }
    }

    /// Which numbered subfolder a file belongs in; None leaves it in the video folder itself.
    pub fn sub_for(file: &str) -> Option<&'static str> {
        let f = file.to_lowercase();
        let ext = f.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        let has = |w: &str| f.contains(w);
        Some(match ext {
            "mp3" | "wav" | "m4a" | "srt" | "vtt" => SUBS[1],
            "mp4" | "mov" | "webm" | "mkv" => SUBS[3],
            "prproj" => SUBS[7],
            "jpg" | "jpeg" | "png" | "webp" if has("thumb") => SUBS[5],
            "jpg" | "jpeg" | "png" | "webp" => SUBS[2],
            _ if has("infographic") => SUBS[4],
            _ if has("flow") || has("prompt") => SUBS[2],
            _ if has("title") || has("description") || has("seo") || has("tags") || has("upload") => SUBS[6],
            "txt" | "md" | "json" | "csv" | "doc" | "docx" | "rtf" => SUBS[0],
            _ => return None,
        })
    }

    pub fn is_act(file: &str) -> bool {
        let b = file.as_bytes();
        b.len() > 7 && b[0].is_ascii_digit() && b[1].is_ascii_digit() && file[2..].starts_with("-act-")
    }

    /// A full voice over take: any MP3 in 02 Narration that is not one act of the edit pack.
    pub fn is_take(file: &str) -> bool {
        file.to_ascii_lowercase().ends_with(".mp3") && !is_act(file)
    }

    /// Unix time in names like "slug-1731480000.mp3" or "voice-over-1731480000 (2).mp3".
    pub fn stamp_of(file: &str) -> Option<u64> {
        let stem = file.strip_suffix(".mp3")?;
        let stem = match stem.rsplit_once(" (") {
            Some((s, n)) if n.strip_suffix(')').is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())) => s,
            _ => stem,
        };
        let (_, digits) = stem.rsplit_once('-')?;
        ((9..=11).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())).then(|| digits.parse().ok()).flatten()
    }

    /// A voice over that ElevenLabs saved loose in Downloads for this title: exactly "<slug>-<timestamp>.mp3".
    pub fn take_for(slug: &str, file: &str) -> Option<u64> {
        if slug.is_empty() {
            return None;
        }
        let digits = file.strip_prefix(slug)?.strip_prefix('-')?.strip_suffix(".mp3")?;
        ((9..=11).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())).then(|| digits.parse().ok()).flatten()
    }

    pub fn enc(s: &str) -> String {
        s.bytes()
            .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
            .collect()
    }

    pub fn audio_url(name: &str, file: &str) -> String {
        format!("/api/audio/{}/{}", enc(name), enc(file))
    }

    /// Regular files in a folder, sorted by name, without temp files and Windows/Mac clutter.
    pub fn files(d: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(d)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| {
                let l = n.to_ascii_lowercase();
                !l.ends_with(".tmp") && !["desktop.ini", "thumbs.db", ".ds_store"].contains(&l.as_str())
            })
            .collect();
        names.sort();
        names
    }

    pub fn dir(app: &Path, name: &str) -> Result<PathBuf, Fail> {
        if !valid_name(name) {
            return Err((400, "The video folder name is not valid.".into()));
        }
        Ok(pack::root(app).join(name))
    }

    pub fn ensure(app: &Path, name: &str) -> Result<PathBuf, Fail> {
        let d = dir(app, name)?;
        create(&d)?;
        Ok(d)
    }

    fn create(d: &Path) -> Result<(), Fail> {
        for s in SUBS {
            fs::create_dir_all(d.join(s)).map_err(|e| (500, format!("Could not create the video folder: {}", why(&e))))?;
        }
        fs::create_dir_all(d.join(NARR).join(OLDER)).map_err(|e| (500, format!("Could not create the video folder: {}", why(&e))))
    }

    /// The "folder" field of a writer's request: Ok(None) when absent, an error when present but not valid.
    pub fn from_req(app: &Path, v: &Value) -> Result<Option<(String, PathBuf)>, Fail> {
        match v.get("folder") {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(n)) if n.is_empty() => Ok(None),
            Some(Value::String(n)) => ensure(app, n).map(|d| Some((n.clone(), d))),
            Some(_) => Err((400, "The video folder name is not valid.".into())),
        }
    }

    pub fn summary(d: &Path, name: &str) -> Value {
        let mut total = 0;
        let folders: Vec<Value> = SUBS
            .iter()
            .map(|s| {
                let list = files(&d.join(s));
                total += list.len();
                let mut o = json!({"name": s, "files": list.len(), "list": list.iter().take(5).collect::<Vec<_>>()});
                if *s == NARR {
                    let older = files(&d.join(NARR).join(OLDER)).len();
                    total += older;
                    o["older"] = json!(older);
                }
                o
            })
            .collect();
        json!({"name": name, "dir": d.display().to_string(), "exists": d.is_dir(), "total": total,
            "root_files": files(d).len(), "folders": folders})
    }

    pub fn image_type(file: &str) -> Option<&'static str> {
        match file.rsplit_once('.')?.1.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" => Some("image/jpeg"),
            "png" => Some("image/png"),
            "webp" => Some("image/webp"),
            _ => None,
        }
    }

    /// The images in one 06 Thumbnail folder, newest first. A missing folder is an empty list.
    pub fn thumbs(d: &Path, name: &str) -> Value {
        let mut list: Vec<(u64, String)> = files(d).into_iter().filter(|f| image_type(f).is_some()).map(|f| (mtime(&d.join(&f)), f)).collect();
        list.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        let files: Vec<Value> = list
            .iter()
            .map(|(at, f)| json!({"file": f, "at": at, "url": format!("/api/vthumb/{}/{}", enc(name), enc(f))}))
            .collect();
        json!({"name": name, "files": files})
    }

    fn mtime(p: &Path) -> u64 {
        fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())
    }

    fn seconds(p: &Path) -> Option<f64> {
        let total = fs::metadata(p).ok()?.len() as usize;
        let mut head = Vec::new();
        fs::File::open(p).ok()?.take(70_000).read_to_end(&mut head).ok()?;
        eleven::mp3_seconds_len(&head, total).map(|s| (s * 10.0).round() / 10.0)
    }

    /// Every take for a video, newest first. Only the newest take in 02 Narration counts as in use.
    pub fn takes(d: &Path, name: &str) -> Value {
        let narr = d.join(NARR);
        let mut list: Vec<(u64, Value)> = Vec::new();
        for (sub, here) in [(narr.clone(), true), (narr.join(OLDER), false)] {
            for f in files(&sub).into_iter().filter(|f| is_take(f)) {
                let p = sub.join(&f);
                let m = mtime(&p);
                let at = stamp_of(&f).unwrap_or(m);
                list.push((at, json!({"file": f, "in_use": here, "where": if here { "current" } else { "older" },
                    "size": fs::metadata(&p).map_or(0, |x| x.len()), "mtime": m, "at": at, "seconds": seconds(&p),
                    "url": audio_url(name, &f)})));
            }
        }
        list.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1["in_use"].as_bool().cmp(&a.1["in_use"].as_bool())));
        let mut inuse = Value::Null;
        for (_, t) in list.iter_mut() {
            if t["in_use"] == true {
                if inuse.is_null() { inuse = t["file"].clone() } else { t["in_use"] = json!(false) }
            }
        }
        json!({"folder": name, "dir": d.display().to_string(), "inuse": inuse, "count": list.len(),
            "takes": list.into_iter().map(|(_, t)| t).collect::<Vec<_>>()})
    }

    /// Moves every take except `keep` from 02 Narration into "older takes". Returns (moved, errors).
    fn retire(narr: &Path, keep: Option<&str>) -> (Vec<String>, Vec<String>) {
        let older = narr.join(OLDER);
        let _ = fs::create_dir_all(&older);
        let (mut moved, mut errors) = (Vec::new(), Vec::new());
        for f in files(narr).into_iter().filter(|f| is_take(f) && Some(f.as_str()) != keep) {
            let to = free_path(&older, &f, |p| p.exists());
            match fs::rename(narr.join(&f), &to) {
                Ok(()) => moved.push(to.file_name().and_then(|n| n.to_str()).unwrap_or(&f).to_string()),
                Err(e) => errors.push(format!("{f}: {}", why(&e))),
            }
        }
        (moved, errors)
    }

    /// Saves a new take as the one in use; the previous one moves to "older takes".
    pub fn save_take(d: &Path, name: &str, s: &eleven::Spoken) -> Result<Value, Fail> {
        let narr = d.join(NARR);
        let at = now();
        let path = free_path(&narr, &format!("voice-over-{at}.mp3"), |p| p.exists());
        let file = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        write_atomic(&path, &s.audio).map_err(|e| (500, format!("Could not save the MP3: {}", why(&e))))?;
        let (older, _) = retire(&narr, Some(&file));
        Ok(json!({"file": file, "path": path.display().to_string(), "folder": name, "dir": d.display().to_string(),
            "url": audio_url(name, &file), "chars": s.chars, "chunks": s.chunks, "bytes": s.audio.len(),
            "seconds": eleven::mp3_seconds(&s.audio), "at": at, "older": older}))
    }

    /// Makes one existing take the one in use. Nothing is deleted.
    pub fn use_take(d: &Path, file: &str) -> Result<(), Fail> {
        if !valid_file(file) || !is_take(file) {
            return Err((400, "This voice over take is not valid.".into()));
        }
        let narr = d.join(NARR);
        let older = narr.join(OLDER);
        let src = [narr.join(file), older.join(file)].into_iter().find(|p| p.is_file())
            .ok_or((404, "This voice over take was not found. It may have been moved or renamed in Explorer.".to_string()))?;
        let (_, errors) = retire(&narr, (src.parent() == Some(narr.as_path())).then_some(file));
        if !errors.is_empty() {
            return Err((500, format!("Could not move the other takes into \"older takes\": {}", errors.join("; "))));
        }
        if src.parent() == Some(older.as_path()) {
            fs::rename(&src, free_path(&narr, file, |p| p.exists())).map_err(|e| (500, format!("Could not move the take: {}", why(&e))))?;
        }
        Ok(())
    }

    pub struct Move {
        pub from: PathBuf,
        pub to: PathBuf,
        pub video: String,
        pub kind: &'static str,
    }

    impl Move {
        pub fn json(&self, root: &Path) -> Value {
            let vd = root.join(&self.video);
            let sub = self.to.parent().and_then(|p| p.strip_prefix(&vd).ok()).map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
            let file = self.to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let mut o = json!({"from": self.from.display().to_string(), "to": self.to.display().to_string(), "video": self.video,
                "kind": self.kind, "sub": sub, "file": file,
                "name": self.from.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()});
            if self.kind == "voice" {
                o["in_use"] = json!(sub == NARR);
                o["url"] = json!(audio_url(&self.video, &file));
            }
            o
        }
    }

    fn strings(v: &Value) -> Vec<String> {
        match v {
            Value::String(s) => vec![s.clone()],
            Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
            _ => vec![],
        }
    }

    /// Proposed moves of older files into video folders. `root` is Downloads\Terra Studio, `downloads` is Downloads.
    pub fn plan(root: &Path, downloads: &Path, videos: &[Value]) -> (Vec<Move>, Vec<Value>) {
        let mut moves: Vec<Move> = Vec::new();
        let mut errors: Vec<Value> = Vec::new();
        let mut claimed: HashSet<PathBuf> = HashSet::new();
        let mut planned: HashSet<String> = HashSet::new();
        let names: HashSet<&str> = videos.iter().filter_map(|v| v["name"].as_str()).collect();
        let loose = files(downloads);
        struct Acc<'a> {
            moves: &'a mut Vec<Move>,
            planned: &'a mut HashSet<String>,
            claimed: &'a mut HashSet<PathBuf>,
        }
        fn add(acc: &mut Acc, from: PathBuf, dest: PathBuf, video: &str, kind: &'static str) {
            let key = |p: &Path| p.to_string_lossy().to_lowercase();
            let file = from.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let to = free_path(&dest, &file, |p| p.exists() || acc.planned.contains(&key(p)));
            acc.planned.insert(key(&to));
            acc.claimed.insert(from.clone());
            acc.moves.push(Move { from, to, video: video.to_string(), kind });
        }
        // Titles that share their first words give identical file names, and two videos can send the same pack slug.
        // Such a file or folder could belong to either video, so it stays where it is and is reported.
        let mut owners: HashMap<String, HashSet<&str>> = HashMap::new();
        for v in videos {
            let name = v["name"].as_str().unwrap_or("");
            if valid_name(name) {
                let (slugs, voice, thumbs) = matches(v, &loose);
                for k in slugs.into_iter().map(|s| format!("/{s}")).chain(voice.into_iter().map(|(_, f)| f)).chain(thumbs) {
                    owners.entry(k).or_default().insert(name);
                }
            }
        }
        let shared = |k: &str| owners.get(k).is_some_and(|o| o.len() > 1);
        let mut told: HashSet<String> = HashSet::new();
        for (k, o) in &owners {
            if o.len() > 1 && told.insert(k.clone()) {
                let mut vs: Vec<&str> = o.iter().copied().collect();
                vs.sort();
                let what = match k.strip_prefix('/') {
                    Some(s) => format!("The old edit pack folder \"{s}\""),
                    None => format!("\"{k}\""),
                };
                errors.push(json!({"file": k.trim_start_matches('/'), "videos": vs,
                    "error": format!("{what} matches more than one video ({}), so it was left where it is. Move it by hand.", vs.join(", "))}));
            }
        }
        errors.sort_by(|a, b| a["file"].as_str().cmp(&b["file"].as_str()));
        for v in videos {
            let name = v["name"].as_str().unwrap_or("");
            if !valid_name(name) {
                errors.push(json!({"video": name, "id": v["id"], "error": "The video folder name is not valid."}));
                continue;
            }
            let vd = root.join(name);
            let (slugs, found, thumbs) = matches(v, &loose);
            // Old flat edit pack folder, by its slug. A slug that is another video's folder is left alone.
            for s in slugs {
                let src = root.join(&s);
                if (s != name && names.contains(s.as_str())) || !src.is_dir() || shared(&format!("/{s}")) {
                    continue;
                }
                for f in files(&src) {
                    let from = src.join(&f);
                    if claimed.contains(&from) {
                        continue;
                    }
                    let dest = sub_for(&f).map_or(vd.clone(), |x| vd.join(x));
                    add(&mut Acc { moves: &mut moves, planned: &mut planned, claimed: &mut claimed }, from, dest, name, "pack");
                }
            }
            // Voice overs ElevenLabs saved loose in Downloads: the newest becomes the take in use.
            let narr = vd.join(NARR);
            let mut has_take = files(&narr).iter().any(|f| is_take(f));
            let found: Vec<String> = found.into_iter().map(|(_, f)| f).filter(|f| !shared(f) && !claimed.contains(&downloads.join(f))).collect();
            for f in found {
                let dest = if has_take { narr.join(OLDER) } else { narr.clone() };
                has_take = true;
                add(&mut Acc { moves: &mut moves, planned: &mut planned, claimed: &mut claimed }, downloads.join(&f), dest, name, "voice");
            }
            for f in thumbs {
                let from = downloads.join(&f);
                if !shared(&f) && !claimed.contains(&from) {
                    add(&mut Acc { moves: &mut moves, planned: &mut planned, claimed: &mut claimed }, from, vd.join(SUBS[5]), name, "thumb");
                }
            }
        }
        (moves, errors)
    }

    /// What one video could claim: its pack slugs, loose voice overs (with their time, newest first) and loose thumbnails.
    fn matches(v: &Value, loose: &[String]) -> (Vec<String>, Vec<(u64, String)>, Vec<String>) {
        let mut slugs: Vec<String> = strings(&v["slug"]).into_iter().chain(strings(&v["slugs"])).filter_map(|s| pack::slug(&s)).collect();
        slugs.sort();
        slugs.dedup();
        let titles: Vec<String> = strings(&v["title"]).into_iter().chain(strings(&v["titles"])).filter(|t| !t.trim().is_empty()).collect();
        let tslugs: HashSet<String> = titles.iter().map(|t| eleven::title_slug(t)).filter(|s| !s.is_empty()).collect();
        let mut voice: Vec<(u64, String)> =
            loose.iter().filter_map(|f| tslugs.iter().find_map(|s| take_for(s, f)).map(|t| (t, f.clone()))).collect();
        voice.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
        // Thumbnails saved to Downloads by "Pick this": "<title> thumbnail <n>.jpg" as a slug.
        let mut thumbs: Vec<String> = titles
            .iter()
            .flat_map(|t| (1..=3).map(move |n| format!("{}.jpg", file_slug(&format!("{t} thumbnail {n}"), "thumbnail"))))
            .filter(|f| loose.contains(f))
            .collect();
        thumbs.sort();
        thumbs.dedup();
        (slugs, voice, thumbs)
    }

    /// Carries out a plan: rename, or copy and keep the original when rename fails (another drive).
    pub fn run(root: &Path, moves: &[Move]) -> Value {
        let (mut moved, mut copied, mut failed) = (Vec::new(), Vec::new(), Vec::new());
        let mut made: HashSet<String> = HashSet::new();
        for m in moves {
            if made.insert(m.video.clone()) {
                if let Err((_, e)) = create(&root.join(&m.video)) {
                    failed.push(json!({"from": m.from.display().to_string(), "video": m.video, "error": e}));
                    continue;
                }
            }
            if !m.from.is_file() {
                failed.push(json!({"from": m.from.display().to_string(), "video": m.video, "error": "The file is no longer there."}));
                continue;
            }
            let parent = m.to.parent().unwrap_or(root);
            let _ = fs::create_dir_all(parent);
            let file = m.to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let to = free_path(parent, &file, |p| p.exists());
            let done = Move { from: m.from.clone(), to: to.clone(), video: m.video.clone(), kind: m.kind };
            match fs::rename(&m.from, &to) {
                Ok(()) => moved.push(done.json(root)),
                Err(_) => match fs::copy(&m.from, &to) {
                    Ok(_) => copied.push(done.json(root)),
                    Err(e) => failed.push(json!({"from": m.from.display().to_string(), "video": m.video,
                        "error": format!("Could not move or copy this file: {}", why(&e))})),
                },
            }
        }
        json!({"dry": false, "moved": moved, "copied": copied, "failed": failed})
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn temp(tag: &str) -> PathBuf {
            let d = std::env::temp_dir().join(format!("terra-test-{tag}-{}-{}", std::process::id(), now()));
            let _ = fs::remove_dir_all(&d);
            fs::create_dir_all(&d).unwrap();
            d
        }

        #[test]
        fn sanitizer_makes_windows_safe_names() {
            assert_eq!(sanitize("Smilodon: the <cat>?"), "Smilodon the cat");
            assert_eq!(sanitize("a/b\\c|d*e\"f"), "a b c d e f");
            assert_eq!(sanitize("  many   spaces\t\nand\u{0007}bell  "), "many spaces and bell");
            assert_eq!(sanitize("ends with dots..."), "ends with dots");
            assert_eq!(sanitize("dots . . ."), "dots");
            for empty in ["", "   ", ".", "..", "...", "???", "\u{0000}"] {
                assert_eq!(sanitize(empty), "Untitled video", "{empty:?}");
            }
            for r in ["CON", "con", "Nul", "aux.txt", "COM1", "lpt9", "PRN.", "con "] {
                let s = sanitize(r);
                assert!(s.starts_with('_'), "{r:?} -> {s:?}");
                assert!(valid_name(&s));
            }
            assert_eq!(sanitize("Conan the cave bear"), "Conan the cave bear");
            assert_eq!(sanitize("2026-11-20 CON"), "2026-11-20 CON");
            let long = sanitize(&"Megatherium ".repeat(20));
            assert!(long.chars().count() <= 80 && !long.ends_with(' '));
            let reserved_long = sanitize(&format!("CON.{}", "x".repeat(100)));
            assert!(reserved_long.chars().count() <= 80 && reserved_long.starts_with("_CON."));
            assert_eq!(sanitize("Mammut – Ära der Riesen 🦣"), "Mammut – Ära der Riesen 🦣");
            let uni = sanitize(&"é".repeat(200));
            assert_eq!(uni.chars().count(), 80);
            for s in ["Smilodon: the <cat>?", "CON", "CON.xx.", &"Megatherium ".repeat(20), &format!("CON.{}", "x".repeat(100)), "..", "a\u{3000}b"] {
                let once = sanitize(s);
                assert_eq!(sanitize(&once), once, "not idempotent for {s:?}");
            }
            assert_eq!(folder_name("2026-11-20", "Smilodon: The Cat"), "2026-11-20 Smilodon The Cat");
            assert_eq!(folder_name("", "Smilodon"), "Smilodon");
            assert_eq!(folder_name("20/11/2026", "Smilodon"), "Smilodon");
            assert_eq!(folder_name("2026-11-20", ""), "2026-11-20 Untitled video");
            assert!(folder_name("2026-11-20", &"word ".repeat(40)).chars().count() <= 80);
        }

        #[test]
        fn names_with_traversal_or_separators_are_rejected() {
            for bad in ["", ".", "..", "../x", "..\\x", "a/b", "a\\b", "C:", "C:\\Windows", "x:y", " lead", "trail ", "trail.", "CON",
                "nul", "a\u{0000}b", "a\nb", "two  spaces", "%2e%2e/x"] {
                assert!(!valid_name(bad), "{bad:?}");
            }
            for good in ["2026-11-20 Smilodon the cat", "Untitled video", "_CON", "Ära 🦣", "x (2)"] {
                assert!(valid_name(good), "{good:?}");
            }
            for bad in ["", ".", "..", "../a.mp3", "a/b.mp3", "a\\b.mp3", "x.mp3:evil", "c:a.mp3", ".hidden.mp3", "a..mp3", "nul.mp3",
                "COM1.mp3", "a\u{0000}.mp3", "a.mp3 ", "a.mp3."] {
                assert!(!valid_file(bad), "{bad:?}");
            }
            for good in ["voice-over-1731480000.mp3", "voice-over-1731480000 (2).mp3", "01-act-1-the-hunt.mp3", "Mammut äre.mp3"] {
                assert!(valid_file(good), "{good:?}");
            }
            // Only invalid names here, so nothing is created on disk.
            let app = Path::new("/nonexistent-terra-app");
            assert!(dir(app, "../../etc").is_err());
            assert!(dir(app, "..").is_err());
            assert!(from_req(app, &json!({"folder": "../x"})).is_err());
            assert!(from_req(app, &json!({"folder": 5})).is_err());
            assert!(matches!(from_req(app, &json!({"slug": "x"})), Ok(None)));
            assert!(matches!(from_req(app, &json!({"folder": ""})), Ok(None)));
        }

        #[test]
        fn clashes_get_a_number_and_nothing_is_overwritten() {
            let d = Path::new("/v");
            let taken: HashSet<PathBuf> = ["/v/a.mp3", "/v/a (2).mp3", "/v/README", "/v/.env"].iter().map(PathBuf::from).collect();
            let t = |p: &Path| taken.contains(p);
            assert_eq!(free_path(d, "b.mp3", t), PathBuf::from("/v/b.mp3"));
            assert_eq!(free_path(d, "a.mp3", t), PathBuf::from("/v/a (3).mp3"));
            assert_eq!(free_path(d, "README", t), PathBuf::from("/v/README (2)"));
            assert_eq!(free_path(d, ".env", t), PathBuf::from("/v/.env (2)"));
            assert_eq!(free_path(d, "x.tar.gz", |p| p == Path::new("/v/x.tar.gz")), PathBuf::from("/v/x.tar (2).gz"));
            assert_eq!(unique_name("2026-11-20 Smilodon", |n| n == "2026-11-20 Smilodon"), "2026-11-20 Smilodon (2)");
            assert_eq!(unique_name("x", |n| n == "x" || n == "x (2)"), "x (3)");
            let long = sanitize(&"y".repeat(100));
            let u = unique_name(&long, |n| n == long);
            assert!(u.ends_with(" (2)") && u.chars().count() <= 80 && valid_name(&u));
            assert_eq!(stamp_of("voice-over-1731480000.mp3"), Some(1731480000));
            assert_eq!(stamp_of("voice-over-1731480000 (2).mp3"), Some(1731480000));
            assert_eq!(stamp_of("smilodon-the-cat-1731480000.mp3"), Some(1731480000));
            assert_eq!(stamp_of("my narration.mp3"), None);
        }

        #[test]
        fn saving_never_replaces_an_existing_file() {
            let d = temp("save");
            fs::write(d.join("thumbnail.png"), "his own").unwrap();
            let p1 = save_new(&d, "pack-thumbnail.jpg", b"one").unwrap();
            assert_eq!(p1, d.join("pack-thumbnail.jpg"));
            assert_eq!(save_new(&d, "pack-thumbnail.jpg", b"one").unwrap(), p1);
            let p2 = save_new(&d, "pack-thumbnail.jpg", b"two").unwrap();
            assert_eq!(p2, d.join("pack-thumbnail (2).jpg"));
            assert_eq!(save_new(&d, "pack-thumbnail.jpg", b"two").unwrap(), p2);
            assert_eq!(fs::read(&p1).unwrap(), b"one");
            assert_eq!(fs::read_to_string(d.join("thumbnail.png")).unwrap(), "his own");
            let e = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
            assert!(why(&e).starts_with("Windows denied access"));
            assert!(why(&std::io::Error::from_raw_os_error(2)).is_ascii());
            let _ = fs::remove_dir_all(&d);
        }

        #[test]
        fn files_that_match_two_videos_stay_where_they_are() {
            let base = temp("ambiguous");
            let (dl, root) = (base.join("Downloads"), base.join("Downloads").join("Terra Studio"));
            fs::create_dir_all(root.join("smilodon")).unwrap();
            fs::write(root.join("smilodon").join("01-act-1.mp3"), "a").unwrap();
            let shared = "what-happened-when-the-asteroid-hit-the-earth";
            for f in [format!("{shared}-1731000000.mp3"), format!("{shared}-1731480000.mp3"), "mammut-1731480000.mp3".into()] {
                fs::write(dl.join(f), "x").unwrap();
            }
            let (a, b, c) = ("2026-11-20 Day 1", "2026-11-27 Day 2", "2026-12-04 Mammut");
            let videos = vec![
                json!({"name": a, "title": "What Happened When the Asteroid Hit the Earth? Day 1", "slug": "smilodon"}),
                json!({"name": b, "title": "What Happened When the Asteroid Hit the Earth? Day 2", "slug": ["smilodon"]}),
                json!({"name": c, "title": "Mammut"}),
            ];
            let (moves, errors) = plan(&root, &dl, &videos);
            assert_eq!(moves.len(), 1, "only the Mammut take is certain");
            assert_eq!(moves[0].video, c);
            assert_eq!(errors.len(), 3, "{errors:?}");
            assert!(errors.iter().all(|e| e["videos"] == json!([a, b])));
            let _ = fs::remove_dir_all(&base);
        }

        #[test]
        fn files_go_to_the_right_subfolder() {
            assert_eq!(sub_for("00-script.txt"), Some("01 Script"));
            assert_eq!(sub_for("outline.txt"), Some("01 Script"));
            assert_eq!(sub_for("script-check.txt"), Some("01 Script"));
            assert_eq!(sub_for("01-act-1-the-hunt.mp3"), Some("02 Narration"));
            assert_eq!(sub_for("subtitle.srt"), Some("02 Narration"));
            assert_eq!(sub_for("flow-prompts.txt"), Some("03 Images"));
            assert_eq!(sub_for("clip-1-2.mp4"), Some("04 Video clips"));
            assert_eq!(sub_for("infographics.txt"), Some("05 Infographics"));
            assert_eq!(sub_for("thumbnail.png"), Some("06 Thumbnail"));
            assert_eq!(sub_for("title-and-description.txt"), Some("07 Upload"));
            assert_eq!(sub_for("Edit.prproj"), Some("08 Premiere"));
            assert_eq!(sub_for("notes.bin"), None);
            assert!(is_act("01-act-1-the-hunt.mp3") && is_act("12-act-12.mp3"));
            assert!(!is_act("voice-over-1731480000.mp3") && !is_act("1-act-1.mp3"));
            assert!(is_take("voice-over-1731480000.mp3") && is_take("old.MP3") && !is_take("01-act-1.mp3") && !is_take("subtitle.srt"));
        }

        #[test]
        fn migration_matches_only_the_full_slug_with_a_timestamp() {
            assert_eq!(take_for("smilodon", "smilodon-1731480000.mp3"), Some(1731480000));
            assert_eq!(take_for("smilodon", "smilodon-the-cat-1731480000.mp3"), None);
            assert_eq!(take_for("smilodon", "smilodon-1731480000.mp3.part"), None);
            assert_eq!(take_for("smilodon", "smilodon-12.mp3"), None);
            assert_eq!(take_for("smilodon", "smilodons-1731480000.mp3"), None);
            assert_eq!(take_for("", "-1731480000.mp3"), None);

            let base = temp("migrate");
            let (dl, root) = (base.join("Downloads"), base.join("Downloads").join("Terra Studio"));
            fs::create_dir_all(root.join("smilodon")).unwrap();
            for (f, body) in [("00-script.txt", "s"), ("01-act-1-hunt.mp3", "a"), ("subtitle.srt", "x"), ("flow-prompts.txt", "f"),
                ("clip-1-1.mp4", "c"), ("infographics.txt", "i"), ("thumbnail.jpg", "t"), ("title-and-description.txt", "d")] {
                fs::write(root.join("smilodon").join(f), body).unwrap();
            }
            for f in ["smilodon-1731000000.mp3", "smilodon-1731480000.mp3", "smilodon-the-cat-1731490000.mp3",
                "smilodon-thumbnail-1.jpg", "unrelated.mp3"] {
                fs::write(dl.join(f), f).unwrap();
            }
            let a = "2026-11-20 Smilodon";
            let b = "2026-11-27 Smilodon the cat";
            let videos = vec![
                json!({"name": a, "title": "Smilodon", "slug": "smilodon", "id": "p1"}),
                json!({"name": b, "title": "Smilodon the cat", "slug": "smilodon-the-cat", "id": "p2"}),
                json!({"name": "../evil", "title": "x", "id": "p3"}),
            ];
            let (moves, errors) = plan(&root, &dl, &videos);
            assert_eq!(errors.len(), 1);
            let to = |from: &str| moves.iter().find(|m| m.from.ends_with(from)).map(|m| m.to.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/"));
            assert_eq!(to("00-script.txt").as_deref(), Some("2026-11-20 Smilodon/01 Script/00-script.txt"));
            assert_eq!(to("01-act-1-hunt.mp3").as_deref(), Some("2026-11-20 Smilodon/02 Narration/01-act-1-hunt.mp3"));
            assert_eq!(to("clip-1-1.mp4").as_deref(), Some("2026-11-20 Smilodon/04 Video clips/clip-1-1.mp4"));
            assert_eq!(to("title-and-description.txt").as_deref(), Some("2026-11-20 Smilodon/07 Upload/title-and-description.txt"));
            assert_eq!(to("smilodon-1731480000.mp3").as_deref(), Some("2026-11-20 Smilodon/02 Narration/smilodon-1731480000.mp3"));
            assert_eq!(to("smilodon-1731000000.mp3").as_deref(), Some("2026-11-20 Smilodon/02 Narration/older takes/smilodon-1731000000.mp3"));
            assert_eq!(to("smilodon-the-cat-1731490000.mp3").as_deref(), Some("2026-11-27 Smilodon the cat/02 Narration/smilodon-the-cat-1731490000.mp3"));
            assert_eq!(to("smilodon-thumbnail-1.jpg").as_deref(), Some("2026-11-20 Smilodon/06 Thumbnail/smilodon-thumbnail-1.jpg"));
            assert_eq!(to("unrelated.mp3"), None);
            assert_eq!(moves.len(), 12);
            // A dry run changes nothing.
            assert!(dl.join("smilodon-1731480000.mp3").is_file() && !root.join(a).exists());

            fs::create_dir_all(root.join(a).join("01 Script")).unwrap();
            fs::write(root.join(a).join("01 Script").join("00-script.txt"), "already here").unwrap();
            let out = run(&root, &moves);
            assert_eq!(out["failed"].as_array().unwrap().len(), 0, "{out}");
            assert_eq!(out["moved"].as_array().unwrap().len(), 12);
            assert_eq!(fs::read_to_string(root.join(a).join("01 Script").join("00-script.txt")).unwrap(), "already here");
            assert_eq!(fs::read_to_string(root.join(a).join("01 Script").join("00-script (2).txt")).unwrap(), "s");
            assert!(root.join("smilodon").is_dir() && dl.join("unrelated.mp3").is_file());
            for s in SUBS {
                assert!(root.join(a).join(s).is_dir());
            }
            let t = takes(&root.join(a), a);
            assert_eq!(t["inuse"], "smilodon-1731480000.mp3");
            assert_eq!(t["count"], 2);
            assert_eq!(t["takes"][1]["where"], "older");
            assert_eq!(t["takes"][0]["url"], "/api/audio/2026-11-20%20Smilodon/smilodon-1731480000.mp3");

            use_take(&root.join(a), "smilodon-1731000000.mp3").unwrap();
            let t = takes(&root.join(a), a);
            assert_eq!(t["inuse"], "smilodon-1731000000.mp3");
            assert!(root.join(a).join(NARR).join(OLDER).join("smilodon-1731480000.mp3").is_file());
            assert!(use_take(&root.join(a), "../smilodon-1731480000.mp3").is_err());
            assert!(use_take(&root.join(a), "01-act-1-hunt.mp3").is_err());
            assert_eq!(use_take(&root.join(a), "missing-1731480000.mp3").unwrap_err().0, 404);
            let _ = fs::remove_dir_all(&base);
        }
    }
}

// YouTube Studio data

/// First start on a PC: pick up files shipped next to the exe, so the user does not
/// have to paste the profile or import the Studio export by hand.
/// - profil-*.json fills an empty channel profile.
/// - *.zip / *.csv from a YouTube Studio export are imported when no data exists yet.
// Send to Premiere: the page works out every time; this checks the files it names and writes them
// into the video's 08 Premiere as an FCP7 XML sequence plus text files. Nothing is overwritten.
mod edit {
    use super::*;

    pub const FPS: i64 = 30;
    pub const LIB: &str = "Sound library";
    pub const GUIDE: &str = "edit-guide.xml";
    const TEXTS: [&str; 5] = ["shots.srt", "subtitles-youtube.srt", "sound-list.txt", "chapters.txt", "edit-checklist.txt"];
    const SOURCES: [&str; 3] = ["02 Narration", "04 Video clips", "05 Infographics"];
    const VIDEO_EXT: [&str; 3] = ["mov", "mp4", "m4v"];
    const AUDIO_EXT: [&str; 2] = ["mp3", "wav"];
    const MAX_ITEMS: usize = 3000;
    const MAX_SECS: f64 = 6.0 * 3600.0;

    type Fail = (u16, String);

    pub fn frames(sec: f64) -> i64 {
        (sec.max(0.0) * FPS as f64).round() as i64
    }

    /// Text for XML: the five entities, and control characters XML 1.0 does not allow are dropped.
    pub fn esc(s: &str) -> String {
        let mut o = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '&' => o.push_str("&amp;"),
                '<' => o.push_str("&lt;"),
                '>' => o.push_str("&gt;"),
                '"' => o.push_str("&quot;"),
                '\'' => o.push_str("&apos;"),
                '\t' | '\n' | '\r' => o.push(c),
                c if (c as u32) < 0x20 || c == '\u{FFFE}' || c == '\u{FFFF}' => {}
                c => o.push(c),
            }
        }
        o
    }

    /// The pathurl form Premiere writes on Windows: file://localhost/C%3a/Users/...%20...
    pub fn path_url(p: &str) -> String {
        let s = p.strip_prefix(r"\\?\").unwrap_or(p).replace('\\', "/");
        let b = s.as_bytes();
        let (drive, rest) = if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' { (Some(&s[..1]), &s[2..]) } else { (None, s.as_str()) };
        let enc: String = rest
            .bytes()
            .map(|c| if c.is_ascii_alphanumeric() || b"-_.~/".contains(&c) { (c as char).to_string() } else { format!("%{c:02X}") })
            .collect();
        match drive {
            Some(d) => format!("file://localhost/{}%3a{}", d.to_ascii_uppercase(), enc),
            None if enc.starts_with('/') => format!("file://localhost{enc}"),
            None => format!("file://localhost/{enc}"),
        }
    }

    pub enum Kind {
        Video,
        Audio { rate: u32, channels: u32 },
    }

    pub struct Media {
        pub name: String,
        pub url: String,
        pub frames: i64,
        pub kind: Kind,
    }

    pub struct Clip {
        pub media: usize,
        pub start: i64,
        pub end: i64,
        pub inp: i64,
    }

    pub struct Mark {
        pub name: String,
        pub comment: String,
        pub start: i64,
        pub end: Option<i64>,
    }

    pub struct Seq {
        pub name: String,
        pub frames: i64,
        pub media: Vec<Media>,
        pub video: Vec<Vec<Clip>>,
        pub audio: Vec<Vec<Clip>>,
        pub marks: Vec<Mark>,
    }

    fn rate() -> String {
        format!("<rate><timebase>{FPS}</timebase><ntsc>FALSE</ntsc></rate>")
    }

    fn video_chars() -> String {
        format!("<samplecharacteristics>{}<width>1920</width><height>1080</height><anamorphic>FALSE</anamorphic><pixelaspectratio>square</pixelaspectratio><fielddominance>none</fielddominance><colordepth>24</colordepth></samplecharacteristics>", rate())
    }

    /// An FCP7 xmeml (version 4) sequence, laid out the way Premiere Pro exports one.
    pub fn xml(s: &Seq) -> String {
        let mut len: Vec<i64> = s.media.iter().map(|m| m.frames.max(1)).collect();
        for c in s.video.iter().chain(s.audio.iter()).flatten() {
            len[c.media] = len[c.media].max(c.inp + c.end - c.start);
        }
        let mut written = vec![false; s.media.len()];
        let mut item = 0;
        let mut o = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE xmeml>\n<xmeml version=\"4\">\n");
        o += "\t<sequence id=\"sequence-1\">\n";
        o += &format!("\t\t<duration>{}</duration>\n\t\t{}\n\t\t<name>{}</name>\n\t\t<media>\n", s.frames.max(1), rate(), esc(&s.name));
        let mut clip = |o: &mut String, c: &Clip, audio: bool| {
            item += 1;
            let m = &s.media[c.media];
            *o += &format!("\t\t\t\t\t<clipitem id=\"clipitem-{item}\">\n");
            *o += &format!("\t\t\t\t\t\t<masterclipid>masterclip-{}</masterclipid>\n", c.media + 1);
            *o += &format!("\t\t\t\t\t\t<name>{}</name>\n\t\t\t\t\t\t<enabled>TRUE</enabled>\n", esc(&m.name));
            *o += &format!("\t\t\t\t\t\t<duration>{}</duration>\n\t\t\t\t\t\t{}\n", len[c.media], rate());
            *o += &format!("\t\t\t\t\t\t<start>{}</start>\n\t\t\t\t\t\t<end>{}</end>\n\t\t\t\t\t\t<in>{}</in>\n\t\t\t\t\t\t<out>{}</out>\n", c.start, c.end, c.inp, c.inp + c.end - c.start);
            if !audio {
                *o += "\t\t\t\t\t\t<pixelaspectratio>square</pixelaspectratio>\n\t\t\t\t\t\t<anamorphic>FALSE</anamorphic>\n";
            }
            if written[c.media] {
                *o += &format!("\t\t\t\t\t\t<file id=\"file-{}\"/>\n", c.media + 1);
            } else {
                written[c.media] = true;
                *o += &format!("\t\t\t\t\t\t<file id=\"file-{}\">\n", c.media + 1);
                *o += &format!("\t\t\t\t\t\t\t<name>{}</name>\n\t\t\t\t\t\t\t<pathurl>{}</pathurl>\n", esc(&m.name), esc(&m.url));
                *o += &format!("\t\t\t\t\t\t\t{}\n\t\t\t\t\t\t\t<duration>{}</duration>\n\t\t\t\t\t\t\t<media>\n", rate(), len[c.media]);
                match m.kind {
                    Kind::Video => *o += &format!("\t\t\t\t\t\t\t\t<video>{}</video>\n", video_chars()),
                    Kind::Audio { rate: r, channels } => {
                        *o += &format!("\t\t\t\t\t\t\t\t<audio><samplecharacteristics><depth>16</depth><samplerate>{r}</samplerate></samplecharacteristics><channelcount>{channels}</channelcount></audio>\n")
                    }
                }
                *o += "\t\t\t\t\t\t\t</media>\n\t\t\t\t\t\t</file>\n";
            }
            if audio {
                *o += "\t\t\t\t\t\t<sourcetrack><mediatype>audio</mediatype><trackindex>1</trackindex></sourcetrack>\n";
            }
            *o += "\t\t\t\t\t</clipitem>\n";
        };
        o += &format!("\t\t\t<video>\n\t\t\t\t<format>{}</format>\n", video_chars());
        for t in &s.video {
            o += "\t\t\t\t<track>\n";
            for c in t {
                clip(&mut o, c, false);
            }
            o += "\t\t\t\t\t<enabled>TRUE</enabled>\n\t\t\t\t\t<locked>FALSE</locked>\n\t\t\t\t</track>\n";
        }
        o += "\t\t\t</video>\n\t\t\t<audio>\n\t\t\t\t<numOutputChannels>2</numOutputChannels>\n";
        o += "\t\t\t\t<format><samplecharacteristics><depth>16</depth><samplerate>48000</samplerate></samplecharacteristics></format>\n";
        o += "\t\t\t\t<outputs>";
        for i in 1..=2 {
            o += &format!("<group><index>{i}</index><numchannels>1</numchannels><downmix>0</downmix><channel><index>{i}</index></channel></group>");
        }
        o += "</outputs>\n";
        for t in &s.audio {
            o += "\t\t\t\t<track>\n";
            for c in t {
                clip(&mut o, c, true);
            }
            o += "\t\t\t\t\t<enabled>TRUE</enabled>\n\t\t\t\t\t<locked>FALSE</locked>\n\t\t\t\t</track>\n";
        }
        o += "\t\t\t</audio>\n\t\t</media>\n";
        o += &format!("\t\t<timecode>\n\t\t\t{}\n\t\t\t<string>00:00:00:00</string>\n\t\t\t<frame>0</frame>\n\t\t\t<displayformat>NDF</displayformat>\n\t\t</timecode>\n", rate());
        for m in &s.marks {
            o += &format!(
                "\t\t<marker>\n\t\t\t<comment>{}</comment>\n\t\t\t<name>{}</name>\n\t\t\t<in>{}</in>\n\t\t\t<out>{}</out>\n\t\t</marker>\n",
                esc(&m.comment),
                esc(&m.name),
                m.start,
                m.end.filter(|e| *e > m.start).unwrap_or(-1)
            );
        }
        o += "\t</sequence>\n</xmeml>\n";
        o
    }

    /// Sample rate and channels from a WAV header, with its length in seconds.
    pub fn wav_info(b: &[u8], total: u64) -> Option<(f64, u32, u32)> {
        if b.len() < 12 || &b[..4] != b"RIFF" || &b[8..12] != b"WAVE" {
            return None;
        }
        let (mut i, mut fmt) = (12usize, None);
        while i + 8 <= b.len() {
            let id = &b[i..i + 4];
            let size = u32::from_le_bytes(b[i + 4..i + 8].try_into().ok()?) as u64;
            if id == b"fmt " && i + 24 <= b.len() {
                let ch = u16::from_le_bytes([b[i + 10], b[i + 11]]) as u32;
                let rate = u32::from_le_bytes(b[i + 12..i + 16].try_into().ok()?);
                let bps = u32::from_le_bytes(b[i + 16..i + 20].try_into().ok()?);
                fmt = Some((ch, rate, bps));
            } else if id == b"data" {
                let (ch, rate, bps) = fmt?;
                if bps == 0 || ch == 0 {
                    return None;
                }
                let data = if size == 0xFFFF_FFFF || size == 0 { total.saturating_sub(i as u64 + 8) } else { size.min(total.saturating_sub(i as u64 + 8)) };
                return Some((data as f64 / bps as f64, rate, ch));
            }
            i += 8 + size as usize + (size as usize & 1);
        }
        None
    }

    /// Length, sample rate and channels of a sound file, read from its header.
    pub fn sound_info(p: &Path) -> Option<(f64, u32, u32)> {
        let total = fs::metadata(p).ok()?.len();
        let mut head = Vec::new();
        fs::File::open(p).ok()?.take(70_000).read_to_end(&mut head).ok()?;
        match ext(&p.to_string_lossy()).as_str() {
            "wav" => wav_info(&head, total),
            "mp3" => eleven::mp3_seconds_len(&head, total as usize).map(|s| (s, 44100, 2)),
            _ => None,
        }
        .filter(|(s, _, _)| s.is_finite() && *s > 0.05)
    }

    fn ext(f: &str) -> String {
        f.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
    }

    fn is_file(p: &Path) -> bool {
        fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_file())
    }

    /// Every function here takes the pack root (Downloads\Terra Studio), so tests never touch real folders.
    pub fn library(root: &Path) -> PathBuf {
        root.join(LIB)
    }

    fn video_dir(root: &Path, v: &Value) -> Result<(String, PathBuf), Fail> {
        let name = v["folder"].as_str().unwrap_or("");
        if !video::valid_name(name) {
            return Err((400, "The video folder name is not valid.".into()));
        }
        let d = root.join(name);
        if !d.is_dir() {
            return Err((404, "The video folder was not found.".into()));
        }
        Ok((name.to_string(), d))
    }

    /// The files the Editing page matches against: sources in the video folder and the shared sound library.
    pub fn scan(root: &Path, v: &Value) -> Result<Value, Fail> {
        let (name, d) = video_dir(root, v)?;
        let list = |sub: &str| video::files(&d.join(sub));
        let lib = library(root);
        let sounds: Vec<Value> = video::files(&lib)
            .into_iter()
            .filter(|f| AUDIO_EXT.contains(&ext(f).as_str()))
            .take(400)
            .filter_map(|f| sound_info(&lib.join(&f)).map(|(s, _, _)| json!({"file": f, "seconds": (s * 1000.0).round() / 1000.0})))
            .collect();
        Ok(json!({"folder": name, "dir": d.display().to_string(), "narration": list(video::NARR), "clips": list(video::SUBS[3]),
            "infographics": list(video::SUBS[4]), "premiere": list(video::SUBS[7]), "images": list(video::SUBS[2]), "upload": list(video::SUBS[6]), "sounds": sounds,
            "library": lib.display().to_string(), "library_exists": lib.is_dir()}))
    }

    fn num(v: &Value, k: &str) -> Result<f64, Fail> {
        v[k].as_f64().filter(|x| x.is_finite() && *x >= 0.0 && *x <= MAX_SECS).ok_or((400, format!("A time in the edit guide is not valid ({k}).")))
    }

    fn text(v: &Value, k: &str, max: usize) -> String {
        v[k].as_str().unwrap_or("").chars().take(max).collect()
    }

    /// Builds the sequence from the page's timeline, refusing any file outside the allowed folders.
    pub fn sequence(root: &Path, d: &Path, v: &Value) -> Result<Seq, Fail> {
        let mut media: Vec<Media> = Vec::new();
        let mut seen: Vec<PathBuf> = Vec::new();
        let mut count = 0;
        let mut track = |key: &str, video: bool| -> Result<Vec<Clip>, Fail> {
            let mut out = Vec::new();
            let empty = Vec::new();
            for it in v["tracks"][key].as_array().unwrap_or(&empty) {
                count += 1;
                if count > MAX_ITEMS {
                    return Err((400, "The edit guide has too many clips.".into()));
                }
                let file = it["file"].as_str().unwrap_or("");
                let ok_ext = if video { VIDEO_EXT.contains(&ext(file).as_str()) } else { AUDIO_EXT.contains(&ext(file).as_str()) };
                if !video::valid_file(file) || !ok_ext {
                    return Err((400, format!("This file cannot go in the edit guide: {}", file.chars().take(80).collect::<String>())));
                }
                let (dir, place) = if it["lib"] == true {
                    (library(root), LIB.to_string())
                } else {
                    let sub = it["sub"].as_str().unwrap_or("");
                    if !SOURCES.contains(&sub) {
                        return Err((400, "The edit guide names a folder it may not use.".into()));
                    }
                    (d.join(sub), sub.to_string())
                };
                let path = dir.join(file);
                if !is_file(&path) {
                    return Err((404, format!("{file} is not in {place}. Open the Editing tab again so it can look once more.")));
                }
                let (start, end, inp, secs) = (num(it, "start")?, num(it, "end")?, num(it, "in")?, num(it, "seconds")?);
                let (s, e) = (frames(start), frames(end));
                if e <= s {
                    continue;
                }
                let idx = match seen.iter().position(|p| *p == path) {
                    Some(i) => i,
                    None => {
                        let kind = if video {
                            Kind::Video
                        } else if place == video::NARR {
                            Kind::Audio { rate: 44100, channels: 1 }
                        } else {
                            let (_, rate, channels) = sound_info(&path).unwrap_or((0.0, 48000, 2));
                            Kind::Audio { rate, channels }
                        };
                        seen.push(path.clone());
                        media.push(Media { name: file.to_string(), url: path_url(&path.display().to_string()), frames: frames(secs), kind });
                        media.len() - 1
                    }
                };
                out.push(Clip { media: idx, start: s, end: e, inp: frames(inp) });
            }
            out.sort_by_key(|c| c.start);
            Ok(out)
        };
        let video = vec![track("V1", true)?, track("V2", true)?, track("V3", true)?];
        let audio = vec![track("A1", false)?, track("A2", false)?, track("A3", false)?, track("A4", false)?];
        let empty = Vec::new();
        let marks_in = v["markers"].as_array().unwrap_or(&empty);
        if marks_in.len() > MAX_ITEMS {
            return Err((400, "The edit guide has too many markers.".into()));
        }
        let mut marks = Vec::new();
        for m in marks_in {
            let start = frames(num(m, "start")?);
            let end = if m["end"].is_null() { None } else { Some(frames(num(m, "end")?)) };
            marks.push(Mark { name: text(m, "name", 80), comment: text(m, "comment", 1500), start, end });
        }
        marks.sort_by_key(|m| m.start);
        let last = video.iter().chain(audio.iter()).flatten().map(|c| c.end).chain(marks.iter().map(|m| m.end.unwrap_or(m.start))).max().unwrap_or(0);
        Ok(Seq { name: text(v, "title", 200), frames: frames(num(v, "seconds")?).max(last), media, video, audio, marks })
    }

    /// Writes edit-guide.xml and the text files into 08 Premiere, each under a free name.
    pub fn premiere(root: &Path, v: &Value) -> Result<Value, Fail> {
        let (_, d) = video_dir(root, v)?;
        let mut texts: Vec<(&str, String)> = Vec::new();
        for t in TEXTS {
            if let Some(s) = v["files"][t].as_str() {
                texts.push((t, s.replace("\r\n", "\n").replace('\n', "\r\n")));
            }
        }
        let seq = sequence(root, &d, v)?;
        let out_dir = d.join(video::SUBS[7]);
        fs::create_dir_all(&out_dir).map_err(|e| (500, format!("Could not open 08 Premiere: {}", video::why(&e))))?;
        let lib = library(root);
        let made_lib = !lib.is_dir() && fs::create_dir_all(&lib).is_ok();
        let jobs: Vec<(&str, String, PathBuf)> = std::iter::once((GUIDE, xml(&seq))).chain(texts)
            .map(|(f, b)| { let p = video::free_path(&out_dir, f, |p| p.exists()); (f, b, p) }).collect();
        let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        // The text files name "{edit-guide.xml}" etc.; they get the free name each file really gets.
        let names: Vec<(String, String)> = jobs.iter().map(|(f, _, p)| (format!("{{{f}}}"), name(p))).collect();
        let mut written = Vec::new();
        for (i, (file, mut body, p)) in jobs.into_iter().enumerate() {
            if i > 0 {
                for (k, n) in &names {
                    body = body.replace(k.as_str(), n);
                }
            }
            write_atomic(&p, body.as_bytes()).map_err(|e| (500, format!("Could not save {file}: {}", video::why(&e))))?;
            written.push(name(&p));
        }
        let clips: usize = seq.video.iter().chain(seq.audio.iter()).map(Vec::len).sum();
        Ok(json!({"dir": out_dir.display().to_string(), "files": written, "guide": written[0], "clips": clips, "markers": seq.marks.len(),
            "library": lib.display().to_string(), "library_made": made_lib}))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A small XML checker: balanced tags, quoted attributes, known entities, no stray '<' or '&'.
        fn well_formed(x: &str) -> Result<usize, String> {
            let b = x.as_bytes();
            let mut stack: Vec<String> = Vec::new();
            let (mut i, mut tags) = (0, 0);
            let entity_ok = |s: &str| {
                let e = s.split(';').next().unwrap_or("");
                s.contains(';') && (["amp", "lt", "gt", "quot", "apos"].contains(&e) || (e.starts_with('#') && e[1..].chars().all(|c| c.is_ascii_digit())))
            };
            while i < b.len() {
                if b[i] == b'<' {
                    let end = x[i..].find('>').ok_or("unclosed tag")? + i;
                    let tag = &x[i + 1..end];
                    i = end + 1;
                    if tag.starts_with('?') || tag.starts_with('!') {
                        continue;
                    }
                    tags += 1;
                    if let Some(name) = tag.strip_prefix('/') {
                        let open = stack.pop().ok_or(format!("close without open: {name}"))?;
                        if open != name {
                            return Err(format!("<{open}> closed by </{name}>"));
                        }
                        continue;
                    }
                    let self_close = tag.ends_with('/');
                    let body = tag.trim_end_matches('/');
                    let mut parts = body.splitn(2, char::is_whitespace);
                    let name = parts.next().unwrap_or("").to_string();
                    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                        return Err(format!("bad tag name {name:?}"));
                    }
                    let mut attrs = parts.next().unwrap_or("").trim();
                    while !attrs.is_empty() {
                        let (k, rest) = attrs.split_once('=').ok_or("attribute without value")?;
                        if !k.trim().chars().all(|c| c.is_ascii_alphanumeric()) || !rest.starts_with('"') {
                            return Err(format!("bad attribute in {name}"));
                        }
                        let close = rest[1..].find('"').ok_or("unquoted attribute")? + 1;
                        if rest[1..close].contains('<') {
                            return Err("'<' in attribute".into());
                        }
                        attrs = rest[close + 1..].trim();
                    }
                    if !self_close {
                        stack.push(name);
                    }
                } else {
                    if b[i] == b'&' && !entity_ok(&x[i + 1..x.len().min(i + 12)]) {
                        return Err(format!("bare & at {i}"));
                    }
                    if b[i] == b'>' {
                        return Err(format!("stray > at {i}"));
                    }
                    i += 1;
                }
            }
            if stack.is_empty() { Ok(tags) } else { Err(format!("unclosed: {stack:?}")) }
        }

        /// Text between the first <tag> and </tag> after position `from`.
        fn inner<'a>(x: &'a str, tag: &str, from: usize) -> Option<(&'a str, usize)> {
            let a = x[from..].find(&format!("<{tag}>"))? + from + tag.len() + 2;
            let b = x[a..].find(&format!("</{tag}>"))? + a;
            Some((&x[a..b], b))
        }

        #[test]
        fn frames_round_from_cumulative_seconds() {
            assert_eq!(frames(0.0), 0);
            assert_eq!(frames(1.0), 30);
            assert_eq!(frames(158.0161), 4740);
            assert_eq!(frames(159.0161), 4770);
            assert_eq!(frames(0.0166), 0);
            assert_eq!(frames(0.0167), 1);
            assert_eq!(frames(-3.0), 0);
            // Acts of 158.4 s and 172.7 s with 1 s pauses: each start comes from the running total, not from rounded lengths.
            let lens = [158.4, 172.7, 189.35, 174.05, 157.3];
            let (mut t, mut starts) = (0.0, vec![]);
            for l in lens {
                starts.push(frames(t));
                t += l + 1.0;
            }
            assert_eq!(starts, [0, 4782, 9993, 15704, 20955]);
        }

        #[test]
        fn path_urls_match_premiere_on_windows() {
            assert_eq!(
                path_url(r"C:\Users\Sigit\Downloads\Terra Studio\2026-11-20 The Den\02 Narration\01-act-1-the-den.mp3"),
                "file://localhost/C%3a/Users/Sigit/Downloads/Terra%20Studio/2026-11-20%20The%20Den/02%20Narration/01-act-1-the-den.mp3"
            );
            assert_eq!(path_url(r"d:\a b\c.mov"), "file://localhost/D%3a/a%20b/c.mov");
            assert_eq!(path_url(r"\\?\C:\x\y (2).mp4"), "file://localhost/C%3a/x/y%20%282%29.mp4");
            assert_eq!(path_url("C:\\Hy\u{e9}ne & 'Den'\\x.mp3"), "file://localhost/C%3a/Hy%C3%A9ne%20%26%20%27Den%27/x.mp3");
            assert_eq!(path_url("/home/a b/x.mp3"), "file://localhost/home/a%20b/x.mp3");
        }

        #[test]
        fn titles_are_escaped() {
            assert_eq!(esc(r#"Bone & Blood <Act 1> "Den" 'Gone'"#), "Bone &amp; Blood &lt;Act 1&gt; &quot;Den&quot; &apos;Gone&apos;");
            assert_eq!(esc("a\u{0}b\u{1b}c\td"), "abc\td");
        }

        fn sample() -> Seq {
            let a = |n: &str, f| Media { name: n.into(), url: path_url(&format!(r"C:\T S\{n}")), frames: f, kind: Kind::Audio { rate: 44100, channels: 1 } };
            Seq {
                name: r#"The Hyena & the "Cave" <Den> it's"#.into(),
                frames: frames(14.0 * 60.0 + 15.0),
                media: vec![
                    a("01-act-1.mp3", 4740),
                    a("02-act-2.mp3", 5190),
                    Media { name: "clip-1-1.mp4".into(), url: path_url(r"C:\T S\clip-1-1.mp4"), frames: 240, kind: Kind::Video },
                    Media { name: "01-size-comparison.mov".into(), url: path_url(r"C:\T S\01-size-comparison.mov"), frames: 180, kind: Kind::Video },
                    Media { name: "whoosh & co.wav".into(), url: path_url(r"C:\L\whoosh & co.wav"), frames: 20, kind: Kind::Audio { rate: 48000, channels: 2 } },
                ],
                video: vec![vec![Clip { media: 2, start: 0, end: 150, inp: 0 }], vec![Clip { media: 3, start: 1230, end: 1410, inp: 0 }], vec![]],
                audio: vec![
                    vec![Clip { media: 0, start: 0, end: 4740, inp: 0 }, Clip { media: 1, start: 4770, end: 9960, inp: 0 }],
                    vec![Clip { media: 4, start: 1226, end: 1246, inp: 0 }, Clip { media: 4, start: 4770, end: 4790, inp: 0 }],
                    vec![],
                    vec![],
                ],
                marks: vec![
                    Mark { name: "SFX".into(), comment: "Bone crack on \"snap\" / A2 / R9 & R13".into(), start: 2112, end: None },
                    Mark { name: "WEAK SPOT".into(), comment: "132 words <no number>".into(), start: 3000, end: Some(4530) },
                ],
            }
        }

        #[test]
        fn xml_is_well_formed_and_shaped_like_premiere_xmeml() {
            let x = xml(&sample());
            let tags = well_formed(&x).unwrap_or_else(|e| panic!("{e}\n{x}"));
            assert!(tags > 50);
            assert!(x.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE xmeml>\n<xmeml version=\"4\">"));
            assert!(x.contains("<name>The Hyena &amp; the &quot;Cave&quot; &lt;Den&gt; it&apos;s</name>"));
            assert_eq!(inner(&x, "duration", 0).unwrap().0, "25650");
            assert!(x.contains("<rate><timebase>30</timebase><ntsc>FALSE</ntsc></rate>"));
            assert!(x.contains("<width>1920</width><height>1080</height>"));
            assert_eq!(x.matches("<track>").count(), 7, "V1-V3 and A1-A4");
            assert_eq!(x.matches("<clipitem ").count(), 6);
            // Every clipitem names its file and has start, end, in, out and a duration.
            for part in x.split("<clipitem ").skip(1) {
                let ci = &part[..part.find("</clipitem>").unwrap()];
                for t in ["<masterclipid>", "<duration>", "<start>", "<end>", "<in>", "<out>", "<file id=\"file-"] {
                    assert!(ci.contains(t), "{t} missing in {ci}");
                }
                let n = |t| inner(ci, t, 0).unwrap().0.parse::<i64>().unwrap();
                assert_eq!(n("out") - n("in"), n("end") - n("start"));
                assert!(n("out") <= n("duration"));
            }
            // A file is written once, then referenced by id like Premiere does.
            assert_eq!(x.matches("<file id=\"file-5\">").count(), 1);
            assert_eq!(x.matches("<file id=\"file-5\"/>").count(), 1);
            assert_eq!(x.matches("<pathurl>").count(), 5);
            assert!(x.contains("<pathurl>file://localhost/C%3a/T%20S/01-act-1.mp3</pathurl>"));
            assert!(x.contains("<pathurl>file://localhost/C%3a/L/whoosh%20%26%20co.wav</pathurl>"));
            assert!(x.contains("<channelcount>1</channelcount>"));
            assert!(x.contains("<sourcetrack><mediatype>audio</mediatype><trackindex>1</trackindex></sourcetrack>"));
            // Video clips carry only a video stream, so their sound is not placed.
            let clip = &x[x.find("<name>clip-1-1.mp4</name>").unwrap()..];
            let clip = &clip[..clip.find("</clipitem>").unwrap()];
            assert!(clip.contains("<video>") && !clip.contains("<audio>"));
            assert_eq!(x.matches("<marker>").count(), 2);
            assert!(x.contains("<comment>Bone crack on &quot;snap&quot; / A2 / R9 &amp; R13</comment>"));
            assert!(x.contains("<in>2112</in>\n\t\t\t<out>-1</out>"));
            assert!(x.contains("<in>3000</in>\n\t\t\t<out>4530</out>"));
            assert!(x.find("</media>").unwrap() < x.find("<timecode>").unwrap() && x.find("<timecode>").unwrap() < x.find("<marker>").unwrap());
            assert!(well_formed("<a><b></a></b>").is_err() && well_formed("<a>x & y</a>").is_err() && well_formed("<a x=1></a>").is_err());
        }

        #[test]
        fn wav_headers_give_length_and_format() {
            let mut w = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
            w.extend(16u32.to_le_bytes());
            w.extend(1u16.to_le_bytes());
            w.extend(2u16.to_le_bytes());
            w.extend(48000u32.to_le_bytes());
            w.extend((48000u32 * 4).to_le_bytes());
            w.extend(4u16.to_le_bytes());
            w.extend(16u16.to_le_bytes());
            w.extend(b"data");
            w.extend((48000u32 * 4 * 3).to_le_bytes());
            let (s, rate, ch) = wav_info(&w, w.len() as u64 + 48000 * 12).unwrap();
            assert!((s - 3.0).abs() < 1e-9);
            assert_eq!((rate, ch), (48000, 2));
            assert!(wav_info(b"RIFF\0\0\0\0AVI ", 100).is_none());
        }

        #[test]
        fn premiere_files_stay_in_the_video_folder_and_never_overwrite() {
            let base = std::env::temp_dir().join(format!("terra-edit-{}", std::process::id()));
            let _ = fs::remove_dir_all(&base);
            let root = base.join("Terra Studio");
            let app = root.as_path();
            let name = "2026-11-20 Hyena & Den";
            let d = root.join(name);
            for s in video::SUBS {
                fs::create_dir_all(d.join(s)).unwrap();
            }
            fs::write(d.join("02 Narration").join("01-act-1-the-den.mp3"), [0xFF, 0xFB, 0x90, 0x00]).unwrap();
            fs::write(d.join("05 Infographics").join("01-size-comparison.mov"), b"x").unwrap();
            fs::write(d.join("08 Premiere").join("edit-guide.xml"), b"the owner's own file").unwrap();
            fs::write(d.join("08 Premiere").join("shots.srt"), b"keep").unwrap();
            let body = |tracks: Value| json!({"folder": name, "title": "Hyena & \"Den\"", "seconds": 20.0, "tracks": tracks,
                "markers": [{"name": "SFX", "comment": "snap / A2 / R9", "start": 3.2}, {"name": "END SCREEN", "comment": "x", "start": 0.0, "end": 20.0}],
                "files": {"shots.srt": "1\n00:00:00,000 --> 00:00:04,000\nShot\n", "chapters.txt": "00:00 The Den\n", "evil.txt": "no",
                    "edit-checklist.txt": "Import {edit-guide.xml}, upload {subtitles-youtube.srt}\n"}});
            let good = json!({"A1": [{"sub": "02 Narration", "file": "01-act-1-the-den.mp3", "start": 0.0, "end": 9.5, "in": 0.0, "seconds": 9.5}],
                "V2": [{"sub": "05 Infographics", "file": "01-size-comparison.mov", "start": 2.0, "end": 8.0, "in": 0.0, "seconds": 6.0}]});
            let out = premiere(app, &body(good.clone())).unwrap();
            assert_eq!(out["files"], json!(["edit-guide (2).xml", "shots (2).srt", "chapters.txt", "edit-checklist.txt"]));
            assert_eq!(fs::read_to_string(d.join("08 Premiere").join("edit-checklist.txt")).unwrap(), "Import edit-guide (2).xml, upload {subtitles-youtube.srt}\r\n");
            assert_eq!(fs::read(d.join("08 Premiere").join("edit-guide.xml")).unwrap(), b"the owner's own file");
            assert_eq!(fs::read(d.join("08 Premiere").join("shots.srt")).unwrap(), b"keep");
            assert!(!d.join("08 Premiere").join("evil.txt").exists());
            assert_eq!(fs::read_to_string(d.join("08 Premiere").join("chapters.txt")).unwrap(), "00:00 The Den\r\n");
            assert!(library(app).is_dir() && out["library_made"] == true);
            let x = fs::read_to_string(d.join("08 Premiere").join("edit-guide (2).xml")).unwrap();
            well_formed(&x).unwrap();
            assert!(x.contains("Hyena%20%26%20Den/02%20Narration/01-act-1-the-den.mp3</pathurl>"));
            assert!(x.contains("<start>60</start>") && x.contains("<end>240</end>"));
            assert_eq!(x.matches("<marker>").count(), 2);
            let again = premiere(app, &body(good)).unwrap();
            assert_eq!(again["guide"], "edit-guide (3).xml");
            assert!(fs::read_to_string(d.join("08 Premiere").join("edit-checklist (2).txt")).unwrap().starts_with("Import edit-guide (3).xml,"));
            assert_eq!(again["library_made"], false);

            // Paths, other folders, wrong types and missing files are refused before anything is written.
            let bad = [
                json!({"A1": [{"sub": "02 Narration", "file": "../../x.mp3", "start": 0.0, "end": 1.0, "in": 0.0, "seconds": 1.0}]}),
                json!({"A1": [{"sub": "08 Premiere", "file": "edit-guide.xml", "start": 0.0, "end": 1.0, "in": 0.0, "seconds": 1.0}]}),
                json!({"A1": [{"sub": "..", "file": "01-act-1-the-den.mp3", "start": 0.0, "end": 1.0, "in": 0.0, "seconds": 1.0}]}),
                json!({"V1": [{"sub": "02 Narration", "file": "01-act-1-the-den.mp3", "start": 0.0, "end": 1.0, "in": 0.0, "seconds": 1.0}]}),
                json!({"A1": [{"sub": "02 Narration", "file": "02-act-2.mp3", "start": 0.0, "end": 1.0, "in": 0.0, "seconds": 1.0}]}),
                json!({"A2": [{"lib": true, "file": "growl.wav", "start": 0.0, "end": 1.0, "in": 0.0, "seconds": 1.0}]}),
                json!({"A1": [{"sub": "02 Narration", "file": "01-act-1-the-den.mp3", "start": f64::NAN, "end": 1.0, "in": 0.0, "seconds": 1.0}]}),
            ];
            let before = video::files(&d.join("08 Premiere")).len();
            for t in bad {
                assert!(premiere(app, &body(t.clone())).is_err(), "{t}");
            }
            assert_eq!(video::files(&d.join("08 Premiere")).len(), before);
            assert!(premiere(app, &json!({"folder": "../x", "seconds": 1.0})).is_err());
            assert!(premiere(app, &json!({"folder": "2026-01-01 Missing", "seconds": 1.0})).is_err());

            // A file from the sound library is placed with its real format.
            let mut w = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
            for x in [16u32.to_le_bytes().to_vec(), 1u16.to_le_bytes().to_vec(), 2u16.to_le_bytes().to_vec(), 48000u32.to_le_bytes().to_vec(),
                192000u32.to_le_bytes().to_vec(), 4u16.to_le_bytes().to_vec(), 16u16.to_le_bytes().to_vec(), b"data".to_vec(), 192000u32.to_le_bytes().to_vec()] {
                w.extend(x);
            }
            w.extend(vec![0u8; 192000]);
            fs::write(library(app).join("low growl.wav"), &w).unwrap();
            let s = scan(app, &json!({"folder": name})).unwrap();
            assert_eq!(s["sounds"], json!([{"file": "low growl.wav", "seconds": 1.0}]));
            assert_eq!(s["infographics"], json!(["01-size-comparison.mov"]));
            let lib = json!({"A2": [{"lib": true, "file": "low growl.wav", "start": 4.0, "end": 5.0, "in": 0.0, "seconds": 1.0}]});
            let out = premiere(app, &body(lib)).unwrap();
            let x = fs::read_to_string(d.join("08 Premiere").join(out["guide"].as_str().unwrap())).unwrap();
            assert!(x.contains("Sound%20library/low%20growl.wav</pathurl>") && x.contains("<samplerate>48000</samplerate></samplecharacteristics><channelcount>2</channelcount>"));
            let _ = fs::remove_dir_all(&base);
        }
    }
}

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
            .map_err(|_| "The .zip file could not be opened.".to_string())?;
        for i in 0..zip.len() {
            let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
            if !f.name().to_lowercase().ends_with(".csv") {
                continue;
            }
            let mut raw = Vec::new();
            (&mut f).take(MAX_CSV + 1).read_to_end(&mut raw).map_err(|e| e.to_string())?;
            if raw.len() as u64 > MAX_CSV {
                return Err("The contents of the .zip file are too large.".into());
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
    write_atomic(&dir.join("data.json"), &bytes).map_err(|e| format!("Could not save the data: {e}"))?;
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
            Some(format!("daily views ({n} days)"))
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
                Some("geography".into())
            } else {
                data["traffic"] = json!(list);
                Some("traffic sources".into())
            }
        }
        f if f.starts_with("viewer age") || f.starts_with("viewer gender") => {
            data["age"] = json!({"headers": headers, "rows": rows});
            Some("age and gender".into())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_inbox_goes_to_front_of_ideas_once() {
        let dir = std::env::temp_dir().join(format!("terra-inbox-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("claude-inbox.json"), r#"{"ideas":[{"title":"Epicyon: Biggest Dog","subject":"Epicyon"},{"title":"Old one","subject":"X"}]}"#).unwrap();
        let mut all = json!({"ideas":{"at":1,"data":{"ideas":[{"title":"Old one","subject":"X"}],"want":1}}});
        assert_eq!(take_claude_inbox(&dir, &mut all), 1);
        assert_eq!(all["ideas"]["data"]["ideas"][0]["title"], "Epicyon: Biggest Dog");
        assert_eq!(all["ideas"]["data"]["want"], 2);
        assert!(!dir.join("claude-inbox.json").exists());
        assert_eq!(take_claude_inbox(&dir, &mut all), 0);
        let _ = fs::remove_dir_all(&dir);
    }

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
    fn base64_decodes_audio_payloads() {
        assert_eq!(base64_decode("SUQzBAAA").as_deref(), Some(&b"ID3\x04\x00\x00"[..]));
        assert_eq!(base64_decode("//uQ").as_deref(), Some(&[0xFF, 0xFB, 0x90][..]));
        assert_eq!(base64_decode("TQ==").as_deref(), Some(&b"M"[..]));
        assert_eq!(base64_decode("TWE=").as_deref(), Some(&b"Ma"[..]));
        assert_eq!(base64_decode("TW\r\nFu").as_deref(), Some(&b"Man"[..]));
        assert_eq!(base64_decode("_-8").as_deref(), Some(&[0xFF, 0xEF][..]));
        assert_eq!(base64_decode("").as_deref(), Some(&b""[..]));
        assert_eq!(base64_decode("a*b"), None);
    }

    #[test]
    fn kie_key_survives_save_and_load() {
        let dir = std::env::temp_dir().join(format!("ts-kie-{}-{}", now(), std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut c = Config::default();
        c.kie_key = "kie-secret".into();
        c.image_provider = "kie".into();
        save_config(&dir, &c).unwrap();
        let back = load_config(&dir);
        assert_eq!((back.kie_key.as_str(), back.image_provider.as_str()), ("kie-secret", "kie"));
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
    fn audio_urls_cannot_leave_the_narration_folders() {
        let base = std::env::temp_dir().join(format!("terra-test-audio-{}", std::process::id()));
        let app = base.as_path();
        for bad in ["..%2F..%2Fsecret.mp3", "../x.mp3", "%2e%2e/x.mp3", "a/b/c.mp3", "x.wav", "2026-11-20%20Smilodon/..%2Fx.mp3",
            "..%5Cx.mp3", "2026-11-20%20Smilodon/x.mp3%3Aevil", "x.mp3%00", "", "/x.mp3"] {
            assert_eq!(audio_path(app, bad), None, "{bad:?}");
        }
        assert!(audio_path(app, "smilodon-1731480000.mp3").is_some_and(|p| p.ends_with("smilodon-1731480000.mp3")));

        let dl = base.join("Downloads");
        let narr = pack::root_in(&dl).join("2026-11-20 Smilodon").join(video::NARR);
        fs::create_dir_all(narr.join(video::OLDER)).unwrap();
        fs::write(narr.join(video::OLDER).join("x-1731480000.mp3"), "old").unwrap();
        fs::write(narr.join("voice-over-1731490000.mp3"), "new").unwrap();
        assert_eq!(audio_in(&dl, "2026-11-20%20Smilodon/x-1731480000.mp3"), Some(narr.join(video::OLDER).join("x-1731480000.mp3")));
        assert_eq!(audio_in(&dl, "2026-11-20%20Smilodon/voice-over-1731490000.mp3"), Some(narr.join("voice-over-1731490000.mp3")));
        assert_eq!(audio_in(&dl, "2026-11-20%20Smilodon/missing-1731480000.mp3"), None);
        assert_eq!(audio_in(&dl, "2026-11-20%20Smilodon%2F..%2F..%2F/x-1731480000.mp3"), None);
        assert_eq!(audio_in(&dl, "smilodon-1731480000.mp3"), Some(dl.join("smilodon-1731480000.mp3")));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn video_thumbnails_stay_in_their_folder() {
        let base = std::env::temp_dir().join(format!("terra-test-vthumb-{}", std::process::id()));
        let dl = base.join("Downloads");
        let tdir = pack::root_in(&dl).join("2026-11-20 Smilodon").join(video::SUBS[5]);
        for bad in ["..%2F..%2Fsecret.jpg", "../x.jpg", "%2e%2e/x.jpg", "2026-11-20%20Smilodon/..%2Fx.jpg", "2026-11-20%20Smilodon/..%5Cx.jpg",
            "2026-11-20%20Smilodon%2F..%2F..%2F/x.jpg", "2026-11-20%20Smilodon/x.jpg%3Aevil", "2026-11-20%20Smilodon/x.jpg%00",
            "2026-11-20%20Smilodon/notes.txt", "2026-11-20%20Smilodon/take.mp3", "2026-11-20%20Smilodon/x", "2026-11-20%20Smilodon/sub/x.jpg",
            "x.jpg", "", "/x.jpg", "..%2F2026-11-20%20Smilodon/x.jpg", "2026-11-20%20Smilodon/con.jpg", "2026-11-20%20Smilodon/.jpg"] {
            assert_eq!(vthumb_in(&dl, bad), None, "{bad:?}");
        }
        assert_eq!(vthumb_in(&dl, "2026-11-20%20Smilodon/smilodon-thumbnail-1.jpg"), Some((tdir.join("smilodon-thumbnail-1.jpg"), "image/jpeg")));
        assert_eq!(vthumb_in(&dl, "2026-11-20%20Smilodon/My%20pick%20(2).PNG"), Some((tdir.join("My pick (2).PNG"), "image/png")));
        assert_eq!(vthumb_in(&dl, "2026-11-20%20Smilodon/a.webp").map(|x| x.1), Some("image/webp"));

        assert_eq!(video::thumbs(&tdir, "2026-11-20 Smilodon")["files"], json!([]));
        fs::create_dir_all(&tdir).unwrap();
        fs::write(tdir.join("old.jpg"), "a").unwrap();
        fs::write(tdir.join("notes.txt"), "b").unwrap();
        fs::write(tdir.join("new.png"), "c").unwrap();
        let t = |f: &str, s: u64| fs::File::options().write(true).open(tdir.join(f)).unwrap().set_modified(UNIX_EPOCH + Duration::from_secs(s)).unwrap();
        t("old.jpg", 1_700_000_000);
        t("new.png", 1_800_000_000);
        let v = video::thumbs(&tdir, "2026-11-20 Smilodon");
        let names: Vec<&str> = v["files"].as_array().unwrap().iter().map(|f| f["file"].as_str().unwrap()).collect();
        assert_eq!(names, ["new.png", "old.jpg"]);
        assert_eq!(v["files"][0]["url"], "/api/vthumb/2026-11-20%20Smilodon/new.png");
        assert_eq!(v["files"][0]["at"], 1_800_000_000u64);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn thumbnail_names_keep_their_number() {
        let long = "What Really Happened When the Asteroid Hit the Earth Sixty Six Million Years Ago";
        let a = thumb_file_stem(&format!("{long} thumbnail 1"));
        let b = thumb_file_stem(&format!("{long} thumbnail 2"));
        assert!(a.ends_with("-thumbnail-1") && b.ends_with("-thumbnail-2"), "{a} {b}");
        assert_eq!(thumb_file_stem("Smilodon thumbnail 3"), "smilodon-thumbnail-3");
        assert_eq!(thumb_file_stem(" thumbnail 1"), "video-thumbnail-1");
        assert_eq!(thumb_file_stem("../.."), "video-thumbnail");
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

    pub const EXPIRED: &str = "The YouTube permission expired.";

    fn friendly(code: u16, body: &str) -> String {
        let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let msg = v["error"]["message"].as_str().or(v["error_description"].as_str()).or(v["error"].as_str()).unwrap_or("").to_string();
        let low = msg.to_lowercase();
        match code {
            0 => format!("Could not reach Google. Check your internet connection. ({body})"),
            400 if low.contains("invalid_grant") || low.contains("expired") || low.contains("revoked") => {
                format!("{} Google ends it every 7 days while the Google app is in Testing. Click Connect again.", EXPIRED)
            }
            401 => "Google rejected the Client ID or Client secret. Check them in Settings.".into(),
            403 if low.contains("has not been used") || low.contains("disabled") => {
                "The YouTube APIs are not enabled in Google Cloud. Enable YouTube Data API v3 and YouTube Analytics API (step 2).".into()
            }
            403 => format!("Google denied access: {msg}"),
            _ => format!("Google failed (code {code}): {msg}"),
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
            return Err("Fill in and save the Client ID and Client secret from Google Cloud first.".into());
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
                return Err("The login session does not match. Click Connect YouTube again from the app.".into());
            }
            s.clear();
        }
        let r = redirect(port);
        let v = post_form(
            "https://oauth2.googleapis.com/token",
            &[("code", code), ("client_id", &c.yt_client_id), ("client_secret", &c.yt_secret), ("redirect_uri", &r), ("grant_type", "authorization_code")],
        )?;
        v["refresh_token"].as_str().map(String::from).ok_or_else(|| "Google gave no long-term permission. Click Connect YouTube again.".into())
    }

    fn access_token(c: &Config) -> Result<String, String> {
        if c.yt_refresh.is_empty() {
            return Err("YouTube is not connected. Click Connect YouTube first.".into());
        }
        let v = post_form(
            "https://oauth2.googleapis.com/token",
            &[("refresh_token", &c.yt_refresh), ("client_id", &c.yt_client_id), ("client_secret", &c.yt_secret), ("grant_type", "refresh_token")],
        )?;
        v["access_token"].as_str().map(String::from).ok_or_else(|| "Google gave no access token.".into())
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
        let ch = ch["items"].get(0).cloned().ok_or("This Google account has no YouTube channel. Sign in with the account that owns the channel.")?;
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
        write_atomic(&dir.join("data.json"), &bytes).map_err(|e| format!("Could not save the data: {e}"))?;
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

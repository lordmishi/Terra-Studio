// Terra Studio: a local desktop toolkit for YouTube creators.
// Serves the UI on 127.0.0.1, opens it in an app-style browser window, proxies
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
    api_key: String,
    model: String,
    effort: String,
    profile: Value,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            api_key: String::new(),
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
    fs::read_to_string(dir.join("config.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
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
    // Second launch: the app is already running, so just open another window.
    for &p in &PORTS {
        if ping(p) {
            open_window(p);
            return;
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
    let cfg = Arc::new(Mutex::new(load_config(&dir)));
    // Give the window a minute of grace to load before the idle timer counts.
    let last_seen = Arc::new(AtomicU64::new(now() + 60));
    {
        let last_seen = last_seen.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(10));
            if now().saturating_sub(last_seen.load(Ordering::Relaxed)) > IDLE_EXIT_SECS {
                std::process::exit(0);
            }
        });
    }

    open_window(port);

    for req in server.incoming_requests() {
        let cfg = cfg.clone();
        let dir = dir.clone();
        let last_seen = last_seen.clone();
        std::thread::spawn(move || handle(req, port, &cfg, &dir, &last_seen));
    }
}

fn ping(port: u16) -> bool {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(800))
        .build()
        .get(&format!("http://127.0.0.1:{port}/api/ping"))
        .call()
        .ok()
        .and_then(|r| r.into_string().ok())
        .map(|s| s == "terra-studio")
        .unwrap_or(false)
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
        (Method::Get, "/api/ping") => respond(req, 200, "text/plain", b"terra-studio".to_vec()),
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
            let saved = serde_json::to_vec_pretty(&*c).map_err(|e| e.to_string()).and_then(|bytes| {
                write_atomic(&dir.join("config.json"), &bytes).map_err(|e| e.to_string())
            });
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
        (Method::Post, "/api/quit") => {
            respond_json(req, 200, json!({"ok": true}));
            std::thread::sleep(Duration::from_millis(300));
            std::process::exit(0);
        }
        _ => respond_json(req, 404, json!({"error": "not found"})),
    }
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

// ---------------------------------------------------------------- Claude API

fn generate(c: &Config, prompt: &str, test: bool) -> (u16, Value) {
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

// ------------------------------------------------------- YouTube Studio data

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

// Direct Google Flow integration: a dedicated WebView2 window inside Terra Studio
// for https://flow.google.com with a persistent login session, a CSP-safe prompt bar,
// and automatic download routing into gen/, 03 Images, and 04 Video clips.
use super::*;
use std::collections::HashMap;

pub const FLOW_URL: &str = "https://flow.google.com";
const MAX_IMAGE_BYTES: usize = 35 * 1024 * 1024;
const MAX_VIDEO_BYTES: usize = 350 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowKind {
    Browse,
    Test,
    Thumbnail,
    Image,
    Clip,
}

impl FlowKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "browse" => Some(Self::Browse),
            "test" => Some(Self::Test),
            "thumbnail" => Some(Self::Thumbnail),
            "image" => Some(Self::Image),
            "clip" => Some(Self::Clip),
            _ => None,
        }
    }

    pub fn is_video(self) -> bool {
        self == Self::Clip
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FlowItem {
    pub id: String,
    pub label: String,
    pub prompt: String,
    pub kind: FlowKind,
    pub slot: u8,
    pub stem: String,
    pub folder: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct FlowSaved {
    pub id: String,
    pub kind: FlowKind,
    pub slot: u8,
    pub stem: String,
    pub folder: String,
    pub file: String,
    pub path: String,
    pub bytes: usize,
    pub v: u64,
}

#[derive(Default)]
struct FlowState {
    open: bool,
    queue: Vec<FlowItem>,
    index: usize,
    done_ids: Vec<String>,
    last_saved: Option<FlowSaved>,
    recent: Vec<FlowSaved>,
    last_error: String,
    downloading: bool,
    pending_auto_fill: bool,
    seq: u64,
    tmp_seq: u64,
    pending_tmp: HashMap<PathBuf, (FlowItem, String)>,
}

static STATE: Mutex<Option<FlowState>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut FlowState) -> R) -> R {
    let mut guard = STATE.lock().unwrap();
    f(guard.get_or_insert_with(FlowState::default))
}

/// Active image provider: "flow", "kie", or "gemini" (default).
pub fn image_provider(c: &Config) -> &'static str {
    match c.image_provider.as_str() {
        "kie" => "kie",
        "flow" => "flow",
        _ => "gemini",
    }
}

/// Active video clip provider: "flow" or "kie" (default).
pub fn clip_provider(c: &Config) -> &'static str {
    if c.clip_provider == "flow" { "flow" } else { "kie" }
}

/// Hosts allowed inside the built-in Google Flow window (Flow, Google sign-in across country TLDs, and Google media CDNs).
pub fn is_google_url(url: &str) -> bool {
    let u = url.trim();
    if u == "about:blank" || u.starts_with("blob:") || u.starts_with("data:") {
        return true;
    }
    let Some(rest) = u.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("").split(':').next().unwrap_or("").to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    let matches_domain = |d: &str| host == d || host.ends_with(&format!(".{d}"));
    if [
        "flow.google.com",
        "labs.google",
        "withgoogle.com",
        "googleusercontent.com",
        "gstatic.com",
        "googleapis.com",
        "g.co",
        "youtube.com",
        "youtu.be",
        "1e100.net",
    ]
    .into_iter()
    .any(matches_domain)
    {
        return true;
    }
    if matches_domain("google.com") {
        return true;
    }
    // Country-code Google domains (e.g. accounts.google.co.id, www.google.co.uk, google.com.sg)
    if let Some(idx) = host.find("google.co.") {
        if (idx == 0 || host.as_bytes()[idx - 1] == b'.') && host[idx + 10..].len() == 2 {
            return true;
        }
    }
    if let Some(idx) = host.find("google.com.") {
        if (idx == 0 || host.as_bytes()[idx - 1] == b'.') && host[idx + 11..].len() == 2 {
            return true;
        }
    }
    false
}

pub fn is_flow_app_url(url: &str) -> bool {
    let Some(rest) = url.trim().strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("").split(':').next().unwrap_or("").to_ascii_lowercase();
    host == "flow.google.com" || host.ends_with(".flow.google.com") || host == "labs.google" || host.ends_with(".labs.google")
}

fn current_item(st: &FlowState) -> Option<FlowItem> {
    st.queue.get(st.index).cloned()
}

fn state_json(st: &FlowState) -> Value {
    let cur = current_item(st);
    json!({
        "open": st.open,
        "native": cfg!(windows),
        "url": FLOW_URL,
        "queue_len": st.queue.len(),
        "index": st.index,
        "current": cur,
        "done_ids": st.done_ids,
        "downloading": st.downloading,
        "last_saved": st.last_saved,
        "recent": st.recent,
        "last_error": st.last_error,
        "seq": st.seq,
    })
}

pub fn status() -> Value {
    with_state(|st| state_json(st))
}

pub fn set_open(open: bool) {
    with_state(|st| {
        st.open = open;
        st.seq += 1;
    });
}

/// JSON string pushed into the Flow WebView bar via evaluate_script.
pub fn bar_payload(auto_fill: bool) -> String {
    with_state(|st| {
        let mut v = state_json(st);
        v["auto_fill"] = json!(auto_fill);
        v.to_string()
    })
}

pub fn open() -> Value {
    with_state(|st| {
        st.open = true;
        st.last_error.clear();
        st.seq += 1;
    });
    open_host_window();
    status()
}

pub fn step(delta: isize) -> Value {
    with_state(|st| {
        if !st.queue.is_empty() {
            let max = st.queue.len() - 1;
            let next = (st.index as isize + delta).clamp(0, max as isize) as usize;
            st.index = next;
        }
        st.last_error.clear();
        st.seq += 1;
    });
    sync_host_bar(true);
    status()
}

pub fn clear() -> Value {
    with_state(|st| {
        st.queue.clear();
        st.index = 0;
        st.done_ids.clear();
        st.last_error.clear();
        st.seq += 1;
    });
    sync_host_bar(false);
    status()
}

/// Sets or updates the active Flow prompt queue, validates target folders upfront,
/// opens/focuses the built-in Flow window, and pushes the first prompt.
pub fn send(app: &Path, v: &Value) -> Result<Value, (u16, String)> {
    send_with_root(app, &pack::root(app), v)
}

pub(crate) fn send_with_root(_app: &Path, pack_root: &Path, v: &Value) -> Result<Value, (u16, String)> {
    let default_kind = FlowKind::parse(v["kind"].as_str().unwrap_or("browse"))
        .ok_or_else(|| (400, "Unknown Flow target kind.".to_string()))?;
    let ensure_sub = |folder: &str| -> Result<(), (u16, String)> {
        if !video::valid_name(folder) {
            return Err((400, "The video folder name is not valid.".into()));
        }
        let d = pack_root.join(folder);
        for s in video::SUBS {
            fs::create_dir_all(d.join(s)).map_err(|e| (500, format!("Could not create the video folder: {}", video::why(&e))))?;
        }
        let _ = fs::create_dir_all(d.join(video::NARR).join(video::OLDER));
        Ok(())
    };
    let default_folder = match v["folder"].as_str().filter(|s| !s.is_empty()) {
        Some(f) => {
            ensure_sub(f)?;
            f.to_string()
        }
        None => String::new(),
    };
    let default_stem = match v["id"].as_str() {
        Some("test") => "test".to_string(),
        Some(id) => thumb_stem(id).ok_or_else(|| (400, "Invalid video id.".to_string()))?,
        None => "test".to_string(),
    };

    let mut items: Vec<FlowItem> = Vec::new();
    if let Some(arr) = v["items"].as_array() {
        if arr.len() > 80 {
            return Err((400, "Too many prompts in one Flow batch (max 80).".into()));
        }
        for (idx, raw) in arr.iter().enumerate() {
            let prompt = raw["prompt"].as_str().unwrap_or("").trim().to_string();
            if prompt.is_empty() || prompt.chars().count() > 6000 {
                continue;
            }
            let kind = raw["kind"].as_str().and_then(FlowKind::parse).unwrap_or(default_kind);
            let folder = match raw["folder"].as_str().filter(|s| !s.is_empty()) {
                Some(f) => {
                    ensure_sub(f)?;
                    f.to_string()
                }
                None => default_folder.clone(),
            };
            if matches!(kind, FlowKind::Image | FlowKind::Clip) && folder.is_empty() {
                return Err((400, "A video folder is required for Flow images and clips.".into()));
            }
            let stem = match raw["stem"].as_str().filter(|s| !s.is_empty()) {
                Some("test") => "test".to_string(),
                Some(s) => thumb_stem(s).ok_or_else(|| (400, "Invalid video id.".to_string()))?,
                None => default_stem.clone(),
            };
            let id = raw["id"].as_str().filter(|s| !s.is_empty()).map(String::from).unwrap_or_else(|| format!("{}", idx + 1));
            let slot = raw["slot"].as_u64().unwrap_or(idx as u64).min(9) as u8;
            let label = raw["label"].as_str().filter(|s| !s.is_empty()).map(String::from).unwrap_or_else(|| format!("Prompt {}", id));
            items.push(FlowItem { id, label, prompt, kind, slot, stem, folder });
        }
    } else if let Some(prompt) = v["prompt"].as_str().map(str::trim).filter(|p| !p.is_empty()) {
        if prompt.chars().count() > 6000 {
            return Err((400, "The prompt is too long.".into()));
        }
        if matches!(default_kind, FlowKind::Image | FlowKind::Clip) && default_folder.is_empty() {
            return Err((400, "A video folder is required for Flow images and clips.".into()));
        }
        let slot = v["slot"].as_u64().unwrap_or(0).min(9) as u8;
        let id = v["item_id"].as_str().filter(|s| !s.is_empty()).map(String::from).unwrap_or_else(|| format!("{slot}"));
        let label = v["label"].as_str().filter(|s| !s.is_empty()).map(String::from).unwrap_or_else(|| match default_kind {
            FlowKind::Test => "Test image".into(),
            FlowKind::Thumbnail => format!("Thumbnail {}", slot + 1),
            FlowKind::Image => format!("Shot {} -> 03 Images", id),
            FlowKind::Clip => format!("Clip {} -> 04 Video clips", id),
            FlowKind::Browse => "Google Flow".into(),
        });
        items.push(FlowItem {
            id,
            label,
            prompt: prompt.to_string(),
            kind: default_kind,
            slot,
            stem: default_stem,
            folder: default_folder,
        });
    }

    if items.is_empty() && default_kind != FlowKind::Browse {
        return Err((400, "No prompt to send to Google Flow.".into()));
    }

    with_state(|st| {
        st.open = true;
        st.last_error.clear();
        if !items.is_empty() {
            st.queue = items;
            st.index = 0;
            st.done_ids.clear();
            st.pending_auto_fill = true;
        }
        st.seq += 1;
    });
    open_host_window();
    sync_host_bar(true);
    Ok(status())
}

pub fn on_ipc(msg: &str) {
    match msg.trim() {
        "ready" => {
            let auto = with_state(|st| {
                let a = st.pending_auto_fill;
                st.pending_auto_fill = false;
                a
            });
            sync_host_bar(auto);
        }
        "fill" => sync_host_bar(true),
        "prev" => {
            let _ = step(-1);
        }
        "next" => {
            let _ = step(1);
        }
        "clear" => {
            let _ = clear();
        }
        "focus_main" => {
            #[cfg(windows)]
            native::show();
        }
        _ => {}
    }
}

/// Detects real media extension from file header bytes so Google Flow downloads
/// always land with the right extension regardless of CDN URL shape.
pub fn detect_media_ext(bytes: &[u8]) -> Option<(&'static str, bool)> {
    if bytes.len() < 12 {
        return None;
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(("jpg", false));
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(("png", false));
    }
    if bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some(("webp", false));
    }
    if &bytes[4..8] == b"ftyp" {
        return Some(("mp4", true));
    }
    if bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return Some(("webm", true));
    }
    None
}

fn sanitize_suggested_name(suggested: &Path) -> String {
    let raw = suggested.file_stem().and_then(|s| s.to_str()).unwrap_or("flow");
    file_slug(raw, "flow")
}

/// Redirects WebView2 downloads into a staging path inside `%APPDATA%\TerraStudio\flow-tmp`
/// and captures the active `FlowItem` snapshot at download start.
pub fn begin_download(app: &Path, _uri: &str, suggested: &mut PathBuf) -> bool {
    let fallback_name = sanitize_suggested_name(suggested);
    let tmp_dir = app.join("flow-tmp");
    if fs::create_dir_all(&tmp_dir).is_err() {
        return false;
    }
    let stage = with_state(|st| {
        st.tmp_seq += 1;
        let stage = tmp_dir.join(format!("dl-{}.part", st.tmp_seq));
        let item = current_item(st).unwrap_or_else(|| FlowItem {
            id: format!("browse-{}", st.tmp_seq),
            label: "Google Flow download".into(),
            prompt: String::new(),
            kind: FlowKind::Browse,
            slot: 0,
            stem: String::new(),
            folder: String::new(),
        });
        st.pending_tmp.insert(stage.clone(), (item, fallback_name));
        st.downloading = true;
        st.last_error.clear();
        st.seq += 1;
        stage
    });
    *suggested = stage;
    sync_host_bar(false);
    true
}

/// Validates downloaded bytes, writes to the appropriate Terra Studio destination
/// (`gen/`, `03 Images`, `04 Video clips`, or `Downloads/Terra Studio`), and advances queue.
pub fn finish_download(app: &Path, path_opt: Option<PathBuf>, success: bool) {
    finish_download_with_root(app, &pack::root(app), path_opt, success);
}

pub(crate) fn finish_download_with_root(app: &Path, pack_root: &Path, path_opt: Option<PathBuf>, success: bool) {
    let Some(stage) = path_opt else {
        with_state(|st| {
            st.downloading = false;
            if !success {
                st.last_error = "Google Flow download did not complete.".into();
            }
            st.seq += 1;
        });
        sync_host_bar(false);
        return;
    };

    let popped = with_state(|st| {
        st.downloading = false;
        st.pending_tmp.remove(&stage)
    });

    if !success {
        let _ = fs::remove_file(&stage);
        with_state(|st| {
            st.last_error = "Google Flow download was cancelled or failed.".into();
            st.seq += 1;
        });
        sync_host_bar(false);
        return;
    }

    let Some((item, fallback_name)) = popped else {
        let _ = fs::remove_file(&stage);
        return;
    };

    let res = finalize_file(app, pack_root, &stage, &item, &fallback_name);
    let _ = fs::remove_file(&stage);

    with_state(|st| {
        match res {
            Ok(saved) => {
                if !st.done_ids.contains(&item.id) {
                    st.done_ids.push(item.id.clone());
                }
                st.last_saved = Some(saved.clone());
                st.recent.push(saved);
                if st.recent.len() > 30 {
                    let drop_n = st.recent.len() - 30;
                    st.recent.drain(..drop_n);
                }
                st.last_error.clear();
                // Auto-advance the pointer to the next unfinished item in the queue.
                if let Some(next_idx) = ((st.index + 1)..st.queue.len())
                    .chain(0..st.index)
                    .find(|&i| !st.done_ids.contains(&st.queue[i].id))
                {
                    st.index = next_idx;
                }
            }
            Err(e) => {
                st.last_error = e;
            }
        }
        st.seq += 1;
    });
    sync_host_bar(false);
}

fn finalize_file(app: &Path, pack_root: &Path, stage: &Path, item: &FlowItem, fallback_name: &str) -> Result<FlowSaved, String> {
    let meta = fs::metadata(stage).map_err(|e| format!("Could not read downloaded file: {e}"))?;
    let limit = if item.kind.is_video() { MAX_VIDEO_BYTES } else { MAX_IMAGE_BYTES };
    if meta.len() == 0 || meta.len() > limit as u64 {
        return Err("Downloaded file from Google Flow is empty or too large.".into());
    }
    let bytes = fs::read(stage).map_err(|e| format!("Could not read downloaded file: {e}"))?;
    let Some((ext, is_vid)) = detect_media_ext(&bytes) else {
        return Err("Google Flow downloaded an unsupported file type (expected PNG, JPG, WebP, or MP4).".into());
    };

    let (file, dest_path, actual_kind) = match item.kind {
        FlowKind::Test | FlowKind::Thumbnail => {
            if is_vid {
                return Err("Expected an image for the thumbnail slot, but a video was downloaded.".into());
            }
            let gdir = app.join("gen");
            fs::create_dir_all(&gdir).map_err(|e| format!("Could not create preview folder: {e}"))?;
            let stem = if item.stem.is_empty() { "test" } else { &item.stem };
            for old in ["png", "jpg", "webp"] {
                let _ = fs::remove_file(gdir.join(format!("{stem}-{}.{old}", item.slot)));
            }
            let file = format!("{stem}-{}.{ext}", item.slot);
            let p = gdir.join(&file);
            write_atomic(&p, &bytes).map_err(|e| format!("Could not save image: {e}"))?;
            (file, p, item.kind)
        }
        FlowKind::Image | FlowKind::Clip => {
            if !video::valid_name(&item.folder) {
                return Err("The video folder name is not valid.".into());
            }
            let folder_path = pack_root.join(&item.folder);
            for s in video::SUBS {
                fs::create_dir_all(folder_path.join(s)).map_err(|e| format!("Could not create the video folder: {}", video::why(&e)))?;
            }
            let sub = if is_vid || item.kind == FlowKind::Clip { video::SUBS[3] } else { video::SUBS[2] };
            let kind = if is_vid { FlowKind::Clip } else { FlowKind::Image };
            let prefix = if kind == FlowKind::Clip { "clip" } else { "shot" };
            let slug = file_slug(&item.id, fallback_name);
            let desired = format!("{prefix}-{slug}.{ext}");
            let saved = video::save_new(&folder_path.join(sub), &desired, &bytes)
                .map_err(|e| format!("Could not save Flow file: {}", video::why(&e)))?;
            let fname = saved.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(desired);
            (fname, saved, kind)
        }
        FlowKind::Browse => {
            fs::create_dir_all(pack_root).map_err(|e| format!("Could not create Terra Studio folder: {e}"))?;
            let desired = format!("{fallback_name}.{ext}");
            let saved = video::save_new(pack_root, &desired, &bytes)
                .map_err(|e| format!("Could not save Flow file: {}", video::why(&e)))?;
            let fname = saved.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(desired);
            (fname, saved, if is_vid { FlowKind::Clip } else { FlowKind::Image })
        }
    };

    Ok(FlowSaved {
        id: item.id.clone(),
        kind: actual_kind,
        slot: item.slot,
        stem: item.stem.clone(),
        folder: item.folder.clone(),
        file,
        path: dest_path.display().to_string(),
        bytes: bytes.len(),
        v: now(),
    })
}

fn open_host_window() {
    #[cfg(windows)]
    native::open_flow();
    #[cfg(not(windows))]
    open_browser(FLOW_URL);
}

fn sync_host_bar(auto_fill: bool) {
    #[cfg(windows)]
    native::sync_flow(auto_fill);
    #[cfg(not(windows))]
    let _ = auto_fill;
}

/// Pure-DOM initialization script injected into the built-in Google Flow WebView2 window.
/// Uses Shadow DOM + createElement (never innerHTML) so Google Flow's Trusted Types & CSP never block it.
pub const INIT_SCRIPT: &str = r#"
(function () {
  if (window.__terraFlowInit) return;
  window.__terraFlowInit = true;
  var host = (location.hostname || '').toLowerCase();
  var isFlow = host === 'flow.google.com' || host.endsWith('.flow.google.com') || host === 'labs.google' || host.endsWith('.labs.google');
  if (!isFlow) return;

  var state = null;
  var rootHost = null;
  var shadow = null;
  var els = {};
  var autoRun = true;
  var activeRunKey = '';
  var watchTimer = 0;
  var knownMedia = new Set();

  function post(msg) {
    try {
      if (window.ipc && typeof window.ipc.postMessage === 'function') {
        window.ipc.postMessage(String(msg));
      }
    } catch (_) {}
  }

  function el(tag, css, text) {
    var n = document.createElement(tag);
    if (css) n.style.cssText = css;
    if (text != null) n.textContent = text;
    return n;
  }

  function makeBtn(label, css, onClick) {
    var b = el('button', 'appearance:none;border:0;border-radius:8px;padding:6px 11px;font:600 12px/1.2 system-ui,sans-serif;cursor:pointer;' + (css || ''), label);
    b.type = 'button';
    b.addEventListener('click', onClick);
    return b;
  }

  function hasChallenge() {
    // Only pause on a genuinely visible CAPTCHA challenge popup, never on Google's hidden background reCAPTCHA badge.
    var frames = document.querySelectorAll('iframe[src*="bframe"], iframe[src*="recaptcha"][src*="challenge"], [role="dialog"] iframe[src*="recaptcha"]');
    for (var i = 0; i < frames.length; i++) {
      var f = frames[i];
      var s = window.getComputedStyle(f);
      if (s.display === 'none' || s.visibility === 'hidden' || s.opacity === '0') continue;
      var r = f.getBoundingClientRect();
      if (r.width >= 160 && r.height >= 120) return true;
    }
    return false;
  }

  function collectDeep(root, selector, out) {
    if (!root) return;
    var list = root.querySelectorAll(selector);
    for (var i = 0; i < list.length; i++) out.push(list[i]);
    var all = root.querySelectorAll('*');
    for (var j = 0; j < all.length; j++) {
      if (all[j].shadowRoot) collectDeep(all[j].shadowRoot, selector, out);
    }
  }

  function findPromptInput() {
    var candidates = [];
    var list = [];
    collectDeep(document, 'div[contenteditable="true"], [contenteditable="plaintext-only"], [contenteditable=""], [role="textbox"], textarea, input[type="text"], input:not([type])', list);
    var vh = window.innerHeight || 800;
    for (var i = 0; i < list.length; i++) {
      var node = list[i];
      if (rootHost && rootHost.contains(node)) continue;
      if (node.disabled || node.readOnly) continue;
      var style = window.getComputedStyle(node);
      if (style.display === 'none' || style.visibility === 'hidden' || style.opacity === '0') continue;
      var rect = node.getBoundingClientRect();
      if (rect.width < 100 || rect.height < 14 || rect.bottom <= 0 || rect.top >= vh) continue;
      var hint = ((node.getAttribute('placeholder') || '') + ' ' + (node.getAttribute('aria-label') || '') + ' ' + (node.dataset && node.dataset.placeholder || '')).toLowerCase();
      if (node.tagName === 'INPUT' && (node.type === 'search' || /\b(search|cari)\b/.test(hint))) continue;
      var score = rect.width * Math.min(rect.height, 140);
      if (rect.top > vh * 0.45) score += 200000;
      if (/buat|create|prompt|describe|ingin|imagine|scene/.test(hint)) score += 300000;
      if (node.tagName === 'TEXTAREA' || node.isContentEditable) score += 50000;
      candidates.push({ node: node, score: score });
    }
    candidates.sort(function (a, b) { return b.score - a.score; });
    return candidates.length ? candidates[0].node : null;
  }

  function findSubmitBtn(inputEl) {
    var scope = inputEl && inputEl.parentElement;
    for (var depth = 0; depth < 6 && scope && scope !== document.body; depth++) {
      var btns = [];
      collectDeep(scope, 'button:not([disabled]), [role="button"]:not([aria-disabled="true"])', btns);
      var labeled = null;
      var rightmost = null;
      var maxRight = -1;
      for (var i = 0; i < btns.length; i++) {
        var b = btns[i];
        if (rootHost && rootHost.contains(b)) continue;
        if (b.getAttribute('role') === 'switch') continue;
        var rect = b.getBoundingClientRect();
        if (rect.width < 18 || rect.height < 18 || rect.width > 220) continue;
        var lbl = ((b.getAttribute('aria-label') || '') + ' ' + (b.textContent || '') + ' ' + (b.title || '')).toLowerCase();
        if (/generate|create|send|submit|arrow_forward|arrow_upward|buat|kirim|mulai/.test(lbl)) {
          labeled = b;
        }
        // In Google Flow's bottom composer pill, the rightmost compact icon button is the submit arrow.
        if (rect.width <= 64 && rect.height <= 64 && rect.right > maxRight && !/add|tambah|\+|attach|upload|settings|pengaturan/.test(lbl)) {
          maxRight = rect.right;
          rightmost = b;
        }
      }
      if (labeled) return labeled;
      if (rightmost) return rightmost;
      scope = scope.parentElement;
    }
    var all = [];
    collectDeep(document, 'button[type="submit"]:not([disabled]), button[aria-label*="Generate" i]:not([disabled]), button[aria-label*="Create" i]:not([disabled]), button[aria-label*="Send" i]:not([disabled]), button[aria-label*="Kirim" i]:not([disabled])', all);
    for (var j = 0; j < all.length; j++) {
      if (!(rootHost && rootHost.contains(all[j]))) return all[j];
    }
    return null;
  }

  function mediaSrc(node) {
    if (!node) return '';
    return node.currentSrc || node.src || '';
  }

  function snapshotMedia() {
    knownMedia.clear();
    var nodes = document.querySelectorAll('img, video');
    for (var i = 0; i < nodes.length; i++) {
      var s = mediaSrc(nodes[i]);
      if (s) knownMedia.add(s);
    }
  }

  function triggerBrowserDownload(url, fallbackName) {
    if (!url) return false;
    if (url.startsWith('blob:') || url.startsWith('data:')) {
      var a = document.createElement('a');
      a.href = url;
      a.download = fallbackName || 'flow-asset';
      document.body.appendChild(a);
      a.click();
      a.remove();
      return true;
    }
    fetch(url, { credentials: 'include' })
      .then(function (r) { if (!r.ok) throw new Error('http ' + r.status); return r.blob(); })
      .then(function (blob) {
        var bu = URL.createObjectURL(blob);
        var a = document.createElement('a');
        a.href = bu;
        a.download = fallbackName || 'flow-asset';
        document.body.appendChild(a);
        a.click();
        a.remove();
        setTimeout(function () { URL.revokeObjectURL(bu); }, 8000);
      })
      .catch(function () {
        var a = document.createElement('a');
        a.href = url;
        a.download = fallbackName || 'flow-asset';
        document.body.appendChild(a);
        a.click();
        a.remove();
      });
    return true;
  }

  function stopWatch() {
    if (watchTimer) {
      clearInterval(watchTimer);
      watchTimer = 0;
    }
  }

  function watchForNewMedia(item) {
    stopWatch();
    var wantVideo = item && item.kind === 'clip';
    var startedAt = Date.now();
    watchTimer = setInterval(function () {
      if (Date.now() - startedAt > 240000) {
        stopWatch();
        showNote('Timed out waiting for Flow result. Click Download in Flow when ready.', true);
        return;
      }
      if (hasChallenge()) {
        stopWatch();
        showNote('Verification needed in Flow. Complete it, then click Fill & Generate.', true);
        return;
      }
      var sel = wantVideo ? 'video' : 'img';
      var nodes = document.querySelectorAll(sel);
      for (var i = nodes.length - 1; i >= 0; i--) {
        var n = nodes[i];
        if (rootHost && rootHost.contains(n)) continue;
        var s = mediaSrc(n);
        if (!s || knownMedia.has(s)) continue;
        if (wantVideo) {
          if (n.readyState >= 1 || s.startsWith('http') || s.startsWith('blob:')) {
            knownMedia.add(s);
            stopWatch();
            showNote('Video ready, saving to Terra Studio...', false);
            triggerBrowserDownload(s, (item.id || 'clip') + '.mp4');
            return;
          }
        } else {
          var rect = n.getBoundingClientRect();
          if (n.complete && n.naturalWidth >= 256 && rect.width >= 120) {
            knownMedia.add(s);
            stopWatch();
            showNote('Image ready, saving to Terra Studio...', false);
            triggerBrowserDownload(s, (item.id || 'image') + '.png');
            return;
          }
        }
      }
    }, 1000);
  }

  function fillPrompt(promptText, andRun) {
    if (!promptText) return false;
    try { navigator.clipboard && navigator.clipboard.writeText(promptText).catch(function () {}); } catch (_) {}
    var input = findPromptInput();
    if (!input) {
      showNote('Prompt copied. Open or create a Flow project so the prompt box appears.', false);
      return false;
    }
    try {
      input.click && input.click();
      input.focus();
      if (input.tagName === 'TEXTAREA' || input.tagName === 'INPUT') {
        var proto = input.tagName === 'TEXTAREA' ? window.HTMLTextAreaElement.prototype : window.HTMLInputElement.prototype;
        var desc = Object.getOwnPropertyDescriptor(proto, 'value');
        if (desc && desc.set) desc.set.call(input, promptText);
        else input.value = promptText;
        input.dispatchEvent(new Event('input', { bubbles: true }));
        input.dispatchEvent(new Event('change', { bubbles: true }));
      } else {
        var sel = window.getSelection();
        var range = document.createRange();
        range.selectNodeContents(input);
        sel.removeAllRanges();
        sel.addRange(range);
        var ok = false;
        try { ok = document.execCommand('insertText', false, promptText); } catch (_) {}
        if (!ok || (input.textContent || '').trim() === '') {
          try { input.dispatchEvent(new InputEvent('beforeinput', { bubbles: true, cancelable: true, inputType: 'insertText', data: promptText })); } catch (_) {}
          input.textContent = promptText;
          input.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: promptText }));
        }
      }
      if (andRun && autoRun && state && state.current) {
        if (hasChallenge()) {
          showNote('Verification required in Flow before generating.', true);
          return true;
        }
        snapshotMedia();
        setTimeout(function () {
          var btn = findSubmitBtn(input);
          if (btn) {
            btn.click();
            showNote('Generating in Flow... result will save to Terra Studio automatically.', false);
            watchForNewMedia(state.current);
          } else {
            try {
              input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', code: 'Enter', keyCode: 13, which: 13, bubbles: true }));
              input.dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter', code: 'Enter', keyCode: 13, which: 13, bubbles: true }));
            } catch (_) {}
            showNote('Prompt filled. If generation did not start, click the arrow button in Flow.', false);
            watchForNewMedia(state.current);
          }
        }, 320);
      } else {
        showNote('Prompt inserted into Flow (also copied). Click Generate, then Download.', false);
      }
      return true;
    } catch (_) {
      showNote('Prompt copied to clipboard. Click the Flow box and press Ctrl+V.', false);
      return false;
    }
  }

  function showNote(msg, isErr) {
    if (!els.note) return;
    els.note.textContent = msg || '';
    els.note.style.color = isErr ? '#fca5a5' : '#d6d3d1';
    els.note.style.display = msg ? 'block' : 'none';
  }

  function updateAutoBtn() {
    if (!els.autoBtn) return;
    els.autoBtn.textContent = autoRun ? 'Auto-run: ON' : 'Auto-run: OFF';
    els.autoBtn.style.background = autoRun ? 'rgba(52,211,153,0.22)' : 'rgba(255,255,255,0.08)';
    els.autoBtn.style.color = autoRun ? '#6ee7b7' : '#d6d3d1';
  }

  function ensureBar() {
    if (rootHost && rootHost.isConnected) return;
    if (!document.body) return;
    rootHost = document.createElement('div');
    rootHost.id = 'terra-flow-bridge-host';
    rootHost.style.cssText = 'position:fixed;top:12px;right:14px;z-index:2147483647;max-width:440px;width:calc(100vw - 28px);font-family:system-ui,-apple-system,Segoe UI,sans-serif;';
    shadow = rootHost.attachShadow({ mode: 'open' });

    var box = el('div', 'background:#141210;color:#f5f0e8;border:1px solid rgba(255,255,255,0.16);border-radius:14px;padding:11px 13px;box-shadow:0 10px 28px rgba(0,0,0,0.42);display:flex;flex-direction:column;gap:8px;');
    var top = el('div', 'display:flex;align-items:center;justify-content:space-between;gap:8px;');
    var titleWrap = el('div', 'display:flex;flex-direction:column;gap:2px;min-width:0;');
    els.kicker = el('span', 'font-size:11px;font-weight:700;letter-spacing:0.04em;text-transform:uppercase;color:#f2a230;', 'Terra Studio');
    els.title = el('b', 'font-size:13px;font-weight:700;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;', 'Google Flow Ready');
    titleWrap.appendChild(els.kicker);
    titleWrap.appendChild(els.title);

    var rightTop = el('div', 'display:flex;align-items:center;gap:6px;flex-shrink:0;');
    els.autoBtn = makeBtn('Auto-run: ON', '', function () {
      autoRun = !autoRun;
      if (!autoRun) stopWatch();
      updateAutoBtn();
    });
    els.backBtn = makeBtn('Terra Studio', 'background:rgba(255,255,255,0.12);color:#f5f0e8;', function () { post('focus_main'); });
    els.minBtn = makeBtn('_', 'background:rgba(255,255,255,0.08);color:#d6d3d1;padding:6px 9px;', function () {
      var collapsed = els.body.style.display === 'none';
      els.body.style.display = collapsed ? 'flex' : 'none';
      els.minBtn.textContent = collapsed ? '_' : '+';
    });
    rightTop.appendChild(els.autoBtn);
    rightTop.appendChild(els.backBtn);
    rightTop.appendChild(els.minBtn);
    top.appendChild(titleWrap);
    top.appendChild(rightTop);

    els.body = el('div', 'display:flex;flex-direction:column;gap:8px;');
    els.prompt = el('div', 'font-size:12px;line-height:1.4;color:#e7e5e4;background:rgba(255,255,255,0.07);border-radius:8px;padding:7px 9px;max-height:74px;overflow:auto;word-break:break-word;', '');
    els.actions = el('div', 'display:flex;flex-wrap:wrap;gap:6px;align-items:center;');
    els.fillBtn = makeBtn('Fill & Generate', 'background:#f2a230;color:#141210;', function () {
      if (state && state.current && state.current.prompt) fillPrompt(state.current.prompt, true);
      else post('fill');
    });
    els.prevBtn = makeBtn('Prev', 'background:rgba(255,255,255,0.12);color:#f5f0e8;', function () { stopWatch(); post('prev'); });
    els.nextBtn = makeBtn('Next', 'background:rgba(255,255,255,0.12);color:#f5f0e8;', function () { stopWatch(); post('next'); });
    els.clearBtn = makeBtn('Clear', 'background:transparent;color:#a8a29e;', function () { stopWatch(); post('clear'); });
    els.actions.appendChild(els.fillBtn);
    els.actions.appendChild(els.prevBtn);
    els.actions.appendChild(els.nextBtn);
    els.actions.appendChild(els.clearBtn);

    els.note = el('div', 'font-size:11.5px;line-height:1.35;color:#d6d3d1;display:none;', '');

    els.body.appendChild(els.prompt);
    els.body.appendChild(els.actions);
    els.body.appendChild(els.note);

    box.appendChild(top);
    box.appendChild(els.body);
    shadow.appendChild(box);
    document.body.appendChild(rootHost);
    updateAutoBtn();
  }

  function render() {
    ensureBar();
    if (!els.title) return;
    if (!state || !state.current) {
      stopWatch();
      els.kicker.textContent = 'Terra Studio';
      els.title.textContent = 'Signed in & auto-saving downloads';
      els.prompt.textContent = 'Any image or video you download here is saved automatically into Terra Studio. Send prompts from Thumbnail or Edit pack to run them automatically.';
      els.actions.style.display = 'none';
      if (state && state.last_saved) {
        showNote('Last saved: ' + state.last_saved.file, false);
      } else if (state && state.last_error) {
        showNote(state.last_error, true);
      } else {
        showNote('', false);
      }
      return;
    }

    var cur = state.current;
    var done = Array.isArray(state.done_ids) && state.done_ids.indexOf(cur.id) !== -1;
    els.kicker.textContent = 'Terra Studio · ' + (state.index + 1) + ' of ' + state.queue_len + (done ? ' · Saved' : '');
    els.title.textContent = cur.label || 'Active prompt';
    els.prompt.textContent = cur.prompt || '';
    els.actions.style.display = 'flex';
    els.fillBtn.textContent = autoRun ? 'Fill & Generate' : 'Fill prompt';
    els.prevBtn.disabled = state.index <= 0;
    els.nextBtn.disabled = state.index >= state.queue_len - 1;
    els.prevBtn.style.opacity = els.prevBtn.disabled ? '0.45' : '1';
    els.nextBtn.style.opacity = els.nextBtn.disabled ? '0.45' : '1';

    if (state.last_error) {
      showNote(state.last_error, true);
    } else if (state.downloading) {
      showNote('Saving download into Terra Studio...', false);
    } else if (state.last_saved) {
      showNote('Saved ' + state.last_saved.file + '. Starting next item...', false);
    }
  }

  window.__terraFlowUpdate = function (nextState) {
    state = nextState;
    render();
    if (nextState && nextState.current && nextState.current.prompt) {
      var done = Array.isArray(nextState.done_ids) && nextState.done_ids.indexOf(nextState.current.id) !== -1;
      var runKey = (nextState.index + ':' + nextState.current.id);
      if (nextState.auto_fill || (autoRun && !done && activeRunKey !== runKey && nextState.last_saved)) {
        activeRunKey = runKey;
        setTimeout(function () { fillPrompt(nextState.current.prompt, true); }, 550);
      }
    }
  };

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', function () {
      ensureBar();
      post('ready');
    });
  } else {
    ensureBar();
    post('ready');
  }
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_url_filter_allows_flow_and_blocks_unrelated_sites() {
        assert!(is_google_url("https://flow.google.com/"));
        assert!(is_google_url("https://flow.google.com/fx/tools/video-fx"));
        assert!(is_google_url("https://labs.google/fx"));
        assert!(is_google_url("https://accounts.google.com/v3/signin"));
        assert!(is_google_url("https://accounts.google.co.id/v3/signin"));
        assert!(is_google_url("https://www.google.co.id/"));
        assert!(is_google_url("https://withgoogle.com/"));
        assert!(is_google_url("https://lh3.googleusercontent.com/abc"));
        assert!(is_google_url("about:blank"));
        assert!(is_google_url("blob:https://flow.google.com/1234"));
        assert!(!is_google_url("http://flow.google.com/"));
        assert!(!is_google_url("https://evil.com/flow.google.com"));
        assert!(!is_google_url("https://evilgoogle.co.id/"));
        assert!(!is_google_url("https://example.org/"));
    }

    #[test]
    fn magic_byte_detection_handles_images_and_videos() {
        assert_eq!(detect_media_ext(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0, 0, 0, 0, 0, 0, 0]), Some(("jpg", false)));
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0, 0, 0, 13]);
        assert_eq!(detect_media_ext(&png), Some(("png", false)));
        let mut webp = b"RIFF1234WEBP".to_vec();
        assert_eq!(detect_media_ext(&webp), Some(("webp", false)));
        webp[8] = b'X';
        assert_eq!(detect_media_ext(&webp), None);
        assert_eq!(detect_media_ext(b"\x00\x00\x00\x20ftypisom\x00\x00"), Some(("mp4", true)));
        assert_eq!(detect_media_ext(&[0x1A, 0x45, 0xDF, 0xA3, 0, 0, 0, 0, 0, 0, 0, 0]), Some(("webm", true)));
    }

    #[test]
    fn downloads_route_to_video_subfolders_and_gen_without_overwriting() {
        let app = std::env::temp_dir().join(format!("terra-flow-test-{}", std::process::id()));
        let pack_root = app.join("Downloads").join("Terra Studio");
        let _ = fs::remove_dir_all(&app);
        fs::create_dir_all(&pack_root).unwrap();

        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0, 0, 0, 13, 1, 2, 3, 4]);
        let mp4 = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00mp41".to_vec();

        // Send a 2-item batch: first an image into 03 Images, second a clip into 04 Video clips.
        let req = json!({
            "folder": "2026-11-26 Test Flow",
            "items": [
                {"id": "1.1", "kind": "image", "label": "Shot 1.1", "prompt": "Giant hyena in cave"},
                {"id": "1.2", "kind": "clip", "label": "Clip 1.2", "prompt": "Hyena running"}
            ]
        });
        let st = send_with_root(&app, &pack_root, &req).unwrap();
        assert_eq!(st["queue_len"], 2);
        assert_eq!(st["index"], 0);

        // Simulate downloading shot 1.1
        let mut p1 = PathBuf::from("C:\\temp\\download.bin");
        assert!(begin_download(&app, "https://lh3.googleusercontent.com/x", &mut p1));
        fs::write(&p1, &png).unwrap();
        finish_download_with_root(&app, &pack_root, Some(p1), true);

        let st = status();
        assert_eq!(st["last_saved"]["file"], "shot-1-1.png");
        assert_eq!(st["index"], 1);
        let img_saved = pack_root.join("2026-11-26 Test Flow").join("03 Images").join("shot-1-1.png");
        assert!(img_saved.is_file());

        // Simulate downloading clip 1.2
        let mut p2 = PathBuf::from("C:\\temp\\video");
        assert!(begin_download(&app, "https://storage.googleapis.com/y", &mut p2));
        fs::write(&p2, &mp4).unwrap();
        finish_download_with_root(&app, &pack_root, Some(p2), true);

        let st = status();
        assert_eq!(st["last_saved"]["file"], "clip-1-2.mp4");
        let vid_saved = pack_root.join("2026-11-26 Test Flow").join("04 Video clips").join("clip-1-2.mp4");
        assert!(vid_saved.is_file());

        // Thumbnail routing writes into gen/{stem}-{slot}.{ext}
        let thumb_req = json!({"kind": "thumbnail", "id": "abc123", "slot": 1, "prompt": "Thumb prompt"});
        send_with_root(&app, &pack_root, &thumb_req).unwrap();
        let mut p3 = PathBuf::from("thumb.dat");
        assert!(begin_download(&app, "https://flow.google.com/img", &mut p3));
        fs::write(&p3, &png).unwrap();
        finish_download_with_root(&app, &pack_root, Some(p3), true);
        assert!(app.join("gen").join("abc123-1.png").is_file());

        let _ = clear();
        let _ = fs::remove_dir_all(&app);
    }
}

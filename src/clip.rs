// Clip maker: renders infographic clips with Remotion on this PC.
// One-time setup puts a private Node.js and the clip kit in %LOCALAPPDATA%\TerraStudio\clipmaker.
use super::*;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::mpsc;

const NODE_VER: &str = "v22.14.0";
const NODE_SHA256: &str = "55b639295920b219bb2acbcfa00f90393a2789095b7323f79475c9f34795f217";
const NODE_MB: u64 = 32;

const KIT: &[(&str, &[u8])] = &[
    ("package.json", include_bytes!("../assets/clipkit/package.json")),
    ("package-lock.json", include_bytes!("../assets/clipkit/package-lock.json")),
    ("tsconfig.json", include_bytes!("../assets/clipkit/tsconfig.json")),
    ("public/oswald-latin-500-normal.woff2", include_bytes!("../assets/clipkit/public/oswald-latin-500-normal.woff2")),
    ("public/oswald-latin-700-normal.woff2", include_bytes!("../assets/clipkit/public/oswald-latin-700-normal.woff2")),
    ("src/index.ts", include_bytes!("../assets/clipkit/src/index.ts")),
    ("src/Root.tsx", include_bytes!("../assets/clipkit/src/Root.tsx")),
    ("src/kit.tsx", include_bytes!("../assets/clipkit/src/kit.tsx")),
    ("src/shapes.ts", include_bytes!("../assets/clipkit/src/shapes.ts")),
    ("src/BigNumber.tsx", include_bytes!("../assets/clipkit/src/BigNumber.tsx")),
    ("src/SizeCompare.tsx", include_bytes!("../assets/clipkit/src/SizeCompare.tsx")),
    ("src/MapRange.tsx", include_bytes!("../assets/clipkit/src/MapRange.tsx")),
    ("src/Timeline.tsx", include_bytes!("../assets/clipkit/src/Timeline.tsx")),
    ("src/BarChart.tsx", include_bytes!("../assets/clipkit/src/BarChart.tsx")),
    (CUSTOM, include_bytes!("../assets/clipkit/src/custom/Custom.tsx")),
];
const CUSTOM: &str = "src/custom/Custom.tsx";
pub const TEMPLATES: [&str; 6] = ["BigNumber", "SizeCompare", "MapRange", "Timeline", "BarChart", "Custom"];

#[derive(Clone, Default)]
struct Setup {
    running: bool,
    step: String,
    pct: f64,
    error: String,
}

#[derive(Clone)]
struct Job {
    state: &'static str,
    pct: f64,
    msg: String,
    error: String,
    file: String,
    still: Option<PathBuf>,
    at: u64,
}

static SETUP: Mutex<Option<Setup>> = Mutex::new(None);
static JOBS: Mutex<Option<HashMap<u64, Job>>> = Mutex::new(None);
static NEXT: AtomicU64 = AtomicU64::new(1);
/// One render at a time: they share the kit folder, and two at once would starve the PC.
static RENDER: Mutex<()> = Mutex::new(());

pub fn base() -> PathBuf {
    if let Some(d) = std::env::var_os("TERRA_CLIP_DIR") {
        return PathBuf::from(d);
    }
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("TerraStudio")).unwrap_or_else(app_dir).join("clipmaker")
}

fn node_dir(b: &Path) -> PathBuf {
    b.join("node")
}

fn node_exe(b: &Path) -> PathBuf {
    if let Some(n) = std::env::var_os("TERRA_NODE") {
        return PathBuf::from(n);
    }
    if cfg!(windows) { node_dir(b).join("node.exe") } else { node_dir(b).join("bin").join("node") }
}

fn npm_cli(b: &Path) -> PathBuf {
    let exe = node_exe(b);
    let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let win = dir.join("node_modules").join("npm").join("bin").join("npm-cli.js");
    if win.exists() { win } else { dir.join("..").join("lib").join("node_modules").join("npm").join("bin").join("npm-cli.js") }
}

fn kit(b: &Path) -> PathBuf {
    b.join("kit")
}

fn remotion_cli(b: &Path) -> PathBuf {
    kit(b).join("node_modules").join("@remotion").join("cli").join("remotion-cli.js")
}

fn fnv(parts: &[&[u8]]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in p.iter() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Changes when the package list changes, so a new app version reinstalls only when it must.
fn deps_id() -> String {
    format!("{NODE_VER}-{}", fnv(&[KIT[0].1, KIT[1].1]))
}

fn src_id() -> String {
    fnv(&KIT.iter().map(|(_, b)| *b).collect::<Vec<_>>())
}

pub fn ready(b: &Path) -> bool {
    node_exe(b).exists() && remotion_cli(b).exists() && fs::read_to_string(b.join("ready")).is_ok_and(|s| s.trim() == deps_id())
}

pub fn status() -> Value {
    let b = base();
    let s = SETUP.lock().unwrap().clone().unwrap_or_default();
    let mb = 400;
    json!({"ready": ready(&b), "running": s.running, "step": s.step, "pct": s.pct, "error": s.error, "mb": mb, "dir": b.display().to_string()})
}

fn set_setup(f: impl FnOnce(&mut Setup)) {
    let mut g = SETUP.lock().unwrap();
    f(g.get_or_insert_with(Setup::default));
}

/// Starts the one-time setup in the background. Returns false when it is already running.
pub fn start_setup() -> bool {
    {
        let mut g = SETUP.lock().unwrap();
        let s = g.get_or_insert_with(Setup::default);
        if s.running {
            return false;
        }
        *s = Setup { running: true, step: "Starting".into(), pct: 0.0, error: String::new() };
    }
    std::thread::spawn(|| {
        let r = setup(&base());
        set_setup(|s| {
            s.running = false;
            match r {
                Ok(()) => {
                    s.step = "Ready".into();
                    s.pct = 100.0;
                }
                Err(e) => s.error = e,
            }
        });
    });
    true
}

fn setup(b: &Path) -> Result<(), String> {
    fs::create_dir_all(b).map_err(|e| format!("Could not make the clip maker folder {}: {}", b.display(), video::why(&e)))?;
    if !node_exe(b).exists() {
        get_node(b)?;
    }
    write_kit(b, true)?;
    let k = kit(b);
    if !remotion_cli(b).exists() || fs::read_to_string(b.join("ready")).map(|s| s.trim() != deps_id()).unwrap_or(true) {
        let _ = fs::remove_file(b.join("ready"));
        set_setup(|s| {
            s.step = "Installing the clip tools (about 150 MB, 2 to 5 minutes)".into();
            s.pct = -1.0;
        });
        let mut cmd = node_cmd(b);
        cmd.arg(npm_cli(b)).args(["ci", "--omit=dev", "--no-audit", "--no-fund", "--loglevel=error"]).current_dir(&k);
        run(cmd, 1800, |_| {}).map_err(|e| format!("Installing the clip tools failed. Check the internet connection and press Get clip maker again. {e}"))?;
    }
    if std::env::var_os("TERRA_CLIP_BROWSER").is_some() {
        return fs::write(b.join("ready"), deps_id()).map_err(|e| video::why(&e));
    }
    set_setup(|s| {
        s.step = "Downloading the render browser (about 100 MB)".into();
        s.pct = -1.0;
    });
    let mut cmd = node_cmd(b);
    cmd.arg(remotion_cli(b)).args(["browser", "ensure"]).current_dir(&k);
    run(cmd, 1800, |l| {
        if let Some(p) = percent_in(l) {
            set_setup(|s| s.pct = p);
        }
    })
    .map_err(|e| format!("Downloading the render browser failed. Check the internet connection and press Get clip maker again. {e}"))?;
    fs::write(b.join("ready"), deps_id()).map_err(|e| video::why(&e))?;
    Ok(())
}

fn get_node(b: &Path) -> Result<(), String> {
    if !cfg!(windows) {
        return Err("The clip maker downloads Node.js for Windows only. Set TERRA_NODE to a node binary on other systems.".into());
    }
    let url = format!("https://nodejs.org/dist/{NODE_VER}/node-{NODE_VER}-win-x64.zip");
    set_setup(|s| {
        s.step = format!("Downloading Node.js (about {NODE_MB} MB)");
        s.pct = 0.0;
    });
    let agent = ureq::AgentBuilder::new().try_proxy_from_env(true).timeout_connect(Duration::from_secs(20)).timeout_read(Duration::from_secs(120)).build();
    let resp = agent.get(&url).call().map_err(|e| format!("Could not download Node.js. Check the internet connection. ({e})"))?;
    let total = resp.header("Content-Length").and_then(|v| v.parse::<u64>().ok()).unwrap_or(NODE_MB * 1024 * 1024);
    let mut rd = resp.into_reader().take(200 * 1024 * 1024);
    let mut data = Vec::with_capacity(total as usize);
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = rd.read(&mut buf).map_err(|e| format!("The Node.js download stopped. Press Get clip maker again. ({e})"))?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        let p = (data.len() as f64 / total as f64 * 100.0).min(99.0);
        set_setup(|s| s.pct = p);
    }
    if to_hex(&Sha256::digest(&data)) != NODE_SHA256 {
        return Err("The Node.js download was damaged (checksum differs). Press Get clip maker again.".into());
    }
    set_setup(|s| {
        s.step = "Unpacking Node.js".into();
        s.pct = -1.0;
    });
    let tmp = b.join("node.part");
    let _ = fs::remove_dir_all(&tmp);
    unzip_strip(&data, &tmp)?;
    let _ = fs::remove_dir_all(node_dir(b));
    fs::rename(&tmp, node_dir(b)).map_err(|e| format!("Could not unpack Node.js: {}", video::why(&e)))
}

/// Unzips into dir, dropping the archive's single top folder.
fn unzip_strip(data: &[u8], dir: &Path) -> Result<(), String> {
    let mut z = zip::ZipArchive::new(Cursor::new(data)).map_err(|e| format!("The Node.js download could not be opened: {e}"))?;
    for i in 0..z.len() {
        let mut f = z.by_index(i).map_err(|e| e.to_string())?;
        let Some(p) = f.enclosed_name() else { continue };
        let rel: PathBuf = p.components().skip(1).collect();
        if rel.as_os_str().is_empty() {
            continue;
        }
        let out = dir.join(rel);
        if f.is_dir() {
            fs::create_dir_all(&out).map_err(|e| video::why(&e))?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(|e| video::why(&e))?;
        }
        let mut w = fs::File::create(&out).map_err(|e| video::why(&e))?;
        std::io::copy(&mut f, &mut w).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Writes the embedded kit files. Without `force`, only when the app's copy changed.
fn write_kit(b: &Path, force: bool) -> Result<(), String> {
    let k = kit(b);
    let mark = k.join(".src-id");
    if !force && fs::read_to_string(&mark).is_ok_and(|s| s == src_id()) {
        return Ok(());
    }
    for (name, bytes) in KIT {
        let p = k.join(name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).map_err(|e| video::why(&e))?;
        }
        if fs::read(&p).ok().as_deref() != Some(*bytes) {
            fs::write(&p, bytes).map_err(|e| format!("Could not write the clip kit: {}", video::why(&e)))?;
        }
    }
    fs::write(mark, src_id()).map_err(|e| video::why(&e))
}

fn node_cmd(b: &Path) -> Command {
    let exe = node_exe(b);
    let mut cmd = Command::new(&exe);
    // npm runs install scripts with "node", so the private Node goes first on PATH.
    if let Some(dir) = exe.parent() {
        let old = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![dir.to_path_buf()];
        paths.extend(std::env::split_paths(&old));
        if let Ok(p) = std::env::join_paths(paths) {
            cmd.env("PATH", p);
        }
    }
    cmd.env("NO_COLOR", "1").env("FORCE_COLOR", "0").env("npm_config_update_notifier", "false");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

/// Runs a command, feeding each output line (split on CR and LF) to `line`. The error keeps the last lines.
fn run(mut cmd: Command, secs: u64, mut line: impl FnMut(&str)) -> Result<(), String> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("Could not start {}: {e}", cmd.get_program().to_string_lossy()))?;
    let (tx, rx) = mpsc::channel::<String>();
    let pump = |r: Box<dyn Read + Send>, tx: mpsc::Sender<String>| {
        std::thread::spawn(move || {
            let mut r = r;
            let mut buf = [0u8; 8192];
            let mut cur = Vec::new();
            loop {
                let n = match r.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                for &c in &buf[..n] {
                    if c == b'\n' || c == b'\r' {
                        if !cur.is_empty() {
                            let _ = tx.send(String::from_utf8_lossy(&cur).into_owned());
                            cur.clear();
                        }
                    } else {
                        cur.push(c);
                    }
                }
            }
            if !cur.is_empty() {
                let _ = tx.send(String::from_utf8_lossy(&cur).into_owned());
            }
        })
    };
    let t1 = pump(Box::new(child.stdout.take().unwrap()), tx.clone());
    let t2 = pump(Box::new(child.stderr.take().unwrap()), tx);
    let end = std::time::Instant::now() + Duration::from_secs(secs);
    let mut tail: Vec<String> = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(l) => {
                let l = strip_ansi(&l);
                line(&l);
                if !l.trim().is_empty() && !l.contains("memory") {
                    tail.push(l);
                    if tail.len() > 30 {
                        tail.remove(0);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if std::time::Instant::now() > end {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("It took longer than {} minutes, so it was stopped.", secs / 60));
        }
    }
    let _ = t1.join();
    let _ = t2.join();
    let st = child.wait().map_err(|e| e.to_string())?;
    if st.success() {
        Ok(())
    } else {
        Err(error_lines(&tail))
    }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' {
            if it.peek() == Some(&'[') {
                it.next();
                for d in it.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// The useful part of a failed run: lines from the first error on, else the last few lines.
fn error_lines(tail: &[String]) -> String {
    let keep: Vec<&String> = tail.iter().filter(|l| !l.starts_with("Rendered ") && !l.starts_with("Bundling ") && !l.trim_start().starts_with("at ")).collect();
    let from = keep.iter().position(|l| l.contains("Error") || l.contains("error")).unwrap_or(keep.len().saturating_sub(6));
    let s: Vec<&str> = keep[from..].iter().take(8).map(|l| l.trim()).collect();
    let s = s.join("\n");
    if s.is_empty() { "It stopped without a message.".into() } else { s.chars().take(1500).collect() }
}

fn percent_in(l: &str) -> Option<f64> {
    let i = l.find('%')?;
    let digits: String = l[..i].chars().rev().take_while(|c| c.is_ascii_digit() || *c == '.').collect::<Vec<_>>().into_iter().rev().collect();
    digits.parse::<f64>().ok().filter(|p| (0.0..=100.0).contains(p))
}

/// "Rendered 45/180" or "Encoded 45/180" as a fraction.
fn frames_in(l: &str) -> Option<(&str, f64)> {
    let (word, rest) = l.split_once(' ')?;
    if word != "Rendered" && word != "Encoded" && word != "Stitched" {
        return None;
    }
    let nums = rest.split([',', ' ']).next()?;
    let (a, b) = nums.split_once('/')?;
    let (a, b) = (a.parse::<f64>().ok()?, b.parse::<f64>().ok()?);
    (b > 0.0).then_some((word, a / b))
}

/// AI-written clips may only use the kit and the packages it ships with, and no network or storage.
pub fn code_ok(code: &str) -> Result<(), String> {
    if code.len() > 60_000 {
        return Err("The clip code is too long.".into());
    }
    if !(code.contains("export const Custom") || code.contains("export function Custom")) {
        return Err("The clip code does not export a component named Custom.".into());
    }
    const BANNED: [&str; 18] = [
        "require(", "import(", "fetch(", "XMLHttpRequest", "WebSocket", "EventSource", "eval(", "Function(", "process.", "child_process",
        "localStorage", "sessionStorage", "indexedDB", "document.cookie", "window.open", "navigator.", "<iframe", "<script",
    ];
    if let Some(b) = BANNED.iter().find(|b| code.contains(*b)) {
        return Err(format!("The clip code uses {b}, which is not allowed."));
    }
    let lower = code.to_lowercase();
    let mut at = 0;
    while let Some(i) = lower[at..].find("://") {
        let start = at + i;
        let before = &lower[..start];
        let scheme_ok = before.ends_with("http") || before.ends_with("https");
        if scheme_ok && !lower[start..].starts_with("://www.w3.org/") {
            return Err("The clip code links to a web address, which is not allowed.".into());
        }
        at = start + 3;
    }
    for m in modules(code) {
        let ok = ["react", "remotion", "../kit", "../shapes", "d3-geo", "topojson-client"].contains(&m.as_str())
            || m == "world-atlas/land-110m.json"
            || m == "world-atlas/countries-110m.json";
        if !ok {
            return Err(format!("The clip code imports \"{m}\", which the clip maker does not have."));
        }
    }
    Ok(())
}

/// Module names after `from` and bare `import`.
fn modules(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    for kw in ["from", "import"] {
        let mut at = 0;
        while let Some(i) = code[at..].find(kw) {
            let s = at + i + kw.len();
            at = s;
            let rest = code[s..].trim_start();
            let Some(q) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else { continue };
            if let Some(end) = rest[1..].find(q) {
                out.push(rest[1..1 + end].to_string());
            }
        }
    }
    out
}

fn jobs<T>(f: impl FnOnce(&mut HashMap<u64, Job>) -> T) -> T {
    let mut g = JOBS.lock().unwrap();
    f(g.get_or_insert_with(HashMap::new))
}

fn update(id: u64, f: impl FnOnce(&mut Job)) {
    jobs(|m| {
        if let Some(j) = m.get_mut(&id) {
            f(j)
        }
    });
}

pub fn job(id: u64) -> Option<Value> {
    jobs(|m| m.get(&id).cloned()).map(|j| {
        json!({"state": j.state, "pct": (j.pct * 10.0).round() / 10.0, "msg": j.msg, "error": j.error, "file": j.file,
            "still": j.still.as_ref().map(|_| format!("/api/clip/still/{id}"))})
    })
}

pub fn still(id: u64) -> Option<Vec<u8>> {
    jobs(|m| m.get(&id).and_then(|j| j.still.clone())).and_then(|p| fs::read(p).ok())
}

/// Starts a render. `v`: {template, props, code?, still?, folder?, file?}. A still is a preview PNG;
/// otherwise a transparent ProRes 4444 .mov is saved in the video's 05 Infographics, never over another file.
pub fn start(app: &Path, v: &Value) -> Result<u64, (u16, String)> {
    let b = base();
    if !ready(&b) {
        return Err((409, "The clip maker is not set up yet. Press Get clip maker first.".into()));
    }
    let tpl = v["template"].as_str().filter(|t| TEMPLATES.contains(t)).ok_or((400, "Unknown clip template.".to_string()))?.to_string();
    let mut props = v["props"].clone();
    if !props.is_object() || props.to_string().len() > 64 * 1024 {
        return Err((400, "The clip fields could not be read.".into()));
    }
    let code = if tpl == "Custom" {
        let c = v["code"].as_str().unwrap_or("").to_string();
        code_ok(&c).map_err(|e| (400, e))?;
        Some(c)
    } else {
        None
    };
    let is_still = v["still"] == true;
    props["background"] = json!(is_still);
    let target = if is_still {
        None
    } else {
        let d = video::dir(app, v["folder"].as_str().unwrap_or(""))?.join("05 Infographics");
        let file = pack::safe_file(v["file"].as_str().unwrap_or("")).filter(|f| f.ends_with(".mov")).ok_or((400, "The clip file name is not valid.".to_string()))?;
        Some((d, file))
    };
    let id = NEXT.fetch_add(1, Ordering::SeqCst);
    jobs(|m| {
        // Old finished jobs go, with their preview pictures.
        let old: Vec<u64> = m.iter().filter(|(_, j)| j.state != "running" && j.state != "queued" && now() > j.at + 3600).map(|(k, _)| *k).collect();
        for k in old {
            if let Some(p) = m.remove(&k).and_then(|j| j.still) {
                let _ = fs::remove_file(p);
            }
        }
        m.insert(id, Job { state: "queued", pct: 0.0, msg: "Waiting for the clip before it".into(), error: String::new(), file: String::new(), still: None, at: now() });
    });
    std::thread::spawn(move || {
        let r = render(&b, id, &tpl, &props, code.as_deref(), target);
        update(id, |j| {
            j.at = now();
            match r {
                Ok(()) => {
                    j.state = "done";
                    j.pct = 100.0;
                    j.msg = "Done".into();
                }
                Err(e) => {
                    j.state = "error";
                    j.error = e;
                }
            }
        });
    });
    Ok(id)
}

fn render(b: &Path, id: u64, tpl: &str, props: &Value, code: Option<&str>, target: Option<(PathBuf, String)>) -> Result<(), String> {
    let _g = RENDER.lock().unwrap_or_else(|e| e.into_inner());
    update(id, |j| {
        j.state = "running";
        j.msg = "Preparing".into();
    });
    write_kit(b, false)?;
    let k = kit(b);
    let work = b.join("work");
    fs::create_dir_all(&work).map_err(|e| video::why(&e))?;
    let pf = work.join(format!("props-{id}.json"));
    fs::write(&pf, props.to_string()).map_err(|e| video::why(&e))?;
    let custom = k.join(CUSTOM);
    if let Some(c) = code {
        fs::write(&custom, c).map_err(|e| video::why(&e))?;
    }
    let out = work.join(if target.is_some() { format!("clip-{id}.mov") } else { format!("still-{id}.png") });
    let mut cmd = node_cmd(b);
    cmd.arg(remotion_cli(b)).current_dir(&k);
    if target.is_some() {
        cmd.args(["render", "src/index.ts", tpl]).arg(&out).args(["--codec=prores", "--prores-profile=4444", "--pixel-format=yuva444p10le", "--image-format=png"]);
    } else {
        cmd.args(["still", "src/index.ts", tpl]).arg(&out).arg("--frame=-1");
    }
    cmd.arg(format!("--props={}", pf.display()));
    if let Some(br) = std::env::var_os("TERRA_CLIP_BROWSER") {
        cmd.arg(format!("--browser-executable={}", br.to_string_lossy()));
    }
    let r = run(cmd, 900, |l| {
        if let Some(p) = l.strip_prefix("Bundling ").and_then(percent_in) {
            update(id, |j| {
                j.msg = "Preparing".into();
                j.pct = p * 0.1;
            });
        } else if let Some((w, f)) = frames_in(l) {
            update(id, |j| {
                j.msg = if w == "Rendered" { "Drawing frames".into() } else { "Saving the video".into() };
                j.pct = if w == "Rendered" { 10.0 + f * 75.0 } else { 85.0 + f * 14.0 };
            });
        }
    });
    if code.is_some() {
        let _ = fs::write(&custom, KIT.iter().find(|(n, _)| *n == CUSTOM).unwrap().1);
    }
    let _ = fs::remove_file(&pf);
    r?;
    match target {
        None => {
            update(id, |j| j.still = Some(out));
            Ok(())
        }
        Some((dir, file)) => {
            fs::create_dir_all(&dir).map_err(|e| format!("Could not make 05 Infographics: {}", video::why(&e)))?;
            let _lk = video::LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let dest = video::free_path(&dir, &file, |p| p.exists());
            if fs::rename(&out, &dest).is_err() {
                fs::copy(&out, &dest).map_err(|e| format!("Could not save the clip: {}", video::why(&e)))?;
                let _ = fs::remove_file(&out);
            }
            let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            update(id, |j| j.file = name);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_check() {
        let ok = "import React from \"react\";\nimport { AbsoluteFill, useCurrentFrame } from 'remotion';\nimport { Stage, AMBER } from \"../kit\";\nexport const Custom = () => <svg xmlns=\"http://www.w3.org/2000/svg\" />;";
        assert!(code_ok(ok).is_ok());
        assert!(code_ok("export const Other = 1;").is_err());
        assert!(code_ok(&format!("{ok}\nfetch('x')")).unwrap_err().contains("fetch("));
        assert!(code_ok(&format!("{ok}\nimport fs from \"fs\";")).unwrap_err().contains("\"fs\""));
        assert!(code_ok(&format!("{ok}\nconst u = \"https://evil.example/a.png\";")).is_err());
        assert!(code_ok(&format!("{ok}\nconst x = import(\"remotion\");")).is_err());
    }

    #[test]
    fn progress_lines() {
        assert_eq!(frames_in("Rendered 45/180, time remaining: 3s"), Some(("Rendered", 0.25)));
        assert_eq!(frames_in("Encoded 90/90"), Some(("Encoded", 1.0)));
        assert_eq!(frames_in("Rendering is fun"), None);
        assert_eq!(percent_in("Bundling 63%"), Some(63.0));
        assert_eq!(percent_in("Downloading 12.5% done"), Some(12.5));
        assert_eq!(strip_ansi("\u{1b}[32mDone\u{1b}[0m"), "Done");
    }

    #[test]
    fn errors_keep_the_message() {
        let t: Vec<String> = ["Bundling 100%", "Rendered 3/90", "Error: Cannot read properties of undefined (reading 'x')", "    at Custom (bundle.js:1:2)", "An error occurred while rendering frame 3"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let e = error_lines(&t);
        assert!(e.starts_with("Error: Cannot read"));
        assert!(!e.contains("bundle.js"));
    }

    #[test]
    fn kit_ids_are_stable() {
        assert_eq!(deps_id(), deps_id());
        assert!(deps_id().starts_with(NODE_VER));
        assert_ne!(fnv(&[b"ab", b"c"]), fnv(&[b"a", b"bc"]));
    }
}

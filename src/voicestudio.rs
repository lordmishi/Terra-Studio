// Local VoiceStudio API integration (OpenAI-compatible speech & transcription on http://127.0.0.1:3900).
use super::*;
use std::process::{Child, Command, Stdio};
#[cfg(windows)]
use std::os::windows::process::CommandExt;

pub const DEFAULT_URL: &str = "http://127.0.0.1:3900";
pub const DEFAULT_MODEL: &str = "tts-1";
/// Local GPU TTS models (VoxCPM2/CosyVoice) hit VoiceStudio's 925s sidecar timeout on 2400-char chunks;
/// 450 chars (~2-3 sentences) keeps every request fast and prevents VRAM/timeout failures.
const VS_CHUNK: usize = 450;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

static START_LOCK: Mutex<()> = Mutex::new(());
static SERVER: Mutex<Option<Child>> = Mutex::new(None);

pub fn is_active(c: &Config) -> bool {
    c.voice_provider == "voicestudio"
        || (c.voice_provider.is_empty() && c.eleven_key.is_empty() && !c.voicestudio_voice.is_empty())
}

pub fn base_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_end_matches('/');
    let u = if trimmed.is_empty() { DEFAULT_URL } else { trimmed };
    let ok = u.starts_with("http://127.0.0.1:")
        || u == "http://127.0.0.1"
        || u.starts_with("http://localhost:")
        || u == "http://localhost"
        || u.starts_with("https://");
    if !ok || u.len() > 200 || u.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("Enter a valid VoiceStudio URL such as http://127.0.0.1:3900.".into());
    }
    Ok(u.to_string())
}

pub fn is_local_default(base: &str) -> bool {
    base == "http://127.0.0.1:3900" || base == "http://localhost:3900"
}

fn quick_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .timeout_connect(Duration::from_secs(2))
        .timeout_read(Duration::from_secs(3))
        .build()
}

fn probe_health(base: &str) -> Option<Value> {
    let v: Value = quick_agent()
        .get(&format!("{base}/health"))
        .call()
        .ok()?
        .into_json()
        .ok()?;
    (v["status"].as_str() == Some("ok")).then_some(v)
}

fn probe_voices_ready(base: &str) -> bool {
    quick_agent()
        .get(&format!("{base}/v1/audio/voices"))
        .call()
        .is_ok_and(|r| r.status() == 200)
}

#[cfg(windows)]
fn backend_paths() -> Option<(PathBuf, PathBuf, PathBuf)> {
    let appdata = PathBuf::from(std::env::var_os("APPDATA")?);
    let runtime = appdata.join("VoiceStudio").join("runtime").join("project");
    let py = [
        runtime.join(".venv").join("Scripts").join("python.exe"),
        runtime.join(".venv").join("Scripts").join("pythonw.exe"),
    ]
    .into_iter()
    .find(|p| p.is_file())?;
    let mut main_candidates = vec![runtime.join("backend").join("main.py")];
    if let Some(pf) = std::env::var_os("ProgramFiles") {
        main_candidates.push(PathBuf::from(pf).join("VoiceStudio").join("resources").join("backend").join("main.py"));
    }
    let main_py = main_candidates.into_iter().find(|p| p.is_file())?;
    let cwd = main_py.parent()?.to_path_buf();
    Some((py, main_py, cwd))
}

#[cfg(not(windows))]
fn backend_paths() -> Option<(PathBuf, PathBuf, PathBuf)> {
    None
}

pub fn app_exe() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let mut cands = Vec::new();
        if let Some(pf) = std::env::var_os("ProgramFiles") {
            cands.push(PathBuf::from(pf).join("VoiceStudio").join("VoiceStudio.exe"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            cands.push(PathBuf::from(local).join("Programs").join("VoiceStudio").join("VoiceStudio.exe"));
        }
        return cands.into_iter().find(|p| p.is_file());
    }
    #[cfg(not(windows))]
    {
        None
    }
}

fn spawn_background() -> Result<bool, String> {
    let mut slot = SERVER.lock().unwrap();
    if let Some(child) = slot.as_mut() {
        if child.try_wait().ok().flatten().is_none() {
            return Ok(false);
        }
        *slot = None;
    }

    if let Some((py, main_py, cwd)) = backend_paths() {
        let mut cmd = Command::new(&py);
        cmd.arg(&main_py)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let child = cmd
            .spawn()
            .map_err(|e| format!("Could not start VoiceStudio backend: {e}"))?;
        *slot = Some(child);
        return Ok(true);
    }

    if let Some(exe) = app_exe() {
        let mut cmd = Command::new(&exe);
        if let Some(dir) = exe.parent() {
            cmd.current_dir(dir);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = cmd
            .spawn()
            .map_err(|e| format!("Could not start VoiceStudio: {e}"))?;
        *slot = Some(child);
        return Ok(true);
    }

    Err("VoiceStudio is not running and could not be found on this PC. Install VoiceStudio or open it manually first.".into())
}

/// Ensures local VoiceStudio on http://127.0.0.1:3900 is running, launching its headless backend if needed.
pub fn ensure_running(raw_url: &str) -> Result<bool, String> {
    let base = base_url(raw_url)?;
    if let Some(h) = probe_health(&base) {
        if !h["engine"].is_null() || probe_voices_ready(&base) {
            return Ok(false);
        }
    }
    if !is_local_default(&base) {
        return Ok(false);
    }

    let _guard = START_LOCK.lock().unwrap();
    if let Some(h) = probe_health(&base) {
        if !h["engine"].is_null() || probe_voices_ready(&base) {
            return Ok(false);
        }
    }

    let launched = spawn_background()?;
    for step in 0..90 {
        std::thread::sleep(Duration::from_millis(500));
        if let Some(h) = probe_health(&base) {
            let dev_ready = h["device"].as_str().is_some_and(|d| d != "unknown");
            let engine_ready = !h["engine"].is_null();
            if (engine_ready || dev_ready || step >= 30) && probe_voices_ready(&base) {
                return Ok(launched);
            }
        }
        if step >= 4 {
            let mut slot = SERVER.lock().unwrap();
            if let Some(child) = slot.as_mut() {
                if let Ok(Some(status)) = child.try_wait() {
                    *slot = None;
                    if probe_health(&base).is_none() {
                        return Err(format!(
                            "VoiceStudio backend stopped unexpectedly ({status}). Try opening the VoiceStudio app directly."
                        ));
                    }
                }
            }
        }
    }
    Err("VoiceStudio took too long to start in the background. Try opening the VoiceStudio app once.".into())
}

/// Stops a headless VoiceStudio backend started by Terra Studio (never touches a user-opened app).
pub fn stop_owned() {
    if let Ok(mut slot) = SERVER.lock() {
        if let Some(mut child) = slot.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Opens the VoiceStudio desktop window so the user can manage or record voice profiles.
pub fn open_app() -> Result<(), String> {
    let exe = app_exe().ok_or_else(|| "VoiceStudio.exe was not found on this PC.".to_string())?;
    // Release port 3900 if our headless backend holds it so the desktop UI can claim it cleanly.
    stop_owned();
    let mut cmd = Command::new(&exe);
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Could not open VoiceStudio: {e}"))?;
    Ok(())
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(960))
        .build()
}

fn friendly(url: &str, code: u16, body: &str) -> String {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["detail"].as_str().or(v["error"]["message"].as_str()).map(String::from))
        .unwrap_or_else(|| body.chars().take(240).collect());
    if detail.contains("longer than 30s") {
        return "Your VoiceStudio voice profile's reference clip is longer than 30 seconds, which CosyVoice does not accept. Trim the reference audio in VoiceStudio to 10-20 seconds, or switch TTS engine.".into();
    }
    match code {
        0 => format!("Could not reach VoiceStudio at {url}. Make sure VoiceStudio is installed and running ({detail})."),
        400 | 422 => format!("VoiceStudio did not accept the request: {detail}"),
        404 => format!("Voice or engine was not found in VoiceStudio ({detail})."),
        409 => format!("VoiceStudio found multiple voices with that name: {detail}"),
        _ => format!("VoiceStudio failed (code {code}): {detail}"),
    }
}

/// Splits text at paragraph, then sentence boundaries into <= 450-character pieces for local TTS.
pub fn chunks(parts: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let push = |piece: &str, cur: &mut String, out: &mut Vec<String>| {
        if !cur.is_empty() && cur.chars().count() + piece.chars().count() + 2 > VS_CHUNK {
            out.push(std::mem::take(cur));
        }
        if !cur.is_empty() {
            cur.push_str(" ");
        }
        cur.push_str(piece);
    };
    for part in parts {
        for para in part.split('\n').map(str::trim).filter(|p| !p.is_empty()) {
            if para.chars().count() <= VS_CHUNK {
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

fn strip_id3(mp3: &[u8]) -> &[u8] {
    if mp3.len() > 10 && &mp3[..3] == b"ID3" {
        let size = mp3[6..10].iter().fold(0usize, |n, x| (n << 7) | (*x & 0x7F) as usize);
        if 10 + size < mp3.len() {
            return &mp3[10 + size..];
        }
    }
    mp3
}

/// Checks /health and loads available voices and installed TTS engines from VoiceStudio.
pub fn info(raw_url: &str) -> Result<Value, String> {
    let base = base_url(raw_url)?;
    let auto_started = ensure_running(&base)?;
    let health: Value = match agent().get(&format!("{base}/health")).call() {
        Ok(r) => r.into_json().map_err(|e| friendly(&base, 0, &e.to_string()))?,
        Err(ureq::Error::Status(code, r)) => return Err(friendly(&base, code, &r.into_string().unwrap_or_default())),
        Err(e) => return Err(friendly(&base, 0, &e.to_string())),
    };
    let data: Value = match agent().get(&format!("{base}/v1/audio/voices")).call() {
        Ok(r) => r.into_json().map_err(|e| friendly(&base, 0, &e.to_string()))?,
        Err(ureq::Error::Status(code, r)) => return Err(friendly(&base, code, &r.into_string().unwrap_or_default())),
        Err(e) => return Err(friendly(&base, 0, &e.to_string())),
    };

    let mut voices: Vec<Value> = data["voices"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| {
            let id = v["voice_id"].as_str().or(v["id"].as_str()).unwrap_or("");
            let name = v["name"].as_str().unwrap_or(id);
            let kind = v["type"].as_str().unwrap_or("");
            let lang = v["language"].as_str().unwrap_or("");
            let desc = [kind, lang].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", ");
            json!({"id": id, "name": name, "kind": kind, "desc": desc})
        })
        .collect();
    // Put custom cloned profiles first so narrator voices appear at the top.
    voices.sort_by_key(|v| if v["kind"] == "profile" { 0 } else { 1 });

    let mut engines: Vec<Value> = vec![json!({
        "id": DEFAULT_MODEL,
        "name": format!("Active engine in VoiceStudio ({})", health["engine"].as_str().unwrap_or("auto")),
        "available": true
    })];
    for e in data["engines"].as_array().into_iter().flatten() {
        if e["available"].as_bool() != Some(true) {
            continue;
        }
        let id = e["id"].as_str().unwrap_or("");
        if id.is_empty() {
            continue;
        }
        let name = e["display_name"].as_str().unwrap_or(id);
        engines.push(json!({"id": id, "name": name, "available": true}));
    }

    Ok(json!({
        "ok": true,
        "url": base,
        "auto_started": auto_started,
        "engine": health["engine"],
        "version": health["version"],
        "voices": voices,
        "models": engines,
    }))
}

pub fn speak(c: &Config, parts: &[String], voice_override: &str) -> Result<eleven::Spoken, String> {
    let base = base_url(&c.voicestudio_url)?;
    ensure_running(&base)?;
    let voice = if voice_override.is_empty() { c.voicestudio_voice.as_str() } else { voice_override };
    let voice = if voice.is_empty() { "default" } else { voice };
    let model = if c.voicestudio_model.is_empty() { DEFAULT_MODEL } else { c.voicestudio_model.as_str() };
    let pieces = chunks(parts);
    if pieces.is_empty() {
        return Err("The script is empty.".into());
    }

    let mut audio: Vec<u8> = Vec::new();
    for (i, text) in pieces.iter().enumerate() {
        let body = json!({
            "model": model,
            "input": text,
            "voice": voice,
            "response_format": "mp3",
            "language": "en"
        });
        match agent()
            .post(&format!("{base}/v1/audio/speech"))
            .set("content-type", "application/json")
            .send_json(body)
        {
            Ok(r) => {
                let mut buf = Vec::new();
                r.into_reader()
                    .take(80 * 1024 * 1024)
                    .read_to_end(&mut buf)
                    .map_err(|e| friendly(&base, 0, &e.to_string()))?;
                if buf.is_empty() {
                    return Err("VoiceStudio returned empty audio.".into());
                }
                audio.extend_from_slice(if i == 0 { &buf } else { strip_id3(&buf) });
            }
            Err(ureq::Error::Status(code, r)) => {
                let msg = friendly(&base, code, &r.into_string().unwrap_or_default());
                return Err(if i == 0 { msg } else { format!("{msg} (stopped at part {} of {})", i + 1, pieces.len()) });
            }
            Err(e) => return Err(friendly(&base, 0, &e.to_string())),
        }
    }

    let chars: usize = pieces.iter().map(|p| p.chars().count()).sum();
    Ok(eleven::Spoken { audio, chars, chunks: pieces.len() })
}

/// Calls VoiceStudio's local Whisper ASR (/v1/audio/transcriptions) to time subtitles;
/// returns None if no ASR engine is active so proportional cues can be used as fallback.
fn transcribe_cues(base: &str, mp3: &[u8]) -> Option<Vec<pack::Cue>> {
    let boundary = format!("----TerraVoiceStudio{}", now());
    let mut body = Vec::with_capacity(mp3.len() + 512);
    let mut push_field = |name: &str, val: &str| {
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{val}\r\n").as_bytes());
    };
    push_field("model", "whisper-1");
    push_field("language", "en");
    push_field("response_format", "verbose_json");
    push_field("timestamp_granularities[]", "segment");
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"act.mp3\"\r\nContent-Type: audio/mpeg\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(mp3);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let resp = agent()
        .post(&format!("{base}/v1/audio/transcriptions"))
        .set("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
        .send_bytes(&body)
        .ok()?;
    let v: Value = resp.into_json().ok()?;
    let cues = cues_from_transcription(&v);
    (!cues.is_empty()).then_some(cues)
}

pub fn cues_from_transcription(v: &Value) -> Vec<pack::Cue> {
    let mut out = Vec::new();
    for seg in v["segments"].as_array().into_iter().flatten() {
        let Some(start) = seg["start"].as_f64() else { continue };
        let Some(end) = seg["end"].as_f64() else { continue };
        let text = seg["text"].as_str().unwrap_or("").trim();
        if text.is_empty() || end <= start {
            continue;
        }
        out.push(pack::Cue { start, end, text: text.to_string() });
    }
    out
}

/// Splits text into sentence cues distributed proportionally over total audio duration.
pub fn proportional_cues(text: &str, total_seconds: f64) -> Vec<pack::Cue> {
    let mut sentences: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        cur.push(ch);
        if matches!(ch, '.' | '!' | '?' | '\n') {
            let s = cur.split_whitespace().collect::<Vec<_>>().join(" ");
            if !s.is_empty() {
                sentences.push(s);
            }
            cur.clear();
        }
    }
    let tail = cur.split_whitespace().collect::<Vec<_>>().join(" ");
    if !tail.is_empty() {
        sentences.push(tail);
    }
    if sentences.is_empty() || total_seconds <= 0.0 {
        return Vec::new();
    }
    let weights: Vec<f64> = sentences.iter().map(|s| s.chars().count().max(1) as f64).collect();
    let sum: f64 = weights.iter().sum();
    let mut t: f64 = 0.0;
    sentences
        .into_iter()
        .zip(weights)
        .map(|(s, w)| {
            let dur = total_seconds * (w / sum);
            let cue = pack::Cue {
                start: (t * 1000.0).round() / 1000.0,
                end: ((t + dur) * 1000.0).round() / 1000.0,
                text: s,
            };
            t += dur;
            cue
        })
        .collect()
}

pub struct ActAudio {
    pub audio: Vec<u8>,
    pub seconds: f64,
    pub chars: usize,
    pub chunks: usize,
    pub cues: Vec<pack::Cue>,
}

pub fn speak_act(c: &Config, text: &str) -> Result<ActAudio, String> {
    let spoken = speak(c, &[text.to_string()], "")?;
    let seconds = eleven::mp3_seconds(&spoken.audio).unwrap_or_else(|| {
        let words = text.split_whitespace().count().max(1) as f64;
        (words / 2.35).max(1.0)
    });
    let base = base_url(&c.voicestudio_url)?;
    let cues = transcribe_cues(&base, &spoken.audio).unwrap_or_else(|| proportional_cues(text, seconds));
    Ok(ActAudio {
        audio: spoken.audio,
        seconds,
        chars: spoken.chars,
        chunks: spoken.chunks,
        cues,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_validates_local_and_https_endpoints() {
        assert_eq!(base_url("").unwrap(), "http://127.0.0.1:3900");
        assert_eq!(base_url("http://127.0.0.1:3900/").unwrap(), "http://127.0.0.1:3900");
        assert_eq!(base_url("http://localhost:3900").unwrap(), "http://localhost:3900");
        assert!(base_url("ftp://127.0.0.1:3900").is_err());
        assert!(base_url("http://example.com:3900").is_err());
    }

    #[test]
    fn proportional_and_transcription_cues_cover_duration() {
        let cues = proportional_cues("First beast waited. Then it struck!", 10.0);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start, 0.0);
        assert!((cues[1].end - 10.0).abs() < 0.01);

        let parsed = cues_from_transcription(&json!({
            "segments": [
                {"start": 0.1, "end": 2.4, "text": " Sarkastodon stalked the forest. "},
                {"start": 2.5, "end": 5.0, "text": "It weighed over a thousand pounds."}
            ]
        }));
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].text, "Sarkastodon stalked the forest.");
    }

    #[test]
    fn chunks_keep_local_tts_pieces_under_limit() {
        let long = "First sentence here. Second sentence is longer. ".repeat(20);
        let c = chunks(&[long]);
        assert!(c.len() >= 2);
        assert!(c.iter().all(|p| p.chars().count() <= VS_CHUNK));
    }

    #[test]
    fn local_default_check_matches_expected_urls() {
        assert!(is_local_default("http://127.0.0.1:3900"));
        assert!(is_local_default("http://localhost:3900"));
        assert!(!is_local_default("http://127.0.0.1:8080"));
        assert!(!is_local_default("https://voice.example.com"));
    }
}

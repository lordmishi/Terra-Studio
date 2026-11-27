// Terra Studio for Claude: a small MCP server that Claude Desktop starts over stdio.
// It reads the app's own files in %APPDATA%\TerraStudio and can only add ideas (to an inbox the app picks up).
// No network, no keys, nothing is deleted.
#![cfg_attr(windows, windows_subsystem = "windows")]

use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

const RULES: &str = include_str!("../assets/terra-skill.md");
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn app_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("TerraStudio")
}

fn load(p: &Path) -> Value {
    fs::read_to_string(p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null)
}

fn long_videos(data: &Value) -> Vec<Value> {
    data["videos"].as_array().cloned().unwrap_or_default().into_iter().filter(|v| v["dur"].as_f64().unwrap_or(0.0) >= 120.0).collect()
}

fn video_row(v: &Value) -> Value {
    let views = v["v"].as_f64().unwrap_or(0.0);
    let hours = v["h"].as_f64().unwrap_or(0.0);
    let dur = v["dur"].as_f64().unwrap_or(0.0);
    let avg_min = if views > 0.0 { hours * 60.0 / views } else { 0.0 };
    let watched = if dur > 0.0 { (avg_min * 60.0 / dur * 1000.0).round() / 10.0 } else { 0.0 };
    json!({
        "title": v["t"].as_str().unwrap_or("").trim(),
        "published": v["d"],
        "minutes": (dur / 6.0).round() / 10.0,
        "views": views,
        "ctr_percent": v["ctr"],
        "impressions": v["imp"],
        "avg_view_minutes": (avg_min * 10.0).round() / 10.0,
        "watched_percent": watched,
        "subs_gained": v["s"],
    })
}

fn sum_days(daily: &[Value], from: usize, to: usize) -> f64 {
    daily.iter().rev().skip(from).take(to - from).map(|d| d[1].as_f64().unwrap_or(0.0)).sum()
}

fn overview(dir: &Path) -> Value {
    let data = load(&dir.join("data.json"));
    if data.is_null() {
        return json!({"error": "No channel data yet. Open Terra Studio and press Update Studio data."});
    }
    let daily = data["daily"].as_array().cloned().unwrap_or_default();
    let (a, b) = (sum_days(&daily, 0, 30), sum_days(&daily, 30, 60));
    let mut lf = long_videos(&data);
    lf.sort_by(|x, y| y["v"].as_f64().unwrap_or(0.0).total_cmp(&x["v"].as_f64().unwrap_or(0.0)));
    let top: Vec<Value> = lf.iter().take(10).map(video_row).collect();
    let mut recent = lf.clone();
    recent.sort_by(|x, y| y["d"].as_str().unwrap_or("").cmp(x["d"].as_str().unwrap_or("")));
    let newest: Vec<Value> = recent.iter().take(8).map(video_row).collect();
    json!({
        "channel": "Terra Studio Channel",
        "data_range": data["range"],
        "currency": data["currency"],
        "totals": data["totals"],
        "views_last_30_days": a,
        "views_previous_30_days": b,
        "long_videos": lf.len(),
        "top_10_by_views": top,
        "newest_8": newest,
        "note": "CTR and impressions come from the YouTube Studio export, so the newest videos may have none yet."
    })
}

fn videos(dir: &Path) -> Value {
    let data = load(&dir.join("data.json"));
    let mut lf = long_videos(&data);
    lf.sort_by(|x, y| y["d"].as_str().unwrap_or("").cmp(x["d"].as_str().unwrap_or("")));
    json!({"videos": lf.iter().map(video_row).collect::<Vec<_>>()})
}

const STATUS: [(&str, &str); 6] = [("ide", "idea"), ("naskah", "script"), ("rekam", "voice over"), ("edit", "editing"), ("siap", "ready"), ("tayang", "published")];

fn calendar(dir: &Path) -> Value {
    let last = load(&dir.join("last.json"));
    let list: Vec<Value> = last["plans"]["data"].as_array().cloned().unwrap_or_default().iter().map(|p| {
        let s = p["status"].as_str().unwrap_or("");
        json!({"date": p["date"], "title": p["title"], "type": p["type"], "status": STATUS.iter().find(|x| x.0 == s).map(|x| x.1).unwrap_or(s)})
    }).collect();
    json!({"calendar": list})
}

fn ideas(dir: &Path) -> Value {
    let last = load(&dir.join("last.json"));
    let saved: Vec<Value> = last["ideas"]["data"]["ideas"].as_array().cloned().unwrap_or_default().iter()
        .map(|x| json!({"title": x["title"], "subject": x["subject"], "hook": x["hook"]})).collect();
    let inbox = load(&dir.join("claude-inbox.json"));
    json!({"saved_ideas": saved, "saved_at_ms": last["ideas"]["at"], "waiting_from_claude": inbox["ideas"].as_array().cloned().unwrap_or_default()})
}

fn norm(s: &str) -> String {
    s.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

fn check_animal(dir: &Path, name: &str) -> Value {
    let n = norm(name);
    if n.len() < 3 {
        return json!({"error": "Give an animal name of at least 3 letters."});
    }
    let data = load(&dir.join("data.json"));
    let last = load(&dir.join("last.json"));
    let map = &last["_animals"]["data"];
    let hit = |title: &str| {
        let t = title.trim();
        if norm(t).contains(&n) {
            return true;
        }
        let a = &map[t];
        let mut names: Vec<String> = a["n"].as_array().cloned().unwrap_or_default().iter().filter_map(|x| x.as_str().map(norm)).collect();
        if let Some(g) = a["g"].as_str() { names.push(norm(g)); }
        names.iter().any(|x| !x.is_empty() && (x == &n || x.contains(&n) || n.contains(x.as_str())))
    };
    let made: Vec<Value> = long_videos(&data).iter().filter(|v| hit(v["t"].as_str().unwrap_or(""))).map(video_row).collect();
    let planned: Vec<Value> = last["plans"]["data"].as_array().cloned().unwrap_or_default().into_iter()
        .filter(|p| p["status"] != "tayang" && hit(p["title"].as_str().unwrap_or(""))).map(|p| json!({"title": p["title"], "date": p["date"]})).collect();
    json!({"animal": name, "already_published": made, "already_in_calendar": planned, "is_new": made.is_empty() && planned.is_empty()})
}

fn add_idea(dir: &Path, a: &Value) -> Value {
    let title = a["title"].as_str().unwrap_or("").trim().to_string();
    let subject = a["subject"].as_str().unwrap_or("").trim().to_string();
    if title.is_empty() || subject.is_empty() {
        return json!({"error": "title and subject are required."});
    }
    let s = |k: &str, max: usize| a[k].as_str().unwrap_or("").trim().chars().take(max).collect::<String>();
    let idea = json!({
        "title": title.chars().take(120).collect::<String>(), "subject": subject.chars().take(80).collect::<String>(),
        "genus": s("genus", 60),
        "names": a["names"].as_array().cloned().unwrap_or_default().into_iter().filter_map(|x| x.as_str().map(|y| y.trim().to_lowercase())).filter(|x| !x.is_empty()).take(6).collect::<Vec<_>>(),
        "mammal": a["mammal"].as_bool().unwrap_or(false), "predator": a["predator"].as_bool().unwrap_or(false),
        "threat": a["threat"].as_bool().unwrap_or(false), "size": a["size"].as_bool().unwrap_or(false),
        "hook": s("hook", 400), "why": s("why", 400), "from": "claude",
    });
    let _ = fs::create_dir_all(dir);
    let path = dir.join("claude-inbox.json");
    let mut inbox = load(&path);
    if !inbox["ideas"].is_array() {
        inbox = json!({"ideas": []});
    }
    let list = inbox["ideas"].as_array_mut().unwrap();
    if list.iter().any(|x| x["title"].as_str().map(|t| t.eq_ignore_ascii_case(&title)).unwrap_or(false)) {
        return json!({"ok": true, "note": "This idea was already waiting for Terra Studio."});
    }
    if list.len() >= 30 {
        return json!({"error": "30 ideas are already waiting. Open Terra Studio so it takes them in, then try again."});
    }
    list.push(idea);
    let tmp = path.with_extension("tmp");
    match fs::write(&tmp, inbox.to_string()).and_then(|_| fs::rename(&tmp, &path)) {
        Ok(()) => json!({"ok": true, "note": "Added. It shows in the Ideas tab of Terra Studio the next time the app opens or reloads (marked From Claude)."}),
        Err(e) => json!({"error": format!("Could not save the idea: {e}")}),
    }
}

fn tools() -> Value {
    let none = json!({"type": "object", "properties": {}});
    json!([
        {"name": "get_channel_overview", "description": "Channel numbers from Terra Studio: totals, views in the last 30 days vs the 30 before, top 10 long videos by views and the 8 newest, with CTR, average view minutes and percent watched.", "inputSchema": none, "annotations": {"readOnlyHint": true}},
        {"name": "list_videos", "description": "Every published long video (2 minutes or more) with date, length, views, CTR, impressions, average view minutes and percent watched, newest first.", "inputSchema": none, "annotations": {"readOnlyHint": true}},
        {"name": "get_calendar", "description": "Videos planned or in progress in Terra Studio's calendar, with date and status (idea, script, voice over, editing, ready, published).", "inputSchema": none, "annotations": {"readOnlyHint": true}},
        {"name": "get_ideas", "description": "The idea list saved in Terra Studio's Ideas tab, plus ideas from Claude that wait for the app to take them in.", "inputSchema": none, "annotations": {"readOnlyHint": true}},
        {"name": "check_animal", "description": "Check whether the channel already has a published video or a calendar entry about an animal (genus or common name). Use this before suggesting any topic.", "inputSchema": {"type": "object", "properties": {"name": {"type": "string", "description": "Genus or common name, e.g. Smilodon or giant lion"}}, "required": ["name"]}, "annotations": {"readOnlyHint": true}},
        {"name": "get_channel_rules", "description": "The channel's fixed house rules: topics, titles, thumbnails, scripts of 3,000 to 5,000 words in 5 acts, the pre-upload checklist and the CTR rule. Read these before suggesting topics or writing anything.", "inputSchema": none, "annotations": {"readOnlyHint": true}},
        {"name": "add_idea", "description": "Send one approved video idea to Terra Studio's Ideas tab. Only call this after the owner agreed to the idea, and after check_animal says the animal is new.", "inputSchema": {"type": "object", "properties": {
            "title": {"type": "string", "description": "Video title, under 60 characters, naming the animal"},
            "subject": {"type": "string", "description": "The animal's usual name"},
            "genus": {"type": "string", "description": "Scientific genus, one word"},
            "names": {"type": "array", "items": {"type": "string"}, "description": "Other common names, lowercase"},
            "hook": {"type": "string", "description": "One-sentence cold-open hook"},
            "why": {"type": "string", "description": "One sentence on why it can perform for this channel"},
            "mammal": {"type": "boolean"}, "predator": {"type": "boolean"},
            "threat": {"type": "boolean", "description": "True only if the evidence supports a danger to humans or our ancestors"},
            "size": {"type": "boolean", "description": "True if the title states a size, weight or record"}
        }, "required": ["title", "subject", "mammal", "predator", "threat", "size"]}}
    ])
}

fn call(dir: &Path, name: &str, args: &Value) -> Result<String, String> {
    let v = match name {
        "get_channel_overview" => overview(dir),
        "list_videos" => videos(dir),
        "get_calendar" => calendar(dir),
        "get_ideas" => ideas(dir),
        "check_animal" => check_animal(dir, args["name"].as_str().unwrap_or("")),
        "get_channel_rules" => return Ok(RULES.to_string()),
        "add_idea" => add_idea(dir, args),
        _ => return Err(format!("Unknown tool: {name}")),
    };
    if let Some(e) = v["error"].as_str() {
        return Err(e.to_string());
    }
    Ok(serde_json::to_string_pretty(&v).unwrap_or_default())
}

fn reply(id: &Value, msg: &str, dir: &Path) -> Option<Value> {
    let req: Value = serde_json::from_str(msg).ok()?;
    let method = req["method"].as_str().unwrap_or("");
    let _ = id;
    let id = req.get("id").cloned()?; // notifications get no answer
    let result = match method {
        "initialize" => json!({
            "protocolVersion": req["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "terra-studio", "version": VERSION},
            "instructions": "Terra Studio channel data and tools. Read get_channel_rules first. Before suggesting a topic, call check_animal. Use add_idea only after the owner approves an idea."
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "resources/list" => json!({"resources": []}),
        "prompts/list" => json!({"prompts": []}),
        "tools/call" => {
            let name = req["params"]["name"].as_str().unwrap_or("");
            let empty = json!({});
            let args = req["params"].get("arguments").unwrap_or(&empty);
            match call(dir, name, args) {
                Ok(text) => json!({"content": [{"type": "text", "text": text}]}),
                Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
            }
        }
        _ => return Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("Method not found: {method}")}})),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn main() {
    let dir = std::env::var_os("TERRA_DIR").map(PathBuf::from).unwrap_or_else(app_dir);
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(r) = reply(&Value::Null, &line, &dir) {
            let _ = writeln!(out, "{r}");
            let _ = out.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inbox_and_check() {
        let dir = std::env::temp_dir().join(format!("terra-mcp-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("data.json"), r#"{"daily":[["2026-11-01",5]],"videos":[{"t":"The Saber-Toothed Predator of the Steppe","d":"2026-11-21","dur":700,"v":10,"h":1}]}"#).unwrap();
        fs::write(dir.join("last.json"), r#"{"plans":{"data":[{"title":"Megacerops: Titan of the Plains","date":"2026-12-19","status":"ide","type":"video"}]},"_animals":{"data":{"The Saber-Toothed Predator of the Steppe":{"g":"Smilodon","n":["sabertooth"]}}}}"#).unwrap();
        assert_eq!(check_animal(&dir, "Smilodon")["is_new"], false);
        assert_eq!(check_animal(&dir, "megacerops")["already_in_calendar"].as_array().unwrap().len(), 1);
        assert_eq!(check_animal(&dir, "Epicyon")["is_new"], true);
        assert!(add_idea(&dir, &json!({"title":"Epicyon","subject":"Epicyon","mammal":true,"predator":true,"threat":false,"size":true}))["ok"].as_bool().unwrap());
        assert_eq!(load(&dir.join("claude-inbox.json"))["ideas"].as_array().unwrap().len(), 1);
        let r = reply(&Value::Null, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#, &dir).unwrap();
        assert_eq!(r["result"]["tools"].as_array().unwrap().len(), 7);
        assert!(reply(&Value::Null, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, &dir).is_none());
        let _ = fs::remove_dir_all(&dir);
    }
}

//! Settings and browsing history, stored as JSON in the app data directory.

use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::Url;

const MAX_HISTORY: usize = 5000;
/// Re-visits of the same URL within this window update the last entry instead of adding one.
const DEDUPE_MS: u64 = 60_000;
const DEFAULT_HOME: &str = "https://duckduckgo.com";

/// A user-defined search engine. `url` contains `%s` where the search terms go.
#[derive(Clone, Serialize, Deserialize)]
pub struct Engine {
    pub id: String,
    pub name: String,
    pub url: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// "system" | "light" | "dark"
    pub theme: String,
    /// "duckduckgo" | "google" | "bing" | "brave" | "ecosia", or the id of a custom engine.
    pub search: String,
    pub home: String,
    pub save_history: bool,
    pub custom_engines: Vec<Engine>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            search: "duckduckgo".into(),
            home: DEFAULT_HOME.into(),
            save_history: true,
            custom_engines: vec![],
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Entry {
    pub url: String,
    pub title: String,
    /// Milliseconds since the Unix epoch.
    pub time: u64,
}

pub struct Data {
    dir: Option<PathBuf>,
    settings: Mutex<Settings>,
    history: Mutex<Vec<Entry>>,
}

/// Percent-encodes search terms for a query string (spaces become `+`).
fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The search terms if `url` is a results page of a well-known search engine.
fn search_query(url: &str) -> Option<String> {
    let u = Url::parse(url).ok()?;
    let host = u.host_str()?;
    let known = ["duckduckgo.com", "bing.com", "brave.com", "ecosia.org"]
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
        || host.starts_with("www.google.")
        || host.starts_with("google.");
    if !known {
        return None;
    }
    u.query_pairs().find(|(k, _)| k == "q").map(|(_, v)| v.into_owned()).filter(|v| !v.is_empty())
}

/// One address-bar suggestion drawn from the history.
#[derive(Serialize)]
pub struct Suggestion {
    pub url: String,
    pub title: String,
    /// Set when the page is a search results page: the search terms.
    pub query: Option<String>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Data {
    pub fn load(dir: Option<PathBuf>) -> Self {
        let read = |name: &str| dir.as_ref().and_then(|d| fs::read_to_string(d.join(name)).ok());
        let settings: Settings = read("settings.json")
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let history: Vec<Entry> = read("history.json")
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Self { dir, settings: Mutex::new(settings), history: Mutex::new(history) }
    }

    fn save<T: Serialize>(&self, name: &str, value: &T) {
        let Some(dir) = &self.dir else { return };
        let _ = fs::create_dir_all(dir);
        let Ok(text) = serde_json::to_vec(value) else { return };
        let tmp = dir.join(format!("{name}.tmp"));
        if fs::write(&tmp, text).is_ok() {
            let _ = fs::rename(&tmp, dir.join(name));
        }
    }

    // --- Settings -------------------------------------------------------------

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    /// Validates, stores and returns the settings actually saved.
    pub fn set_settings(&self, mut s: Settings) -> Settings {
        s.home = s.home.trim().to_string();
        if s.home.is_empty() {
            s.home = DEFAULT_HOME.into();
        }
        if !matches!(s.theme.as_str(), "light" | "dark") {
            s.theme = "system".into();
        }
        // Keep only well-formed custom engines: a name, an http(s) URL and a %s placeholder.
        s.custom_engines.retain(|e| {
            !e.id.trim().is_empty()
                && !e.name.trim().is_empty()
                && (e.url.starts_with("http://") || e.url.starts_with("https://"))
                && e.url.contains("%s")
        });
        let known = matches!(s.search.as_str(), "duckduckgo" | "google" | "bing" | "brave" | "ecosia")
            || s.custom_engines.iter().any(|e| e.id == s.search);
        if !known {
            s.search = "duckduckgo".into();
        }
        *self.settings.lock().unwrap() = s.clone();
        self.save("settings.json", &s);
        s
    }

    /// URL of a web search for `query` on the chosen search engine.
    pub fn search_url(&self, query: &str) -> String {
        let (engine, custom) = {
            let s = self.settings.lock().unwrap();
            (s.search.clone(), s.custom_engines.iter().find(|e| e.id == s.search).cloned())
        };
        if let Some(c) = custom {
            return c.url.replace("%s", &encode(query));
        }
        let base = match engine.as_str() {
            "google" => "https://www.google.com/search",
            "bing" => "https://www.bing.com/search",
            "brave" => "https://search.brave.com/search",
            "ecosia" => "https://www.ecosia.org/search",
            _ => "https://duckduckgo.com/",
        };
        Url::parse_with_params(base, &[("q", query)])
            .map(|u| u.to_string())
            .unwrap_or_else(|_| base.to_string())
    }

    // --- History --------------------------------------------------------------

    /// Records a visit. Returns true if the history changed.
    pub fn add_history(&self, url: &str, title: &str) -> bool {
        if !self.settings.lock().unwrap().save_history {
            return false;
        }
        let Ok(u) = Url::parse(url) else { return false };
        if !matches!(u.scheme(), "http" | "https") || u.host_str() == Some("tauri.localhost") {
            return false;
        }
        let (t, title) = (now(), title.trim());
        let mut h = self.history.lock().unwrap();
        let dup = matches!(h.last(), Some(l) if l.url == url && t.saturating_sub(l.time) < DEDUPE_MS);
        if dup {
            if !title.is_empty() {
                if let Some(l) = h.last_mut() {
                    l.title = title.to_string();
                }
            }
        } else {
            h.push(Entry { url: url.to_string(), title: title.to_string(), time: t });
            let extra = h.len().saturating_sub(MAX_HISTORY);
            h.drain(..extra);
        }
        self.save("history.json", &*h);
        true
    }

    /// Newest first, optionally filtered by title or URL.
    pub fn history(&self, query: &str) -> Vec<Entry> {
        let q = query.trim().to_lowercase();
        self.history
            .lock()
            .unwrap()
            .iter()
            .rev()
            .filter(|e| q.is_empty() || e.title.to_lowercase().contains(&q) || e.url.to_lowercase().contains(&q))
            .take(1000)
            .cloned()
            .collect()
    }

    /// History matches for the address bar: newest first, one row per page or per search.
    pub fn suggest(&self, query: &str, limit: usize) -> Vec<Suggestion> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return vec![];
        }
        let mut seen = std::collections::HashSet::new();
        let mut out = vec![];
        for e in self.history.lock().unwrap().iter().rev() {
            let sq = search_query(&e.url);
            let (key, hay) = match &sq {
                Some(t) => (format!("q:{}", t.to_lowercase()), t.to_lowercase()),
                None => (e.url.clone(), format!("{} {}", e.title, e.url).to_lowercase()),
            };
            if !hay.contains(&q) || !seen.insert(key) {
                continue;
            }
            out.push(Suggestion { url: e.url.clone(), title: e.title.clone(), query: sq });
            if out.len() >= limit {
                break;
            }
        }
        out
    }

    pub fn remove(&self, time: u64, url: &str) {
        let mut h = self.history.lock().unwrap();
        h.retain(|e| !(e.time == time && e.url == url));
        self.save("history.json", &*h);
    }

    /// range: "hour" | "day" | "week" | anything else = everything.
    pub fn clear(&self, range: &str) {
        let span: u64 = match range {
            "hour" => 3_600_000,
            "day" => 86_400_000,
            "week" => 604_800_000,
            _ => 0,
        };
        let mut h = self.history.lock().unwrap();
        if span == 0 {
            h.clear();
        } else {
            let cutoff = now().saturating_sub(span);
            h.retain(|e| e.time < cutoff);
        }
        self.save("history.json", &*h);
    }
}

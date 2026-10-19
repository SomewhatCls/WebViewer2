//! Chrome Web Store installs: download CRX -> strip header -> unpack into
//! `<app data>/extensions/<id>/`, then load it into the running profile (see live.rs).
//! wry also re-adds every sub-folder of that directory at each startup.

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
};
use tauri::{AppHandle, Manager};

use crate::{Ext, Loaded, live};

#[derive(Serialize)]
pub struct ExtInfo {
    id: String,
    name: String,
    version: String,
    /// true if it is loaded in the running WebView2 profile; false means a restart is needed.
    active: bool,
    /// true if the manifest declares an options/settings page.
    options: bool,
}

fn is_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

/// Accepts a 32-letter extension ID or a Chrome Web Store URL.
fn parse_id(input: &str) -> Option<String> {
    let s = input.trim();
    if is_id(s) {
        return Some(s.into());
    }
    let u = tauri::Url::parse(s).ok()?;
    if !matches!(
        u.host_str(),
        Some("chromewebstore.google.com" | "chrome.google.com")
    ) {
        return None;
    }
    u.path_segments()?.find(|p| is_id(p)).map(String::from)
}

fn root(app: &AppHandle) -> Result<PathBuf, String> {
    app.state::<Ext>()
        .0
        .clone()
        .ok_or_else(|| "Extensions folder unavailable".to_string())
}

fn read_json(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

/// Called at startup. WebView2 setup fails if the folder holds anything that
/// isn't a valid extension, so remove stray files, temp dirs and broken installs.
pub fn prepare(root: &Path) -> Vec<String> {
    let mut ids = vec![];
    for entry in fs::read_dir(root).into_iter().flatten().flatten() {
        let p = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if p.is_dir() && !name.starts_with('.') && read_json(&p.join("manifest.json")).is_some() {
            ids.push(name);
        } else if p.is_dir() {
            let _ = fs::remove_dir_all(&p);
        } else {
            let _ = fs::remove_file(&p);
        }
    }
    ids
}

// --- Download ---------------------------------------------------------------

async fn download(id: &str) -> Result<Vec<u8>, String> {
    // Match the engine that actually runs the extension: the WebView2 version.
    let ver = tauri::webview_version().unwrap_or_else(|_| "130.0.0.0".into());
    let url = format!(
        "https://clients2.google.com/service/update2/crx?response=redirect&prodversion={ver}&acceptformat=crx2,crx3&x=id%3D{id}%26uc"
    );
    let res = reqwest::get(&url)
        .await
        .map_err(|e| format!("Download failed: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("The Web Store answered {}", res.status()));
    }
    let bytes = res
        .bytes()
        .await
        .map_err(|e| format!("Download failed: {e}"))?;
    if bytes.len() < 16 {
        return Err("The Web Store has no package for this ID. It may be delisted, unavailable in your region, or need a newer browser engine.".into());
    }
    Ok(bytes.to_vec())
}

// --- CRX parsing ------------------------------------------------------------

fn u32le(b: &[u8], o: usize) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?) as usize)
}

fn varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let (mut v, mut shift) = (0u64, 0);
    loop {
        let x = *b.get(*i)?;
        *i += 1;
        v |= ((x & 0x7f) as u64) << shift;
        if x & 0x80 == 0 {
            return Some(v);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

/// Minimal protobuf walker: returns (field number, bytes) for length-delimited fields.
fn fields(b: &[u8]) -> Vec<(u64, &[u8])> {
    let (mut i, mut out) = (0, vec![]);
    while i < b.len() {
        let Some(tag) = varint(b, &mut i) else { break };
        match tag & 7 {
            0 => {
                if varint(b, &mut i).is_none() {
                    break;
                }
            }
            1 => i += 8,
            5 => i += 4,
            2 => {
                let Some(len) = varint(b, &mut i) else { break };
                let end = i.saturating_add(len as usize);
                if end > b.len() {
                    break;
                }
                out.push((tag >> 3, &b[i..end]));
                i = end;
            }
            _ => break,
        }
    }
    out
}

/// Chrome extension ID: first 16 bytes of SHA-256(public key), nibbles mapped to a-p.
fn crx_id(key: &[u8]) -> String {
    Sha256::digest(key)[..16]
        .iter()
        .flat_map(|b| [b >> 4, b & 15])
        .map(|n| (b'a' + n) as char)
        .collect()
}

/// Returns the ZIP payload and the DER public key whose hash equals the extension ID.
fn parse_crx<'a>(b: &'a [u8], id: &str) -> Result<(&'a [u8], Option<Vec<u8>>), String> {
    let bad = || "The downloaded file is not a valid CRX package".to_string();
    if b.get(..4) != Some(&b"Cr24"[..]) {
        return Err(bad());
    }
    match u32le(b, 4).ok_or_else(bad)? {
        2 => {
            let (pk, sig) = (u32le(b, 8).ok_or_else(bad)?, u32le(b, 12).ok_or_else(bad)?);
            let key = b.get(16..16 + pk).map(|k| k.to_vec());
            Ok((b.get(16 + pk + sig..).ok_or_else(bad)?, key))
        }
        3 => {
            let hl = u32le(b, 8).ok_or_else(bad)?;
            let header = b.get(12..12 + hl).ok_or_else(bad)?;
            // CrxFileHeader: field 2 = RSA proofs, field 3 = ECDSA proofs; each holds public_key (field 1).
            let key = fields(header)
                .into_iter()
                .filter(|(f, _)| *f == 2 || *f == 3)
                .flat_map(|(_, m)| fields(m))
                .filter(|(g, _)| *g == 1)
                .map(|(_, k)| k)
                .find(|k| crx_id(k) == id);
            Ok((b.get(12 + hl..).ok_or_else(bad)?, key.map(|k| k.to_vec())))
        }
        _ => Err(bad()),
    }
}

// --- Install / list / remove --------------------------------------------------

fn unpack(zip: &[u8], key: Option<Vec<u8>>, dir: &Path) -> Result<(), String> {
    zip::ZipArchive::new(Cursor::new(zip))
        .map_err(|e| e.to_string())?
        .extract(dir) // rejects paths that escape `dir`
        .map_err(|e| e.to_string())?;
    let mf = dir.join("manifest.json");
    let mut json = read_json(&mf).ok_or("The extension has no readable manifest.json")?;
    let obj = json
        .as_object_mut()
        .ok_or("manifest.json is not an object")?;
    if !obj.contains_key("manifest_version") {
        return Err("manifest.json has no manifest_version".into());
    }
    // Pin the Web Store ID: unpacked extensions otherwise get an ID derived from their path.
    if let Some(k) = key {
        obj.insert("key".into(), STANDARD.encode(k).into());
    }
    fs::write(
        &mf,
        serde_json::to_vec_pretty(&json).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

pub async fn install(app: &AppHandle, input: &str) -> Result<ExtInfo, String> {
    let id = parse_id(input).ok_or("Enter a Chrome Web Store link or a 32-letter extension ID.")?;
    let crx = download(&id).await?;
    let (zip, key) = parse_crx(&crx, &id)?;

    let root = root(app)?;
    let (tmp, dest) = (root.join(format!(".{id}.tmp")), root.join(&id));
    let _ = fs::remove_dir_all(&tmp);
    if let Err(e) = unpack(zip, key, &tmp) {
        let _ = fs::remove_dir_all(&tmp);
        return Err(e);
    }
    let _ = fs::remove_dir_all(&dest);
    fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
    // Load it into the running profile. If that fails it still loads at next start.
    if live::add(app, &dest).await.is_ok() {
        let state = app.state::<Loaded>();
        let mut loaded = state.0.lock().unwrap();
        if !loaded.contains(&id) {
            loaded.push(id.clone());
        }
    }
    info(app, &dest).ok_or_else(|| "Installed, but the manifest could not be read".to_string())
}

fn info(app: &AppHandle, dir: &Path) -> Option<ExtInfo> {
    let id = dir.file_name()?.to_str()?.to_string();
    let m = read_json(&dir.join("manifest.json"))?;
    let mut name = m["name"].as_str().unwrap_or(&id).to_string();
    // Resolve "__MSG_key__" names from the default locale.
    let msg_key = name
        .strip_prefix("__MSG_")
        .and_then(|n| n.strip_suffix("__"))
        .map(str::to_string);
    if let Some(k) = msg_key {
        let loc = m["default_locale"].as_str().unwrap_or("en");
        let msgs = read_json(&dir.join("_locales").join(loc).join("messages.json"));
        let found = msgs
            .as_ref()
            .and_then(|v| v.as_object())
            .and_then(|o| o.iter().find(|(key, _)| key.eq_ignore_ascii_case(&k)))
            .and_then(|(_, e)| e["message"].as_str());
        if let Some(text) = found {
            name = text.to_string();
        }
    }
    let active = {
        let state = app.state::<Loaded>();
        let loaded = state.0.lock().unwrap();
        loaded.contains(&id)
    };
    Some(ExtInfo {
        version: m["version"].as_str().unwrap_or("").to_string(),
        active,
        options: options_page(&m).is_some(),
        id,
        name,
    })
}

/// Relative path of the extension's settings page (MV3 `options_ui`, or MV2 `options_page`).
fn options_page(m: &Value) -> Option<String> {
    let p = m["options_ui"]["page"]
        .as_str()
        .or_else(|| m["options_page"].as_str())?;
    let p = p.trim_start_matches('/');
    (!p.is_empty()).then(|| p.to_string())
}

/// `chrome-extension://<id>/<options page>` for an extension that WebView2 has loaded.
/// The ID comes from WebView2 itself, so it is right even if it differs from the folder name.
pub async fn options_url(app: &AppHandle, id: &str) -> Result<String, String> {
    if !is_id(id) {
        return Err("Invalid extension ID".into());
    }
    let dir = root(app)?.join(id);
    let m = read_json(&dir.join("manifest.json")).ok_or("Extension not found")?;
    let page = options_page(&m).ok_or("This extension has no settings page")?;
    let raw = m["name"].as_str().unwrap_or("").to_string();
    let resolved = info(app, &dir).map(|i| i.name).unwrap_or_default();

    let loaded = live::list(app)
        .await
        .map_err(|e| format!("Couldn’t read the loaded extensions: {e}"))?;
    let real = loaded
        .iter()
        .find(|(lid, _)| lid == id)
        .or_else(|| loaded.iter().find(|(_, n)| *n == raw || *n == resolved))
        .map(|(lid, _)| lid.clone())
        .ok_or("WebView2 hasn’t loaded this extension. Restart the browser and try again.")?;
    Ok(format!("chrome-extension://{real}/{page}"))
}

pub fn list(app: &AppHandle) -> Vec<ExtInfo> {
    let Ok(root) = root(app) else { return vec![] };
    let mut out: Vec<ExtInfo> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && !p
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        })
        .filter_map(|p| info(app, &p))
        .collect();
    out.sort_by_key(|e| e.name.to_lowercase());
    out
}

pub async fn remove(app: &AppHandle, id: &str) -> Result<(), String> {
    if !is_id(id) {
        return Err("Invalid extension ID".into());
    }
    // WebView2 keeps added extensions in the profile, so unload it there first.
    live::remove(app, id)
        .await
        .map_err(|e| format!("Couldn’t unload the extension: {e}"))?;
    let dir = root(app)?.join(id);
    if fs::remove_dir_all(&dir).is_err() {
        // Files may still be locked; park the folder where startup cleanup deletes it.
        fs::rename(&dir, dir.with_file_name(format!(".{id}.del"))).map_err(|e| e.to_string())?;
    }
    let state = app.state::<Loaded>();
    state.0.lock().unwrap().retain(|x| x != id);
    Ok(())
}

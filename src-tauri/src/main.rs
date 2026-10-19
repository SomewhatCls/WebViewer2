#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(windows))]
compile_error!("WebViewer2 requires Windows WebView2.");

mod live;
mod store;

use serde::Serialize;
use std::path::PathBuf;
use tauri::{
    webview::{NewWindowResponse, PageLoadEvent, WebviewBuilder},
    window::WindowBuilder,
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Position, Size, Url, Window,
    WindowEvent, WebviewUrl,
};

/// Height (logical px) of the UI webview: 40 tab strip + 44 toolbar.
const CHROME_H: f64 = 84.0;

struct Ext(Option<PathBuf>);

/// IDs of the extensions loaded into the running WebView2 profile.
struct Loaded(std::sync::Mutex<Vec<String>>);

#[derive(Clone, Serialize)]
struct TabUpdate {
    id: String,
    url: Option<String>,
    title: Option<String>,
    loading: Option<bool>,
}

fn label(id: &str) -> String {
    format!("tab-{id}")
}

/// Every webview shares one WebView2 environment, so options must match.
fn configure(b: WebviewBuilder<tauri::Wry>, ext: &Option<PathBuf>) -> WebviewBuilder<tauri::Wry> {
    let b = b.browser_extensions_enabled(true);
    match ext {
        Some(p) => b.extensions_path(p),
        None => b,
    }
}

fn normalize(input: &str) -> String {
    let s = input.trim();
    if let Ok(u) = Url::parse(s) {
        if matches!(u.scheme(), "http" | "https") {
            return s.to_string();
        }
    }
    if !s.contains(char::is_whitespace) {
        if s.starts_with("localhost") {
            return format!("http://{s}");
        }
        if s.contains('.') {
            return format!("https://{s}");
        }
    }
    Url::parse_with_params("https://duckduckgo.com/", &[("q", s)])
        .map(|u| u.to_string())
        .unwrap_or_else(|_| "https://duckduckgo.com/".into())
}

fn layout(window: &Window) {
    let (Ok(scale), Ok(px)) = (window.scale_factor(), window.inner_size()) else {
        return;
    };
    let size = px.to_logical::<f64>(scale);
    let content_h = (size.height - CHROME_H).max(0.0);
    for (name, wv) in window.app_handle().webviews() {
        if name == "ui" {
            let _ = wv.set_size(Size::Logical(LogicalSize::new(size.width, CHROME_H)));
        } else if name.starts_with("tab-") {
            let _ = wv.set_position(Position::Logical(LogicalPosition::new(0.0, CHROME_H)));
            let _ = wv.set_size(Size::Logical(LogicalSize::new(size.width, content_h)));
        }
    }
}

fn show_only(app: &AppHandle, id: &str) {
    let keep = label(id);
    for (name, wv) in app.webviews() {
        if name.starts_with("tab-") {
            if name == keep {
                let _ = wv.show();
                let _ = wv.set_focus();
            } else {
                let _ = wv.hide();
            }
        }
    }
}

fn emit_tab(app: &AppHandle, update: TabUpdate) {
    let _ = app.emit("tab-updated", update);
}

// Commands are async: creating webviews from sync commands deadlocks on Windows.
#[tauri::command]
async fn new_tab(app: AppHandle, id: String, url: String) -> Result<(), String> {
    let window = app.get_window("main").ok_or("main window missing")?;
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    let size = window.inner_size().map_err(|e| e.to_string())?.to_logical::<f64>(scale);
    let target = if url == "about:extensions" {
        WebviewUrl::App("extensions.html".into())
    } else {
        WebviewUrl::External(normalize(&url).parse::<Url>().map_err(|e| e.to_string())?)
    };
    let ext = app.state::<Ext>().0.clone();

    let (a1, i1) = (app.clone(), id.clone());
    let (a2, i2) = (app.clone(), id.clone());
    let (a3, i3) = (app.clone(), id.clone());
    let a4 = app.clone();

    let builder = WebviewBuilder::new(label(&id), target)
        .on_navigation(move |u| {
            emit_tab(&a1, TabUpdate { id: i1.clone(), url: Some(u.to_string()), title: None, loading: None });
            true
        })
        .on_page_load(move |_, p| {
            emit_tab(&a2, TabUpdate {
                id: i2.clone(),
                url: Some(p.url().to_string()),
                title: None,
                loading: Some(matches!(p.event(), PageLoadEvent::Started)),
            });
        })
        .on_document_title_changed(move |_, title| {
            emit_tab(&a3, TabUpdate { id: i3.clone(), url: None, title: Some(title), loading: None });
        })
        // target=_blank and window.open become new tabs.
        .on_new_window(move |u, _| {
            let _ = a4.emit("open-tab", u.to_string());
            NewWindowResponse::Deny
        });

    window
        .add_child(
            configure(builder, &ext),
            LogicalPosition::new(0.0, CHROME_H),
            LogicalSize::new(size.width, (size.height - CHROME_H).max(0.0)),
        )
        .map_err(|e| e.to_string())?;
    show_only(&app, &id);
    Ok(())
}

#[tauri::command]
async fn activate_tab(app: AppHandle, id: String) {
    show_only(&app, &id);
}

#[tauri::command]
async fn close_tab(app: AppHandle, id: String) {
    if let Some(wv) = app.get_webview(&label(&id)) {
        let _ = wv.close();
    }
}

#[tauri::command]
async fn navigate(app: AppHandle, id: String, input: String) -> Result<(), String> {
    let wv = app.get_webview(&label(&id)).ok_or("no such tab")?;
    let url = normalize(&input).parse::<Url>().map_err(|e| e.to_string())?;
    wv.navigate(url).map_err(|e| e.to_string())
}

#[tauri::command]
async fn nav(app: AppHandle, id: String, action: String) -> Result<(), String> {
    let wv = app.get_webview(&label(&id)).ok_or("no such tab")?;
    let js = match action.as_str() {
        "back" => "history.back()",
        "forward" => "history.forward()",
        "reload" => "location.reload()",
        _ => return Err("unknown action".into()),
    };
    wv.eval(js).map_err(|e| e.to_string())
}

#[tauri::command]
async fn win_minimize(window: Window) {
    let _ = window.minimize();
}

#[tauri::command]
async fn win_toggle_maximize(window: Window) {
    if window.is_maximized().unwrap_or(false) {
        let _ = window.unmaximize();
    } else {
        let _ = window.maximize();
    }
}

#[tauri::command]
async fn win_is_maximized(window: Window) -> bool {
    window.is_maximized().unwrap_or(false)
}

#[tauri::command]
async fn win_drag(window: Window) {
    let _ = window.start_dragging();
}

#[tauri::command]
async fn win_close(window: Window) {
    let _ = window.close();
}

#[tauri::command]
async fn install_extension(app: AppHandle, input: String) -> Result<store::ExtInfo, String> {
    store::install(&app, &input).await
}

#[tauri::command]
async fn list_extensions(app: AppHandle) -> Vec<store::ExtInfo> {
    store::list(&app)
}

#[tauri::command]
async fn remove_extension(app: AppHandle, id: String) -> Result<(), String> {
    store::remove(&app, &id).await
}

#[tauri::command]
fn restart_app(app: AppHandle) {
    app.restart();
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            new_tab, activate_tab, close_tab, navigate, nav,
            install_extension, list_extensions, remove_extension, restart_app,
            win_minimize, win_toggle_maximize, win_is_maximized, win_drag, win_close
        ])
        .setup(|app| {
            // Unpacked Chrome extensions go in <app data dir>/extensions.
            let ext = app.path().app_data_dir().ok().map(|d| d.join("extensions"));
            if let Some(dir) = &ext {
                let _ = std::fs::create_dir_all(dir);
            }
            let loaded = ext.as_deref().map(store::prepare).unwrap_or_default();
            app.manage(Ext(ext.clone()));
            app.manage(Loaded(std::sync::Mutex::new(loaded)));

            let window = WindowBuilder::new(app, "main")
                .title("WebViewer2")
                .inner_size(1200.0, 800.0)
                .min_inner_size(640.0, 400.0)
                // Custom frame: the tab strip is the title bar. With shadow(true), Windows keeps
                // native resize handles outside the client area and rounds corners on Windows 11.
                .decorations(false)
                .shadow(true)
                .build()?;

            let size = window.inner_size()?.to_logical::<f64>(window.scale_factor()?);
            window.add_child(
                configure(WebviewBuilder::new("ui", WebviewUrl::App("index.html".into())), &ext),
                LogicalPosition::new(0.0, 0.0),
                LogicalSize::new(size.width, CHROME_H),
            )?;

            let w = window.clone();
            window.on_window_event(move |e| {
                if let WindowEvent::Resized(_) = e {
                    layout(&w);
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running WebViewer2");
}

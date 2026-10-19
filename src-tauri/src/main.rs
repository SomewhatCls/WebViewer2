#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(windows))]
compile_error!("WebViewer2 requires Windows WebView2.");

mod data;
mod live;
mod store;

use serde::Serialize;
use std::path::PathBuf;
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Position, Size, Theme, Url,
    WebviewUrl, Window, WindowEvent,
    webview::{NewWindowResponse, PageLoadEvent, WebviewBuilder},
    window::{Effect, EffectsBuilder, WindowBuilder},
};

/// Height (logical px) of the UI webview: 40 tab strip + 44 toolbar.
const CHROME_H: f64 = 84.0;

struct Ext(Option<PathBuf>);

/// IDs of the extensions loaded into the running WebView2 profile.
struct Loaded(std::sync::Mutex<Vec<String>>);

/// Whether the Mica backdrop could be applied (Windows 11 only).
struct Mica(bool);

/// Extra height (logical px) the UI webview grows by while the address-bar suggestions are open.
/// Page webviews slide down by the same amount so the list is not covered by them.
struct Suggest(std::sync::Mutex<f64>);

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
fn configure(
    b: WebviewBuilder<tauri::Wry>,
    ext: Option<&PathBuf>,
) -> WebviewBuilder<tauri::Wry> {
    let b = b.browser_extensions_enabled(true);

    match ext {
        Some(path) => b.extensions_path(path),
        None => b,
    }
}

fn normalize(input: &str, data: &data::Data) -> String {
    let s = input.trim();
    if let Ok(u) = Url::parse(s) {
        // chrome-extension:// lets extension pages (e.g. their settings) open in a tab.
        if matches!(u.scheme(), "http" | "https" | "chrome-extension") {
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
    data.search_url(s)
}

fn apply_theme(window: &Window, theme: &str) {
    let t = match theme {
        "light" => Some(Theme::Light),
        "dark" => Some(Theme::Dark),
        _ => None, // follow the system
    };
    let _ = window.set_theme(t);
}

fn layout(window: &Window) {
    let (Ok(scale), Ok(px)) = (window.scale_factor(), window.inner_size()) else {
        return;
    };
    let size = px.to_logical::<f64>(scale);
    let content_h = (size.height - CHROME_H).max(0.0);
    let extra = *window.app_handle().state::<Suggest>().0.lock().unwrap();
    for (name, wv) in window.app_handle().webviews() {
        if name == "ui" {
            let _ = wv.set_size(Size::Logical(LogicalSize::new(
                size.width,
                CHROME_H + extra,
            )));
        } else if name.starts_with("tab-") {
            // Only moved, not resized: the page keeps its layout while the suggestion list is open.
            let _ = wv.set_position(Position::Logical(LogicalPosition::new(
                0.0,
                CHROME_H + extra,
            )));
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
    let size = window
        .inner_size()
        .map_err(|e| e.to_string())?
        .to_logical::<f64>(scale);
    let target = match url.as_str() {
        "about:extensions" => WebviewUrl::App("extensions.html".into()),
        "about:settings" => WebviewUrl::App("settings.html".into()),
        "about:new" => WebviewUrl::App("new.html".into()),
        _ => {
            let data = app.state::<data::Data>();
            WebviewUrl::External(
                normalize(&url, &data)
                    .parse::<Url>()
                    .map_err(|e| e.to_string())?,
            )
        }
    };

    let (a1, i1) = (app.clone(), id.clone());
    let (a2, i2) = (app.clone(), id.clone());
    let (a3, i3) = (app.clone(), id.clone());
    let a4 = app.clone();

    let builder = WebviewBuilder::new(label(&id), target)
        .on_navigation(move |u| {
            // Some extension pages get bounced to Chrome's built-in new tab page
            // (chrome-search://local-ntp/...), which doesn't exist in WebView2. Cancel that.
            if u.scheme() == "chrome-search" {
                return false;
            }
            emit_tab(
                &a1,
                TabUpdate {
                    id: i1.clone(),
                    url: Some(u.to_string()),
                    title: None,
                    loading: None,
                },
            );
            true
        })
        .on_page_load(move |_, p| {
            emit_tab(
                &a2,
                TabUpdate {
                    id: i2.clone(),
                    url: Some(p.url().to_string()),
                    title: None,
                    loading: Some(matches!(p.event(), PageLoadEvent::Started)),
                },
            );
        })
        .on_document_title_changed(move |_, title| {
            emit_tab(
                &a3,
                TabUpdate {
                    id: i3.clone(),
                    url: None,
                    title: Some(title),
                    loading: None,
                },
            );
        })
        // target=_blank and window.open become new tabs.
        .on_new_window(move |u, _| {
            let _ = a4.emit("open-tab", u.to_string());
            NewWindowResponse::Deny
        });

    window
        .add_child(
            configure(builder, None),
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
fn close_tab(app: AppHandle, id: String) -> Result<(), String> {
    if let Some(wv) = app.get_webview(&label(&id)) {
        wv.close()
            .map_err(|e| format!("Failed to close tab {id}: {e}"))?;
    }

    Ok(())
}

#[tauri::command]
fn exit_app(app: tauri::AppHandle) {
    app.exit(0);
}

#[tauri::command]
async fn navigate(app: AppHandle, id: String, input: String) -> Result<(), String> {
    let wv = app.get_webview(&label(&id)).ok_or("no such tab")?;
    let url = {
        let data = app.state::<data::Data>();
        normalize(&input, &data)
            .parse::<Url>()
            .map_err(|e| e.to_string())?
    };
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
fn has_mica(app: AppHandle) -> bool {
    app.state::<Mica>().0
}

// --- Settings -------------------------------------------------------------------

#[tauri::command]
fn get_settings(app: AppHandle) -> data::Settings {
    app.state::<data::Data>().settings()
}

#[tauri::command]
async fn set_settings(app: AppHandle, settings: data::Settings) -> Result<(), String> {
    let saved = app.state::<data::Data>().set_settings(settings);
    if let Some(w) = app.get_window("main") {
        apply_theme(&w, &saved.theme);
    }
    app.emit("settings-changed", saved)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn about_webview() -> String {
    tauri::webview_version().unwrap_or_else(|_| "unknown".into())
}

/// Opens an internal page or a web address in the browser's tab strip.
#[tauri::command]
fn open_page(app: AppHandle, url: String) {
    if url.starts_with("about:") || url.starts_with("http://") || url.starts_with("https://") {
        let _ = app.emit("open-tab", url);
    }
}

// --- History --------------------------------------------------------------------

#[tauri::command]
fn history_add(app: AppHandle, url: String, title: String) {
    if app.state::<data::Data>().add_history(&url, &title) {
        let _ = app.emit("history-updated", ());
    }
}

#[tauri::command]
fn history_suggest(app: AppHandle, query: String) -> Vec<data::Suggestion> {
    app.state::<data::Data>().suggest(&query, 6)
}

/// Grows (or shrinks back) the UI webview for the address-bar suggestion list.
#[tauri::command]
async fn set_suggest_height(app: AppHandle, height: f64) {
    *app.state::<Suggest>().0.lock().unwrap() = height.clamp(0.0, 400.0);
    if let Some(w) = app.get_window("main") {
        layout(&w);
    }
}

#[tauri::command]
fn history_list(app: AppHandle, query: String) -> Vec<data::Entry> {
    app.state::<data::Data>().history(&query)
}

#[tauri::command]
fn history_remove(app: AppHandle, time: u64, url: String) {
    app.state::<data::Data>().remove(time, &url);
    let _ = app.emit("history-updated", ());
}

#[tauri::command]
fn history_clear(app: AppHandle, range: String) {
    app.state::<data::Data>().clear(&range);
    let _ = app.emit("history-updated", ());
}

// --- Extensions -----------------------------------------------------------------

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

/// Opens the extension's options page (chrome-extension://<id>/<page>) in a new tab.
#[tauri::command]
async fn open_extension_options(app: AppHandle, id: String) -> Result<(), String> {
    let url = store::options_url(&app, &id).await?;
    app.emit("open-tab", url).map_err(|e| e.to_string())
}

#[tauri::command]
fn restart_app(app: AppHandle) {
    app.restart();
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_process::init())
        .invoke_handler(tauri::generate_handler![
            new_tab,
            activate_tab,
            close_tab,
            exit_app,
            navigate,
            nav,
            install_extension,
            list_extensions,
            remove_extension,
            open_extension_options,
            restart_app,
            get_settings,
            set_settings,
            about_webview,
            open_page,
            history_add,
            history_list,
            history_suggest,
            history_remove,
            history_clear,
            set_suggest_height,
            has_mica,
            win_minimize,
            win_toggle_maximize,
            win_is_maximized,
            win_drag,
            win_close
        ])
        .setup(|app| {
            // Unpacked Chrome extensions go in <app data dir>/extensions.
            let base = app.path().app_data_dir().ok();
            let ext = base.as_ref().map(|d| d.join("extensions"));
            if let Some(dir) = &ext {
                let _ = std::fs::create_dir_all(dir);
            }
            let loaded = ext.as_deref().map(store::prepare).unwrap_or_default();
            let data = data::Data::load(base);
            let theme = data.settings().theme;
            app.manage(Ext(ext.clone()));
            app.manage(Loaded(std::sync::Mutex::new(loaded)));
            app.manage(Suggest(std::sync::Mutex::new(0.0)));
            app.manage(data);

            let window = WindowBuilder::new(app, "main")
                .title("WebViewer2")
                .inner_size(1200.0, 800.0)
                .min_inner_size(640.0, 400.0)
                // Custom frame: the tab strip is the title bar. With shadow(true), Windows keeps
                // native resize handles outside the client area and rounds corners on Windows 11.
                .decorations(false)
                // Transparent so the Mica backdrop can show through the (transparent) UI webview.
                .transparent(true)
                .shadow(true)
                .build()?;

            // Mica needs Windows 11; on Windows 10 this fails and the UI keeps a solid strip.
            let mica = window
                .set_effects(EffectsBuilder::new().effect(Effect::Mica).build())
                .is_ok();
            app.manage(Mica(mica));
            apply_theme(&window, &theme);

            let size = window
                .inner_size()?
                .to_logical::<f64>(window.scale_factor()?);
            window.add_child(
                configure(
                    WebviewBuilder::new("ui", WebviewUrl::App("index.html".into()))
                        .transparent(true),
                    ext.as_ref(),
                ),
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

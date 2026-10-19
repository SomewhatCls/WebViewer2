//! Add/remove extensions on the *running* WebView2 profile, so no restart is needed.
//! Uses ICoreWebView2Profile7 (AddBrowserExtension / GetBrowserExtensions) through
//! `Webview::with_webview`, which runs the closure on the main (UI) thread.

use std::{cell::RefCell, path::Path, rc::Rc, time::Duration};

use tauri::{webview::PlatformWebview, AppHandle, Manager};
use tokio::sync::oneshot;
use webview2_com::{
    take_pwstr,
    Microsoft::Web::WebView2::Win32::{ICoreWebView2Profile7, ICoreWebView2_13},
    BrowserExtensionRemoveCompletedHandler, ProfileAddBrowserExtensionCompletedHandler,
    ProfileGetBrowserExtensionsCompletedHandler,
};
use windows::core::{Interface, HSTRING, PWSTR};

/// One-shot reply slot, shared between the call site and the COM completion handler.
type Reply = Rc<RefCell<Option<oneshot::Sender<Result<(), String>>>>>;

fn finish(reply: &Reply, value: Result<(), String>) {
    if let Some(tx) = reply.borrow_mut().take() {
        let _ = tx.send(value);
    }
}

fn profile(pw: &PlatformWebview) -> windows::core::Result<ICoreWebView2Profile7> {
    let core = unsafe { pw.controller().CoreWebView2()? };
    let core13: ICoreWebView2_13 = core.cast()?;
    unsafe { core13.Profile()? }.cast()
}

/// Runs `f` on the UI thread with the profile, then waits for `f` to call `finish`.
async fn on_profile<F>(app: &AppHandle, f: F) -> Result<(), String>
where
    F: FnOnce(ICoreWebView2Profile7, Reply) + Send + 'static,
{
    let ui = app.get_webview("ui").ok_or("UI webview missing")?;
    let (tx, rx) = oneshot::channel();
    ui.with_webview(move |pw| {
        let reply: Reply = Rc::new(RefCell::new(Some(tx)));
        match profile(&pw) {
            Ok(p) => f(p, reply),
            Err(e) => finish(&reply, Err(e.to_string())),
        }
    })
    .map_err(|e| e.to_string())?;
    match tokio::time::timeout(Duration::from_secs(15), rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("WebView2 dropped the request".into()),
        Err(_) => Err("WebView2 did not answer in time".into()),
    }
}

/// Loads the unpacked extension in `dir` into the running profile.
pub async fn add(app: &AppHandle, dir: &Path) -> Result<(), String> {
    let path = HSTRING::from(dir.to_string_lossy().as_ref());
    on_profile(app, move |profile, reply| {
        let done = reply.clone();
        let handler = ProfileAddBrowserExtensionCompletedHandler::create(Box::new(move |res, _ext| {
            finish(&done, res.map_err(|e| e.to_string()));
            Ok(())
        }));
        if let Err(e) = unsafe { profile.AddBrowserExtension(&path, &handler) } {
            finish(&reply, Err(e.to_string()));
        }
    })
    .await
}

/// Removes the extension with this ID from the running profile (no-op if not loaded).
pub async fn remove(app: &AppHandle, id: &str) -> Result<(), String> {
    let id = id.to_string();
    on_profile(app, move |profile, reply| {
        let r = reply.clone();
        let handler = ProfileGetBrowserExtensionsCompletedHandler::create(Box::new(move |res, list| {
            // Ok(true) = removal started (its own handler will reply); Ok(false) = not loaded.
            let outcome = (|| -> windows::core::Result<bool> {
                res?;
                // This webview2-com version hands over an Option, not a Result.
                let list = list.ok_or_else(|| windows::core::Error::from(windows::core::HRESULT(0x8000_4003_u32 as i32)))?; // E_POINTER
                let mut count = 0u32;
                unsafe { list.Count(&mut count)? };
                for i in 0..count {
                    let ext = unsafe { list.GetValueAtIndex(i)? };
                    let mut raw = PWSTR::null();
                    unsafe { ext.Id(&mut raw)? };
                    if take_pwstr(raw) == id {
                        let done = r.clone();
                        let rm = BrowserExtensionRemoveCompletedHandler::create(Box::new(move |res| {
                            finish(&done, res.map_err(|e| e.to_string()));
                            Ok(())
                        }));
                        unsafe { ext.Remove(&rm)? };
                        return Ok(true);
                    }
                }
                Ok(false)
            })();
            match outcome {
                Ok(true) => {}
                Ok(false) => finish(&r, Ok(())),
                Err(e) => finish(&r, Err(e.to_string())),
            }
            Ok(())
        }));
        if let Err(e) = unsafe { profile.GetBrowserExtensions(&handler) } {
            finish(&reply, Err(e.to_string()));
        }
    })
    .await
}

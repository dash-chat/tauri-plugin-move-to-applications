//! Moves the app into /Applications when macOS runs it from somewhere it
//! cannot update itself: a mounted disk image, or the read-only copy macOS
//! translocates a freshly downloaded app to.
//!
//! When the first window opens from such a place the user is asked whether to
//! move the app; the `move_to_applications_folder` command does the same move
//! on request.

use tauri::plugin::{Builder, TauriPlugin};
use tauri::{AppHandle, Runtime};

#[cfg(target_os = "macos")]
mod macos;

/// The text of the question asked on launch.
pub struct Prompt {
    pub title: String,
    pub message: String,
    pub move_button: String,
    pub cancel_button: String,
}

pub fn init<R: Runtime>(prompt: Prompt) -> TauriPlugin<R> {
    let mut asked = false;
    Builder::new("move-to-applications")
        .invoke_handler(tauri::generate_handler![
            needs_move,
            move_to_applications_folder
        ])
        .on_window_ready(move |window| {
            // The main window is created again when reopened from the tray.
            if std::mem::replace(&mut asked, true) {
                return;
            }
            #[cfg(target_os = "macos")]
            macos::offer_move(&window, &prompt);
            #[cfg(not(target_os = "macos"))]
            let _ = (window, &prompt);
        })
        .build()
}

/// Whether the app runs from somewhere it cannot replace itself.
#[tauri::command]
async fn needs_move<R: Runtime>(app: AppHandle<R>) -> bool {
    #[cfg(target_os = "macos")]
    return macos::needs_move(&app);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        false
    }
}

/// Moves the app into /Applications and relaunches it from there. Returns only
/// on failure, or with `Ok` when the user cancels the administrator prompt.
#[tauri::command]
async fn move_to_applications_folder<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    // The copy and the administrator prompt block.
    #[cfg(target_os = "macos")]
    return tauri::async_runtime::spawn_blocking(move || macos::move_to_applications_folder(&app))
        .await
        .map_err(|err| err.to_string())?;
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Err("only macOS apps get moved to Applications".into())
    }
}

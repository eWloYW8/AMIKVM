//! Native text clipboard access. No clipboard content is returned to JavaScript.
use amikvm_core::{Error, Result};
use tauri::AppHandle;

pub async fn read(app: AppHandle) -> Result<String> {
    #[cfg(target_os = "linux")]
    let read = {
        let (send, receive) = tokio::sync::oneshot::channel();
        app.run_on_main_thread(move || {
            let clipboard = gtk::Clipboard::get(&gtk::gdk::Atom::intern("CLIPBOARD"));
            clipboard.request_text(move |_, text| {
                let result = match text {
                    Some(text) if text.len() <= 65536 => Ok(text.to_owned()),
                    Some(_) => Err(Error::Invalid("文本长度不能超过 64 KiB".into())),
                    None => Err(Error::Invalid("本机剪贴板没有可读取的文本".into())),
                };
                let _ = send.send(result);
            });
        })
        .map_err(|_| Error::Invalid("无法读取本机剪贴板文本".into()))?;
        async {
            receive
                .await
                .map_err(|_| Error::Invalid("无法读取本机剪贴板文本".into()))?
        }
    };
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let read = async {
        use tauri_plugin_clipboard_manager::ClipboardExt;
        tauri::async_runtime::spawn_blocking(move || app.clipboard().read_text())
            .await
            .map_err(|_| Error::Invalid("无法读取本机剪贴板文本".into()))?
            .map_err(|_| Error::Invalid("本机剪贴板没有可读取的文本".into()))
    };
    let text = tokio::time::timeout(std::time::Duration::from_secs(5), read)
        .await
        .map_err(|_| Error::Timeout("无法读取本机剪贴板文本"))??;
    if text.len() > 65536 {
        return Err(Error::Invalid("文本长度不能超过 64 KiB".into()));
    }
    Ok(text)
}

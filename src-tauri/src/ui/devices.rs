//! Device discovery and refresh lifetime are owned by Rust, never by the webview.
use super::{AppState, Dialog};
use amikvm_core::media::device::Device;
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;
#[derive(Default, Clone)]
pub struct Inventory {
    pub generation: Uuid,
    pub entries: Vec<Device>,
    pub loading: bool,
    pub error: Option<String>,
}
pub fn choice(device: &Device) -> String {
    serde_json::to_string(&(&device.id, &device.identity)).expect("device identity serializes")
}
fn visible(ui: &super::UiState, generation: Uuid) -> bool {
    ui.devices.generation == generation
        && (matches!(ui.dialog, Dialog::PhysicalMedia(..))
            || matches!(ui.dialog, Dialog::Confirmation)
                && ui
                    .confirmation
                    .as_ref()
                    .is_some_and(|c| matches!(c.previous, Dialog::PhysicalMedia(..))))
}
pub fn discover(app: AppHandle, generation: Uuid) {
    tauri::async_runtime::spawn(async move {
        loop {
            let state = app.state::<AppState>();
            if !state.ui.lock().is_ok_and(|ui| visible(&ui, generation)) {
                break;
            }
            let found =
                tauri::async_runtime::spawn_blocking(amikvm_core::media::device::list).await;
            let (entries, error) = match found {
                Ok(Ok(entries)) => (entries, None),
                Ok(Err(error)) => (vec![], Some(error.to_string())),
                Err(error) => (vec![], Some(error.to_string())),
            };
            let changed = if let Ok(mut ui) = state.ui.lock() {
                if !visible(&ui, generation) {
                    break;
                }
                let changed = ui.devices.loading
                    || ui.devices.entries != entries
                    || ui.devices.error != error;
                ui.devices.entries = entries;
                ui.devices.error = error;
                ui.devices.loading = false;
                changed
            } else {
                break;
            };
            if changed {
                app.emit("ui-changed", ()).ok();
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });
}

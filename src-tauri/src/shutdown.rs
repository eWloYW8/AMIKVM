//! Keeps the native window and runtime alive through next-master selection and cleanup.
use crate::commands::{AppState, Response};
use amikvm_core::{
    protocol::Control,
    sharing::exit::{Phase, Plan, Scope, Session as ExitSession},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tauri::{AppHandle, Emitter, Manager as _};
use tokio::sync::Notify;
use uuid::Uuid;

#[derive(Default)]
pub struct Manager {
    current: Mutex<Option<Arc<Flow>>>,
}
struct Flow {
    plan: Mutex<Plan>,
    changed: Notify,
    quit_after: AtomicBool,
}
impl Manager {
    pub fn snapshot(&self) -> Option<Plan> {
        self.current
            .lock()
            .ok()?
            .as_ref()?
            .plan
            .lock()
            .ok()
            .map(|p| p.clone())
    }
    pub fn blocks_connection(&self, id: Uuid) -> bool {
        self.snapshot().is_some_and(|p| p.scope.contains(id))
    }
    fn flow(&self, id: Uuid) -> Response<Arc<Flow>> {
        self.current
            .lock()
            .map_err(|_| "关闭状态不可用")?
            .as_ref()
            .filter(|f| f.plan.lock().is_ok_and(|p| p.id == id))
            .cloned()
            .ok_or_else(|| "该关闭窗口已经结束。".into())
    }
    pub fn request(&self, app: &AppHandle, scope: Scope) -> Response<()> {
        let flow = {
            let mut current = self.current.lock().map_err(|_| "关闭状态不可用")?;
            if let Some(flow) = current.as_ref() {
                let mut plan = flow.plan.lock().map_err(|_| "关闭状态不可用")?;
                if plan.phase == Phase::Closing
                    && plan.scope != Scope::Application
                    && scope == Scope::Application
                {
                    flow.quit_after.store(true, Ordering::Release);
                } else {
                    plan.upgrade(scope).map_err(|e| e.to_string())?;
                }
                flow.changed.notify_one();
                return Ok(());
            }
            let flow = Arc::new(Flow {
                plan: Mutex::new(Plan::new(scope)),
                changed: Notify::new(),
                quit_after: AtomicBool::new(false),
            });
            *current = Some(flow.clone());
            flow
        };
        changed(app);
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            run(app, flow).await;
        });
        Ok(())
    }
    pub fn cancel(&self, app: &AppHandle, id: Uuid) -> Response<()> {
        let flow = self.flow(id)?;
        flow.plan
            .lock()
            .map_err(|_| "关闭状态不可用")?
            .cancel(id)
            .map_err(|e| e.to_string())?;
        self.remove(&flow);
        flow.changed.notify_one();
        changed(app);
        Ok(())
    }
    pub async fn choose(
        &self,
        app: &AppHandle,
        id: Uuid,
        server: Uuid,
        target: Option<Uuid>,
    ) -> Response<()> {
        let flow = self.flow(id)?;
        refresh_plan(app, &flow).await?;
        flow.plan
            .lock()
            .map_err(|_| "关闭状态不可用")?
            .choose(id, server, target, std::time::Instant::now())
            .map_err(|e| e.to_string())?;
        flow.changed.notify_one();
        changed(app);
        Ok(())
    }
    pub async fn finish(&self, app: &AppHandle, id: Uuid, transfer: bool) -> Response<()> {
        let flow = self.flow(id)?;
        refresh_plan(app, &flow).await?;
        let mut plan = flow.plan.lock().map_err(|_| "关闭状态不可用")?;
        // Refresh may have exhausted the timer or removed the final candidate.
        if plan.phase == Phase::Choosing {
            plan.finish(id, transfer, std::time::Instant::now())
                .map_err(|e| e.to_string())?;
        }
        drop(plan);
        flow.changed.notify_one();
        changed(app);
        Ok(())
    }
    fn remove(&self, flow: &Arc<Flow>) {
        if let Ok(mut current) = self.current.lock() {
            if current.as_ref().is_some_and(|f| Arc::ptr_eq(f, flow)) {
                *current = None;
            }
        }
    }
    fn is_current(&self, flow: &Arc<Flow>) -> bool {
        self.current
            .lock()
            .is_ok_and(|current| current.as_ref().is_some_and(|f| Arc::ptr_eq(f, flow)))
    }
}
fn changed(app: &AppHandle) {
    let _ = app.emit("ui-changed", serde_json::json!({}));
}
fn failure(app: &AppHandle, message: String) {
    if let Ok(mut ui) = app.state::<AppState>().ui.lock() {
        ui.error = Some(message);
    }
    changed(app);
}
pub fn request(app: &AppHandle, scope: Scope) {
    if let Err(message) = app.state::<AppState>().shutdown.request(app, scope) {
        failure(app, message);
    }
}
async fn sessions(app: &AppHandle) -> Response<Vec<ExitSession>> {
    let state = app.state::<AppState>();
    let names = state
        .store
        .lock()
        .map_err(|_| "服务器数据库不可用")?
        .list()
        .into_iter()
        .map(|s| (s.id, s.name))
        .collect::<std::collections::HashMap<_, _>>();
    let sessions = state
        .sessions
        .lock()
        .await
        .values()
        .cloned()
        .collect::<Vec<_>>();
    sessions
        .into_iter()
        .map(|session| {
            let s = session.snapshot.lock().map_err(|_| "会话状态不可用")?;
            Ok(ExitSession {
                id: s.server_id,
                name: names
                    .get(&s.server_id)
                    .cloned()
                    .unwrap_or_else(|| s.server_id.to_string()),
                eligible: s.phase == "connected" && s.video_connected && s.can_control,
                own_id: s.own_session_id,
                users: s.users.clone(),
            })
        })
        .collect()
}
async fn refresh_plan(app: &AppHandle, flow: &Flow) -> Response<()> {
    let current = sessions(app).await?;
    let mut plan = flow.plan.lock().map_err(|_| "关闭状态不可用")?;
    if plan.phase == Phase::Preparing {
        plan.begin(current, std::time::Instant::now());
    } else {
        plan.sync(current, std::time::Instant::now());
    }
    Ok(())
}
async fn run(app: AppHandle, flow: Arc<Flow>) {
    // Request an updated list while retaining the original cached-list behavior.
    let state = app.state::<AppState>();
    let scope = flow.plan.lock().expect("close plan").scope;
    let all = state
        .sessions
        .lock()
        .await
        .values()
        .cloned()
        .collect::<Vec<_>>();
    let mut refreshes = tokio::task::JoinSet::new();
    for session in all {
        if session
            .snapshot
            .lock()
            .is_ok_and(|s| scope.contains(s.server_id) && s.video_connected && s.can_control)
        {
            refreshes.spawn(async move {
                // A congested session must not block the chooser for every
                // other server; its cached list can still be displayed.
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    session.control(Control::ActiveUsers),
                )
                .await;
            });
        }
    }
    while refreshes.join_next().await.is_some() {}
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    loop {
        if !state.shutdown.is_current(&flow) {
            return;
        }
        let (scope, phase) = {
            let p = flow.plan.lock().expect("close plan");
            (p.scope, p.phase)
        };
        if matches!(phase, Phase::Preparing | Phase::Choosing) {
            let all = state
                .sessions
                .lock()
                .await
                .values()
                .cloned()
                .collect::<Vec<_>>();
            if all.iter().any(|s| {
                s.snapshot.lock().is_ok_and(|s| {
                    scope.contains(s.server_id) && s.can_control && !s.sharing.requests.is_empty()
                })
            }) {
                state.shutdown.remove(&flow);
                failure(&app, "尚有权限申请需要应答；请处理申请后再关闭。".into());
                return;
            }
            if let Err(message) = refresh_plan(&app, &flow).await {
                state.shutdown.remove(&flow);
                failure(&app, message);
                return;
            }
        }
        changed(&app);
        let (scope, phase, transfers) = {
            let p = flow.plan.lock().expect("close plan");
            (p.scope, p.phase, p.transfers())
        };
        if phase == Phase::Cancelled {
            state.shutdown.remove(&flow);
            return;
        }
        if phase == Phase::Closing {
            if scope == Scope::Application {
                state.shutting_down.store(true, Ordering::Release);
            } else if let Scope::Server(id) = scope {
                if let Ok(mut closing) = state.closing_servers.lock() {
                    closing.insert(id);
                }
            }
            let mut writes = tokio::task::JoinSet::new();
            for (id, user) in transfers {
                if let Some(session) = state.sessions.lock().await.get(&id).cloned() {
                    writes.spawn(async move { (id, session.transfer_on_exit(user).await) });
                }
            }
            while let Some(result) = writes.join_next().await {
                if let Ok((id, Err(error))) = result {
                    let message = format!("{id} 控制权限转交未确认写出：{error}");
                    eprintln!("{message}");
                    if let Ok(mut p) = flow.plan.lock() {
                        p.messages.push(message);
                    }
                    changed(&app);
                }
            }
            if scope == Scope::Application {
                loop {
                    let changed = state.connection_changes.notified();
                    tokio::pin!(changed);
                    changed.as_mut().enable();
                    if state.connecting.lock().is_ok_and(|p| p.is_empty()) {
                        break;
                    }
                    changed.await;
                }
                let sessions = state
                    .sessions
                    .lock()
                    .await
                    .values()
                    .cloned()
                    .collect::<Vec<_>>();
                let mut stops = tokio::task::JoinSet::new();
                for session in sessions {
                    stops.spawn(async move {
                        session.stop().await;
                    });
                }
                while stops.join_next().await.is_some() {}
                state.playback.close().await;
                state.folders.shutdown().await;
                state.diagnostics.push(
                    amikvm_core::diagnostics::Level::Info,
                    amikvm_core::diagnostics::Category::Application,
                    None,
                    "应用正在退出",
                    "",
                );
                let logger = state.diagnostics.clone();
                let _ = tauri::async_runtime::spawn_blocking(move || logger.stop_file()).await;
                state.exit_ready.store(true, Ordering::Release);
                app.exit(0);
            } else if let Scope::Server(id) = scope {
                // An in-flight login observes closing_servers and logs out;
                // wait until it has finished before permitting a new connection.
                loop {
                    let changed = state.connection_changes.notified();
                    tokio::pin!(changed);
                    changed.as_mut().enable();
                    if state.connecting.lock().is_ok_and(|p| !p.contains(&id)) {
                        break;
                    }
                    changed.await;
                }
                let session = state.sessions.lock().await.remove(&id);
                if let Some(session) = session {
                    session.stop().await;
                }
                if let Ok(mut ui) = state.ui.lock() {
                    ui.soft_keyboard.remove(&id);
                    ui.paused.remove(&id);
                    if ui.selected == Some(id) {
                        ui.selected = None;
                    }
                }
                let messages = flow
                    .plan
                    .lock()
                    .map(|p| p.messages.join("\n"))
                    .unwrap_or_default();
                if let Ok(mut closing) = state.closing_servers.lock() {
                    closing.remove(&id);
                }
                state.shutdown.remove(&flow);
                if !messages.is_empty() {
                    failure(&app, messages);
                }
                changed(&app);
                if flow.quit_after.load(Ordering::Acquire) {
                    request(&app, Scope::Application);
                }
            }
            return;
        }
        tokio::select! { _ = flow.changed.notified() => {}, _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {} }
    }
}

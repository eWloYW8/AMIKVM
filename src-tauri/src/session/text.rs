//! Clipboard access, routing policy and cancellable text transmission stay in Rust.
use super::*;
use input::{TextPhase, TextPlan, TextStatus, routing};

impl Session {
    pub(crate) fn cancel_text(&self) {
        self.text_generation
            .send_modify(|value| *value = value.wrapping_add(1));
    }

    pub async fn stop_text(&self) {
        self.cancel_text();
        loop {
            let done = self.text_done.notified();
            tokio::pin!(done);
            done.as_mut().enable();
            if !self.text_active.load(Ordering::Acquire) {
                break;
            }
            done.await;
        }
    }

    pub async fn keyboard_option(&self, setting: routing::Setting) -> Result<()> {
        self.input_ready()?;
        self.input(Event::ReleaseAll).await?;
        self.stop_text().await;
        self.input_ready()?;
        update(&self.input_app, &self.snapshot, |s| {
            s.keyboard_options.apply(setting)
        });
        Ok(())
    }

    /// Read only on explicit Paste/Ctrl+V; the clipboard never enters the webview.
    pub async fn paste(self: &Arc<Self>) -> Result<()> {
        self.key_input_ready()?;
        let generation = *self.text_generation.borrow();
        let text = crate::clipboard::read(self.input_app.clone()).await?;
        let mode = self
            .snapshot
            .lock()
            .map_err(|_| Error::Invalid("Session unavailable".into()))?
            .keyboard_options
            .text_mode;
        self.start_text(&text, mode, generation).await
    }

    pub async fn type_text(self: &Arc<Self>, text: &str, mode: TextMode) -> Result<()> {
        let generation = *self.text_generation.borrow();
        self.start_text(text, mode, generation).await
    }

    async fn start_text(
        self: &Arc<Self>,
        text: &str,
        mode: TextMode,
        generation: u64,
    ) -> Result<()> {
        self.key_input_ready()?;
        let plan = input::text_plan(text, mode)?;
        if plan.ends.is_empty() {
            return Err(Error::Invalid("请输入要发送的文本".into()));
        }
        // A GTK selection callback may finish before the queued window event.
        // Query native focus before reserving a job or writing its baseline.
        if !self
            .input_app
            .get_webview_window("main")
            .is_some_and(|window| window.is_focused().unwrap_or(false))
        {
            return Err(Error::Invalid("文本输入已停止".into()));
        }
        let mut keyboard = self.keyboard.lock().await;
        self.key_input_ready()?;
        if *self.text_generation.borrow() != generation {
            return Err(Error::Invalid("文本输入已停止".into()));
        }
        // A successful reservation precedes all writes and UI state changes.
        self.text_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::Invalid("请先停止当前文本输入".into()))?;
        let baseline = keyboard.release_physical();
        if let Err(error) = self.send_keyboard(&mut keyboard, baseline).await {
            self.text_active.store(false, Ordering::Release);
            self.text_done.notify_waiters();
            return Err(error);
        }
        update(&self.input_app, &self.snapshot, |s| {
            s.keyboard_options.text_mode = mode;
            s.text_input = TextStatus {
                phase: TextPhase::Running,
                sent: 0,
                total: plan.ends.len(),
                error: None,
            };
        });
        drop(keyboard);
        let session = self.clone();
        tauri::async_runtime::spawn(async move {
            session.run_text(plan, generation).await;
        });
        Ok(())
    }

    async fn run_text(&self, plan: TextPlan, generation: u64) {
        let mut changed = self.text_generation.subscribe();
        let mut sent = 0;
        let total = plan.ends.len();
        let mut last_notice = std::time::Instant::now();
        let result: Result<()> = async {
            for (index, report) in plan.reports.into_iter().enumerate() {
                if *changed.borrow_and_update() != generation {
                    return Err(Error::Invalid("文本输入已停止".into()));
                }
                self.input_ready()?;
                if self.snapshot.lock().is_ok_and(|s| s.mouse.active()) {
                    return Err(Error::Invalid("请先结束当前鼠标校准".into()));
                }
                let (reply, written) = oneshot::channel();
                tokio::select! {
                    biased;
                    _ = changed.changed() => return Err(Error::Invalid("文本输入已停止".into())),
                    result = async {
                        self.sender.send(Outgoing::Hid {
                            mouse: false, report: report.to_vec(), reply: Some(reply), text_generation: Some(generation),
                        }).await.map_err(|_| Error::Protocol("Connection closed".into()))?;
                        written.await.map_err(|_| Error::Protocol("Connection closed".into()))?
                    } => result?,
                }
                while sent < total && plan.ends[sent] <= index + 1 { sent += 1; }
                if last_notice.elapsed() >= Duration::from_millis(250) {
                    update(&self.input_app, &self.snapshot, |s| s.text_input.sent = sent);
                    last_notice = std::time::Instant::now();
                }
                tokio::select! {
                    biased;
                    _ = changed.changed() => return Err(Error::Invalid("文本输入已停止".into())),
                    _ = tokio::time::sleep(Duration::from_millis(15)) => {},
                }
            }
            Ok(())
        }.await;
        // Never retain the keyboard mutex while waiting for the writer. Native
        // focus/permission/calibration changes can clear it and cancel this job.
        let mut keyboard = self.keyboard.lock().await;
        if self.input_ready().is_err() || self.snapshot.lock().is_ok_and(|s| s.mouse.active()) {
            keyboard.clear();
        }
        let baseline = keyboard.report();
        let restore = if self.input_ready().is_ok() {
            self.send_keyboard(&mut keyboard, baseline).await
        } else {
            Ok(())
        };
        let (phase, error) = if *changed.borrow() != generation {
            (TextPhase::Cancelled, None)
        } else if let Err(error) = result.and(restore) {
            (TextPhase::Failed, Some(error.to_string()))
        } else {
            (TextPhase::Complete, None)
        };
        update(&self.input_app, &self.snapshot, |s| {
            s.software_keys = keyboard.software_keys();
            s.text_input = TextStatus {
                phase,
                sent,
                total,
                error,
            };
        });
        self.text_active.store(false, Ordering::Release);
        self.text_done.notify_waiters();
    }
}

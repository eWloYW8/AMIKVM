use super::{Dialog, Node, UiState, View, active};
use crate::locale::{message as translated, text as tr};
use crate::session::Snapshot;
use amikvm_core::server::Server;
use serde_json::{Value, json};

fn logs(ui: &UiState, servers: &[Server]) -> Node {
    use amikvm_core::diagnostics::{Category, Level};
    let s = &ui.logs;
    let levels = [Level::Debug, Level::Info, Level::Warning, Level::Error]
        .map(|level| (level.value(), tr(level.label())));
    let mut categories = vec![("", tr("所有类别"))];
    categories.extend(Category::ALL.map(|c| (c.value(), tr(c.label()))));
    let mut server_filter = select("server", tr("服务器"), &[("", tr("所有服务器"))], json!({}));
    server_filter.props["options"] = json!(
        std::iter::once(json!({"value":"","label":tr("所有服务器")}))
            .chain(
                servers
                    .iter()
                    .map(|server| json!({"value":server.id,"label":server.name}))
            )
            .collect::<Vec<_>>()
    );
    let mut controls = vec![
        group(
            "div",
            "page-heading",
            vec![
                group("div", "", vec![label("h1", "", tr("诊断日志"))]),
                button(
                    "button secondary",
                    if s.file.is_some() {
                        tr("停止文件日志")
                    } else {
                        tr("追加到日志文件")
                    },
                    "FolderOpen",
                    json!({"action":"log_file"}),
                    false,
                ),
            ],
        ),
        node(
            "form",
            json!({"key":format!("log-settings-{}-{:?}-{}",s.enabled,s.minimum,s.console),"values":{"enabled":s.enabled,"minimum":s.minimum,"console":s.console},"action":{"action":"log_configure"}}),
            vec![group(
                "div",
                "log-settings",
                vec![
                    field("enabled", tr("记录诊断日志"), "checkbox", json!({})),
                    select("minimum", tr("记录级别"), &levels, json!({})),
                    field("console", tr("同时输出到控制台"), "checkbox", json!({})),
                    node(
                        "button",
                        json!({"className":"button secondary","type":"submit","text":tr("应用")}),
                        vec![],
                    ),
                ],
            )],
        ),
        node(
            "form",
            json!({"key":format!("log-filter-{:?}-{:?}-{:?}-{}",ui.log_filter.minimum,ui.log_filter.category,ui.log_filter.server,ui.log_filter.query),"values":{"minimum":ui.log_filter.minimum,"category":ui.log_filter.category.map(|c|c.value()).unwrap_or(""),"server":ui.log_filter.server.map(|id|id.to_string()).unwrap_or_default(),"query":ui.log_filter.query},"action":{"action":"log_filter"}}),
            vec![group(
                "div",
                "log-filters",
                vec![
                    select("minimum", tr("显示级别"), &levels, json!({})),
                    select("category", tr("类别"), &categories, json!({})),
                    server_filter,
                    field(
                        "query",
                        tr("搜索日志"),
                        "text",
                        json!({"placeholder":tr("事件、详情、时间或服务器编号"),"maxLength":256}),
                    ),
                    node(
                        "button",
                        json!({"className":"button secondary","type":"submit","text":tr("筛选")}),
                        vec![],
                    ),
                ],
            )],
        ),
    ];

    if let Some(path) = &s.file {
        controls.push(label("p", "record-path", path.display()));
    }
    if let Some(error) = &s.file_error {
        controls.push(label("p", "inline-error", translated(error)));
    }
    if s.file_dropped != 0 {
        controls.push(label(
            "p",
            "inline-error",
            lformat!(
                "文件写入队列已跳过 {} 条；内存日志继续保留最近的记录。",
                s.file_dropped
            ),
        ));
    }
    controls.push(group(
        "div",
        "log-toolbar",
        vec![
            label(
                "span",
                "input-help",
                lformat!(
                    "匹配 {} 条 · 保留最近 {} 条 · 累计 {} 条",
                    s.matched,
                    s.retained,
                    s.total
                ),
            ),
            button(
                "button secondary",
                tr("导出筛选后的日志"),
                "FolderOpen",
                json!({"action":"log_export"}),
                s.matched == 0,
            ),
            button(
                "button secondary",
                tr("清空内存日志"),
                "X",
                json!({"action":"log_clear"}),
                s.retained == 0,
            ),
        ],
    ));
    let mut rows = vec![];
    for entry in &s.entries {
        let server = entry
            .server_id
            .and_then(|id| servers.iter().find(|s| s.id == id))
            .map(|s| s.name.clone())
            .unwrap_or_else(|| {
                entry
                    .server_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| tr("应用程序").into())
            });
        let mut row = group(
            "article",
            &format!("log-entry log-{}", entry.level.value()),
            vec![
                group(
                    "div",
                    "log-entry-heading",
                    vec![
                        label("span", "log-time", &entry.timestamp),
                        label("strong", "", tr(entry.level.label())),
                        label("span", "", tr(entry.category.label())),
                        label("span", "log-server", server),
                    ],
                ),
                label("strong", "log-source", tr(entry.source)),
                label("p", "log-details", translated(&entry.details)),
            ],
        );
        row.props["key"] = json!(entry.sequence);
        rows.push(row);
    }
    controls.push(group("div", "log-list", rows));
    controls.push(group(
        "div",
        "log-pagination",
        vec![
            button(
                "button secondary",
                tr("上一页"),
                "",
                json!({"action":"log_page","value":s.page.saturating_sub(1)}),
                s.page == 0,
            ),
            label("span", "", lformat!("第 {} / {} 页", s.page + 1, s.pages)),
            button(
                "button secondary",
                tr("下一页"),
                "",
                json!({"action":"log_page","value":s.page+1}),
                s.page + 1 >= s.pages,
            ),
        ],
    ));
    group("section", "server-page logs-page", controls)
}

fn about() -> Node {
    let mut rows = vec![heading(tr("关于 AMIKVM"), "Monitor")];
    for (caption, value) in [
        (tr("应用版本"), env!("CARGO_PKG_VERSION")),
        (tr("核心版本"), amikvm_core::VERSION),
        (tr("构建平台"), env!("AMIKVM_BUILD_TARGET")),
        (tr("Rust 工具链"), env!("AMIKVM_RUSTC")),
        ("Tauri", env!("AMIKVM_TAURI_VERSION")),
        ("OpenH264", env!("AMIKVM_OPENH264_VERSION")),
        ("MP4", env!("AMIKVM_MP4_VERSION")),
        ("fatfs", env!("AMIKVM_FATFS_VERSION")),
    ] {
        rows.push(group(
            "div",
            "about-row",
            vec![label("span", "", caption), label("strong", "", value)],
        ));
    }

    rows.push(label("h3", "", tr("第三方许可 · fatfs")));
    rows.push(label(
        "p",
        "about-license",
        include_str!("../../../public/licenses/fatfs.txt"),
    ));
    rows.push(group(
        "div",
        "modal-actions",
        vec![button(
            "button secondary",
            tr("关闭"),
            "",
            json!({"action":"close_dialog"}),
            false,
        )],
    ));
    node(
        "dialog",
        json!({"key":"about","className":"modal modal-wide about-modal","action":{"action":"close_dialog"}}),
        rows,
    )
}

fn recovery_message(snapshot: &Snapshot) -> String {
    let recovery = &snapshot.recovery;
    lformat!(
        "{} · 重试 {}/{} · {} 秒",
        tr(recovery.stage.label()),
        recovery.attempt,
        recovery.limit,
        recovery.seconds_remaining
    )
}

fn connection_info(server: &Server, snapshot: Option<&Snapshot>) -> Node {
    let mut rows = vec![heading(
        &format!("{} · {}", tr("连接信息"), server.name),
        "Info",
    )];
    if let Some(snapshot) = snapshot {
        if let Some(serial) = &snapshot.native_serial {
            rows.push(group(
                "div",
                "about-row",
                vec![
                    label("span", "", tr("原生序列号")),
                    label("strong", "", serial),
                ],
            ));
        }
        if let Some(config) = &snapshot.config {
            for (caption, value) in [
                (tr("KVM 端口"), config.kvm_port.to_string()),
                (tr("CD/DVD 实例数"), config.cd_instances.to_string()),
                (tr("磁盘实例数"), config.hd_instances.to_string()),
                (tr("配置的重试次数"), config.retry_count.to_string()),
                (
                    tr("配置的重试间隔（秒）"),
                    config.retry_interval.to_string(),
                ),
            ] {
                rows.push(group(
                    "div",
                    "about-row",
                    vec![label("span", "", caption), label("strong", "", value)],
                ));
            }
        }
        if snapshot.recovery.stage != amikvm_core::recovery::Stage::Idle {
            rows.push(label("p", "input-help", recovery_message(snapshot)));
            if let Some(error) = &snapshot.recovery.last_error {
                rows.push(label("p", "input-help", error));
            }
        }
        if let Some(notice) = snapshot.service.notice {
            rows.push(label("p", "input-help", tr(notice)));
        }
        if let Some(message) = &snapshot.message {
            rows.push(label("p", "input-help", translated(message)));
        }
        for change in &snapshot.service.changes {
            rows.push(label(
                "p",
                "input-help",
                format!(
                    "{}: {}",
                    change.service,
                    change
                        .fields
                        .iter()
                        .map(|field| tr(field))
                        .collect::<Vec<_>>()
                        .join(" · ")
                ),
            ));
        }
        for service in &snapshot.service.services {
            rows.push(group(
                "div",
                "service-card",
                vec![
                    label("h3", "", &service.name),
                    label(
                        "p",
                        "input-help",
                        if service.enabled {
                            tr("服务运行中")
                        } else {
                            tr("服务已禁用")
                        },
                    ),
                    group(
                        "div",
                        "service-details",
                        vec![
                            label("span", "", lformat!("网络接口：{}", service.interface)),
                            label("span", "", lformat!("非加密端口：{}", service.port)),
                            label("span", "", lformat!("加密端口：{}", service.secure_port)),
                            label(
                                "span",
                                "",
                                lformat!("空闲超时：{} 秒", service.inactivity_seconds),
                            ),
                            label("span", "", lformat!("最大会话数：{}", service.max_sessions)),
                            label(
                                "span",
                                "",
                                lformat!(
                                    "空闲超时范围：{}–{} 秒",
                                    service.minimum_inactivity_seconds,
                                    service.maximum_inactivity_seconds
                                ),
                            ),
                        ],
                    ),
                ],
            ));
        }
    }
    rows.push(group(
        "div",
        "modal-actions",
        vec![button(
            "button secondary",
            tr("关闭"),
            "",
            json!({"action":"close_dialog"}),
            false,
        )],
    ));
    node(
        "dialog",
        json!({"key":format!("connection-{}",server.id),"className":"modal modal-wide service-modal","action":{"action":"close_dialog"}}),
        rows,
    )
}

fn node(kind: &'static str, props: Value, children: Vec<Node>) -> Node {
    Node {
        kind,
        props,
        children,
    }
}
fn group(tag: &'static str, class: &str, children: Vec<Node>) -> Node {
    node("element", json!({"tag": tag, "className": class}), children)
}
fn label(tag: &'static str, class: &str, value: impl ToString) -> Node {
    node(
        "text",
        json!({"tag":tag, "className":class, "text":value.to_string()}),
        vec![],
    )
}
fn icon(name: &str, size: u8) -> Node {
    node("icon", json!({"name":name, "size":size}), vec![])
}
fn button(class: &str, text: &str, image: &str, action: Value, disabled: bool) -> Node {
    node(
        "button",
        json!({"className":class,"text":text,"icon":image,"action":action,"disabled":disabled}),
        vec![],
    )
}
fn titled(image: &str, title: &str, action: Value, disabled: bool) -> Node {
    let mut n = button("icon-button", "", image, action, disabled);
    n.props["title"] = json!(title);
    n
}
fn dot(class: &str) -> Node {
    group("span", &format!("status-dot {class}"), vec![])
}
fn field(name: &str, title: &str, kind: &str, extra: Value) -> Node {
    let mut props = json!({"name":name,"label":title,"type":kind});
    if let Some(extra) = extra.as_object() {
        props.as_object_mut().unwrap().extend(extra.clone());
    }
    node("field", props, vec![])
}
fn select(name: &str, title: &str, choices: &[(&str, &str)], extra: Value) -> Node {
    let mut f = field(name, title, "select", extra);
    f.props["options"] = json!(
        choices
            .iter()
            .map(|(v, t)| json!({"value":v,"label":t}))
            .collect::<Vec<_>>()
    );
    f
}
fn phase(s: Option<&Snapshot>) -> &'static str {
    match s.map(|s| s.phase.as_str()) {
        Some("authenticating") => tr("正在登录"),
        Some("negotiating") => tr("正在连接"),
        Some("reconnecting") => tr("正在自动重连"),
        Some("connected") => tr("已连接"),
        Some("error") => tr("连接失败"),
        _ => tr("未连接"),
    }
}
fn status(s: Option<&Snapshot>) -> &'static str {
    match s.map(|s| s.phase.as_str()) {
        Some("connected") => "online",
        Some("authenticating" | "negotiating" | "reconnecting") => "pending",
        _ => "",
    }
}
fn heading(title: &str, image: &str) -> Node {
    group(
        "div",
        "modal-heading",
        vec![
            group("div", "modal-icon", vec![icon(image, 23)]),
            group("div", "", vec![label("h2", "", title)]),
            titled("X", tr("关闭"), json!({"action":"close_dialog"}), false),
        ],
    )
}
fn actions(caption: &str) -> Node {
    group(
        "div",
        "modal-actions",
        vec![
            button(
                "button secondary",
                tr("取消"),
                "",
                json!({"action":"close_dialog"}),
                false,
            ),
            node(
                "button",
                json!({"className":"button primary","text":caption,"type":"submit"}),
                vec![],
            ),
        ],
    )
}

pub fn build(ui: &UiState, servers: &[Server], sessions: &[Snapshot]) -> Node {
    let active_sessions: Vec<_> = sessions.iter().filter(|s| active(s)).collect();
    let selected = servers.iter().find(|s| Some(s.id) == ui.selected);
    let name = match ui.view {
        View::All => tr("所有服务器"),
        View::Favorites => tr("收藏服务器"),
        View::Recent => tr("最近连接"),
        View::Logs => tr("诊断日志"),
    };
    let mut nav = vec![];
    for (view, title, image) in [
        (View::All, tr("所有服务器"), "LayoutGrid"),
        (View::Favorites, tr("收藏服务器"), "Star"),
        (View::Recent, tr("最近连接"), "Clock3"),
        (View::Logs, tr("诊断日志"), "Activity"),
    ] {
        let action = json!({"action":"navigate","view": match view { View::All => "all", View::Favorites => "favorites", View::Recent => "recent",View::Logs=>"logs" }});
        let mut b = button(
            if selected.is_none() && !ui.playback_selected && ui.view == view {
                "selected"
            } else {
                ""
            },
            title,
            image,
            action,
            false,
        );
        if view == View::All {
            b.children.push(label("span", "nav-count", servers.len()));
        }
        nav.push(b);
    }
    nav.push(button(
        "",
        tr("用户组合键"),
        "Keyboard",
        json!({"action":"macro_dialog","id":ui.selected}),
        false,
    ));
    nav.push(button(
        if ui.playback_selected { "selected" } else { "" },
        tr("录像回放"),
        "Video",
        json!({"action":"playback_view"}),
        false,
    ));
    let mut session_nav = vec![];
    for s in &active_sessions {
        if let Some(server) = servers.iter().find(|server| server.id == s.server_id) {
            let mut b = button(
                if selected.is_some_and(|server| server.id == s.server_id) {
                    "selected"
                } else {
                    ""
                },
                "",
                "",
                json!({"action":"select","id":s.server_id}),
                false,
            );
            b.children = vec![
                dot(status(Some(s))),
                label("span", "", &server.name),
                icon("ChevronRight", 14),
            ];
            session_nav.push(b);
        }
    }
    for server in servers.iter().filter(|server| {
        ui.folders.iter().any(|f| f.server_id == server.id)
            && !active_sessions.iter().any(|s| s.server_id == server.id)
    }) {
        session_nav.push(button(
            if selected.is_some_and(|s| s.id == server.id) {
                "selected"
            } else {
                ""
            },
            &lformat!("{} · 文件夹", server.name),
            "FolderOpen",
            json!({"action":"select","id":server.id}),
            false,
        ));
    }
    let navigation_count = session_nav.len();
    let sidebar = group(
        "aside",
        "sidebar",
        vec![
            group(
                "div",
                "brand",
                vec![
                    node("image", json!({"src":"/brand.svg","alt":""}), vec![]),
                    group("span", "", vec![label("span", "", "AMIKVM")]),
                ],
            ),
            label("div", "workspace-label", tr("工作空间")),
            group("nav", "", nav),
            group(
                "div",
                "sidebar-section",
                vec![
                    label(
                        "span",
                        "",
                        if ui.folders.is_empty() {
                            tr("活动控制台")
                        } else {
                            tr("控制台与文件夹")
                        },
                    ),
                    label("span", "nav-count", navigation_count),
                ],
            ),
            group("div", "session-nav", session_nav),
            group(
                "div",
                "sidebar-bottom",
                vec![
                    group(
                        "div",
                        "connection-count",
                        vec![
                            icon("Activity", 16),
                            label(
                                "span",
                                "",
                                if active_sessions.is_empty() {
                                    tr("准备连接").into()
                                } else {
                                    lformat!("{} 个活动连接", active_sessions.len())
                                },
                            ),
                            dot("online"),
                        ],
                    ),
                    group(
                        "div",
                        "app-version",
                        vec![
                            label("span", "", "AMIKVM"),
                            label("span", "", concat!("v", env!("CARGO_PKG_VERSION"))),
                            titled(
                                "Circle",
                                tr("关于 AMIKVM"),
                                json!({"action":"about"}),
                                false,
                            ),
                            titled("X", tr("关闭 AMIKVM"), json!({"action":"quit"}), false),
                        ],
                    ),
                ],
            ),
        ],
    );
    let topbar = group(
        "header",
        "topbar",
        vec![
            group(
                "div",
                "breadcrumb",
                vec![
                    icon("TerminalSquare", 17),
                    label("span", "", tr("工作空间")),
                    icon("ChevronRight", 13),
                    label(
                        "strong",
                        "",
                        if ui.playback_selected {
                            tr("录像回放")
                        } else {
                            selected.map_or(name, |s| s.name.as_str())
                        },
                    ),
                ],
            ),
            group(
                "div",
                "topbar-controls",
                vec![select(
                    "language",
                    tr("界面语言"),
                    &[("zh-CN", "简体中文"), ("en", "English"), ("fr", "Français")],
                    json!({"className":"language-select","key":"interface-language","value":ui.language.code(),"action":{"action":"set_language"},"disabled":ui.close_plan.is_some()}),
                )],
            ),
        ],
    );
    let content = if ui.playback_selected {
        playback(ui)
    } else if ui.view == View::Logs {
        logs(ui, servers)
    } else {
        selected.map_or_else(
            || server_page(ui, servers, sessions, active_sessions.len()),
            |s| console(ui, s, sessions.iter().find(|v| v.server_id == s.id)),
        )
    };
    let mut children = vec![sidebar, group("main", "main", vec![topbar, content])];
    if let Some(plan) = &ui.close_plan {
        children.push(exit_dialog(plan));
    } else if let Some(dialog) = dialog(ui, servers, sessions) {
        children.push(dialog);
    }
    if let Some(error) = &ui.error {
        children.push(group(
            "div",
            "toast",
            vec![
                label("span", "", translated(error)),
                titled(
                    "X",
                    tr("关闭提示"),
                    json!({"action":"dismiss_error"}),
                    false,
                ),
            ],
        ));
    }
    let mut root = group("div", "app-shell", children);
    root.props["lang"] = json!(ui.language.code());
    root.props["inputServers"] = json!(
        sessions
            .iter()
            .filter(|s| s.video_connected && s.can_control)
            .map(|s| s.server_id)
            .collect::<Vec<_>>()
    );
    root
}

fn server_page(
    ui: &UiState,
    servers: &[Server],
    sessions: &[Snapshot],
    connections: usize,
) -> Node {
    let title = match ui.view {
        View::All | View::Logs => tr("服务器"),
        View::Favorites => tr("收藏服务器"),
        View::Recent => tr("最近连接"),
    };
    let query = ui.query.to_lowercase();
    let mut filtered: Vec<_> = servers
        .iter()
        .filter(|s| {
            (ui.view != View::Favorites || s.favorite)
                && (ui.view != View::Recent || s.last_connected_at.is_some())
                && format!("{} {} {} {}", s.name, s.host, s.tags.join(" "), s.notes)
                    .to_lowercase()
                    .contains(&query)
        })
        .collect();
    filtered.sort_by(|a, b| {
        if ui.view == View::Recent {
            b.last_connected_at
                .cmp(&a.last_connected_at)
                .then(a.name.cmp(&b.name))
        } else {
            b.favorite
                .cmp(&a.favorite)
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        }
    });
    let mut stats = vec![];
    for (caption, count, unit, image, class) in [
        (
            tr("服务器总数"),
            servers.len(),
            tr("台"),
            "Server",
            "stat-icon",
        ),
        (
            tr("活动连接"),
            connections,
            tr("个控制台"),
            "Monitor",
            "stat-icon green",
        ),
        (
            tr("安全连接"),
            servers.iter().filter(|s| s.https).count(),
            "HTTPS",
            "ShieldCheck",
            "stat-icon",
        ),
    ] {
        stats.push(group(
            "div",
            "",
            vec![
                group("div", class, vec![icon(image, 19)]),
                group(
                    "span",
                    "",
                    vec![
                        label("small", "", caption),
                        group(
                            "strong",
                            "",
                            vec![label("span", "", count), label("em", "", unit)],
                        ),
                    ],
                ),
            ],
        ));
    }
    let mut search = vec![
        icon("Search", 17),
        node(
            "input",
            json!({"value":ui.query,"placeholder":tr("搜索服务器名称、地址或备注…"),"label":tr("搜索服务器"),"action":{"action":"search"}}),
            vec![],
        ),
    ];
    if !ui.query.is_empty() {
        search.push(titled(
            "X",
            tr("清空搜索"),
            json!({"action":"search","value":""}),
            false,
        ));
    }
    let mut children = vec![
        group(
            "div",
            "page-heading",
            vec![
                group("div", "", vec![label("h1", "", title)]),
                button(
                    "button primary",
                    tr("添加服务器"),
                    "Plus",
                    json!({"action":"add"}),
                    false,
                ),
            ],
        ),
        group("div", "overview", stats),
        group(
            "div",
            "list-toolbar",
            vec![
                group("div", "search", search),
                label("span", "", lformat!("{} 台服务器", filtered.len())),
            ],
        ),
    ];
    if filtered.is_empty() {
        let title = if !ui.query.is_empty() {
            tr("没有找到匹配的服务器")
        } else {
            match ui.view {
                View::All | View::Logs => tr("暂无服务器"),
                View::Favorites => tr("暂无收藏服务器"),
                View::Recent => tr("暂无最近连接"),
            }
        };
        let mut empty = vec![
            group(
                "div",
                "empty-art",
                vec![
                    group("span", "", vec![]),
                    group("div", "", vec![icon("Server", 43)]),
                    group("span", "", vec![]),
                ],
            ),
            label("h2", "", title),
        ];
        if servers.is_empty() {
            empty.push(button(
                "button primary",
                tr("添加第一台服务器"),
                "Plus",
                json!({"action":"add"}),
                false,
            ));
        }

        children.push(group("div", "empty-state", empty));
    } else {
        children.push(group(
            "div",
            "server-grid",
            filtered
                .into_iter()
                .map(|s| card(ui, s, sessions.iter().find(|v| v.server_id == s.id)))
                .collect(),
        ));
    }

    group("div", "server-page", children)
}

fn card(ui: &UiState, s: &Server, snapshot: Option<&Snapshot>) -> Node {
    let mut favorite = titled(
        "Star",
        tr("收藏"),
        json!({"action":"favorite","id":s.id}),
        false,
    );
    if s.favorite {
        favorite.props["className"] = json!("icon-button favorite");
        favorite.props["fill"] = json!("currentColor");
    }
    let mut menu = vec![titled(
        "MoreHorizontal",
        tr("更多操作"),
        json!({"action":"menu","id":s.id}),
        false,
    )];
    let busy = snapshot.is_some_and(active);
    if ui.menu == Some(s.id) {
        let mut remove = button(
            "danger",
            tr("删除服务器"),
            "",
            json!({"action":"remove","id":s.id}),
            busy,
        );
        remove.props["confirm"] = json!(lformat!("删除服务器“{}”？", s.name));
        menu.push(group(
            "div",
            "dropdown",
            vec![
                button(
                    "",
                    tr("抓取预览画面"),
                    "Camera",
                    json!({"action":"capture_dialog","id":s.id,"kind":"preview"}),
                    snapshot.is_some_and(|s| {
                        matches!(
                            s.phase.as_str(),
                            "authenticating" | "negotiating" | "reconnecting"
                        )
                    }),
                ),
                button(
                    "",
                    tr("查看蓝屏捕获"),
                    "Camera",
                    json!({"action":"capture_dialog","id":s.id,"kind":"crash"}),
                    snapshot.is_some_and(|s| {
                        matches!(
                            s.phase.as_str(),
                            "authenticating" | "negotiating" | "reconnecting"
                        )
                    }),
                ),
                button(
                    "",
                    tr("编辑服务器"),
                    "",
                    json!({"action":"edit","id":s.id}),
                    busy,
                ),
                remove,
            ],
        ));
    }
    let mut n = group(
        "article",
        "server-card",
        vec![
            group(
                "div",
                "card-top",
                vec![
                    group("div", "server-symbol", vec![icon("Server", 22)]),
                    group(
                        "div",
                        "card-actions",
                        vec![favorite, group("div", "menu-anchor", menu)],
                    ),
                ],
            ),
            label("h3", "", &s.name),
            group(
                "p",
                "server-address",
                vec![
                    label("span", "", &s.host),
                    label("span", "", format!(":{}", s.web_port)),
                ],
            ),
            group(
                "div",
                "server-badges",
                vec![
                    group(
                        "span",
                        "",
                        vec![
                            icon(if s.https { "ShieldCheck" } else { "Monitor" }, 12),
                            label("span", "", if s.https { "HTTPS" } else { "HTTP" }),
                        ],
                    ),
                    label("span", "", &s.username),
                ],
            ),
            group(
                "div",
                "card-footer",
                vec![
                    group(
                        "span",
                        if snapshot.is_some_and(|v| v.phase == "error") {
                            "connection-state failed"
                        } else {
                            "connection-state"
                        },
                        vec![dot(status(snapshot)), label("span", "", phase(snapshot))],
                    ),
                    button(
                        "connect-button",
                        if snapshot.is_some_and(|s| s.web_only) {
                            tr("连接控制台")
                        } else if busy {
                            tr("打开控制台")
                        } else {
                            tr("连接")
                        },
                        "ArrowUpRight",
                        json!({"action":"connect","id":s.id}),
                        false,
                    ),
                ],
            ),
        ],
    );
    n.props["key"] = json!(s.id);
    n
}

fn dialog(ui: &UiState, servers: &[Server], sessions: &[Snapshot]) -> Option<Node> {
    let (key, class, action, values, children) = match ui.dialog {
        Dialog::About => return Some(about()),
        Dialog::Connection(id) => {
            let server = servers.iter().find(|s| s.id == id)?;
            return Some(connection_info(
                server,
                sessions.iter().find(|s| s.server_id == id),
            ));
        }
        Dialog::None => return None,
        Dialog::Confirmation => {
            let confirmation = ui.confirmation.as_ref()?;
            return Some(node(
                "dialog",
                json!({"key":format!("confirmation-{}",confirmation.id),"className":"modal","action":{"action":"close_dialog"}}),
                vec![
                    heading(tr("确认操作"), "ShieldCheck"),
                    label(
                        "p",
                        "confirmation-message",
                        translated(&confirmation.message),
                    ),
                    group(
                        "div",
                        "modal-actions",
                        vec![
                            button(
                                "button secondary",
                                tr("取消"),
                                "",
                                json!({"action":"close_dialog"}),
                                false,
                            ),
                            button(
                                "button primary",
                                tr("确认"),
                                "",
                                json!({"action":"confirm_apply","id":confirmation.id}),
                                false,
                            ),
                        ],
                    ),
                ],
            ));
        }
        Dialog::Sharing(id) => {
            let server = servers.iter().find(|s| s.id == id)?;
            let snapshot = sessions.iter().find(|s| s.server_id == id);
            let controllable = snapshot.is_some_and(|s| s.video_connected && s.can_control);
            return Some(node(
                "dialog",
                json!({"key":format!("sharing-{id}"),"className":"modal modal-wide","action":{"action":"close_dialog"}}),
                vec![
                    heading(tr("共享会话与控制权限"), "Users"),
                    sharing(server, snapshot, controllable),
                    group(
                        "div",
                        "modal-actions",
                        vec![button(
                            "button secondary",
                            tr("关闭"),
                            "",
                            json!({"action":"close_dialog"}),
                            false,
                        )],
                    ),
                ],
            ));
        }
        Dialog::Ipmi(id) => {
            let snapshot = sessions.iter().find(|s| s.server_id == id);
            let controllable = snapshot.is_some_and(|s| s.video_connected && s.can_control);
            let mut children = vec![
                heading(tr("IPMI 命令"), "TerminalSquare"),
                group(
                    "div",
                    "capture-actions",
                    vec![button(
                        "button secondary",
                        tr("清除已完成记录"),
                        "X",
                        json!({"action":"ipmi_clear","id":id}),
                        snapshot.is_none(),
                    )],
                ),
            ];
            if let Some(state) = snapshot.map(|s| &s.ipmi) {
                children.push(label(
                    "p",
                    if state.response_error.is_some() {
                        "input-help danger"
                    } else {
                        "hidden"
                    },
                    state.response_error.as_deref().unwrap_or_default(),
                ));
                let records: Vec<_> = state.records.iter().rev().map(|record| {
                    let mut parts = vec![
                        group("div", "ipmi-record-heading", vec![
                            label("strong", "", lformat!("#{} · {} · 编号 {}", record.sequence, tr(record.operation.label()), record.request_id)),
                            label("span", if record.phase == amikvm_core::ipmi::Phase::Complete { "" } else { "input-help" }, tr(record.phase.label())),
                        ]),
                    ];
                    if !record.command.is_empty() {
                        parts.push(label("small", "", tr("请求 · Hex")));
                        parts.push(label("p", "ipmi-bytes", amikvm_core::ipmi::hex(&record.command)));
                        parts.push(label("small", "", tr("请求 · ASCII")));
                        parts.push(label("p", "ipmi-bytes", amikvm_core::ipmi::ascii(&record.command)));
                    }
                    if let Some(response) = &record.response {
                        parts.push(label("small", "", lformat!("响应 · 完成码 0x{:04X}", response.completion_code)));
                        parts.push(label("p", "ipmi-bytes", if response.data.is_empty() { tr("（无响应数据）").into() } else { amikvm_core::ipmi::hex(&response.data) }));
                        parts.push(label("small", "", tr("响应 · ASCII")));
                        parts.push(label("p", "ipmi-bytes", amikvm_core::ipmi::ascii(&response.data)));
                    }
                    if let Some(message) = &record.message { parts.push(label("p", "input-help", translated(message))); }
                    node("element", json!({"tag":"article","key":record.sequence,"className":"ipmi-record"}), parts)
                }).collect();
                children.push(group("div", "ipmi-history", records));
            }
            children.extend([
                select("format", tr("输入格式"), &[("hex", tr("十六进制")), ("ascii", "ASCII")], json!({"disabled":!controllable})),
                field("command", tr("原始命令"), "textarea", json!({"required":true,"disabled":!controllable,"rows":3,"autoFocus":true,"placeholder":"00 01"})),

                group("div", "modal-actions", vec![button("button secondary", tr("关闭"), "", json!({"action":"close_dialog"}), false), node("button", json!({"className":"button primary","text":tr("发送命令"),"type":"submit","disabled":!controllable}), vec![])]),
            ]);
            (
                format!("ipmi-{id}"),
                "modal modal-wide",
                json!({"action":"ipmi","id":id}),
                json!({"format":"hex","command":""}),
                children,
            )
        }
        Dialog::Boot(id) => {
            use amikvm_core::ipmi::{BootDevice, BootPhase};
            let snapshot = sessions.iter().find(|s| s.server_id == id);
            let boot = snapshot.map(|s| s.ipmi.boot.clone()).unwrap_or_default();
            let controllable = snapshot.is_some_and(|s| s.video_connected && s.can_control);
            let disabled = !controllable || boot.busy();
            let mut children = vec![
                heading(tr("启动选项"), "Power"),
                group(
                    "div",
                    "capture-actions",
                    vec![button(
                        "button secondary",
                        tr("重新读取"),
                        "RefreshCw",
                        json!({"action":"boot_refresh","id":id}),
                        disabled,
                    )],
                ),
            ];
            let phase = match boot.phase {
                BootPhase::Idle => tr("尚未读取启动选项"),
                BootPhase::Reading => tr("正在读取启动选项…"),
                BootPhase::Writing => tr("正在应用启动选项…"),
                BootPhase::Confirming => tr("正在重新读取并确认…"),
                BootPhase::Ready => tr("已读取服务器启动选项"),
                BootPhase::Error => tr("启动选项操作失败"),
            };
            children.push(label("p", "input-help", phase));
            if let Some(message) = &boot.message {
                children.push(label(
                    "p",
                    if boot.phase == BootPhase::Error {
                        "input-help danger"
                    } else {
                        "input-help"
                    },
                    message,
                ));
            }
            let mut choices: Vec<_> = [
                BootDevice::NoChange,
                BootDevice::Pxe,
                BootDevice::CdDvd,
                BootDevice::HardDiskUsb,
                BootDevice::BiosSetup,
            ]
            .iter()
            .map(|d| (d.value(), tr(d.label())))
            .collect();
            if boot.options.as_ref().is_some_and(|o| o.device.is_none()) {
                choices.insert(0, ("unknown", tr("当前设备不在原版选项中，请选择设备")));
            }
            if let Some(options) = &boot.options {
                let current = options
                    .device
                    .map(|d| tr(d.label()).to_string())
                    .unwrap_or_else(|| lformat!("设备代码 0x{:02X}", options.device_code));
                children.push(label(
                    "p",
                    "input-help",
                    lformat!(
                        "服务器返回：{} · {} · {}",
                        current,
                        if options.valid {
                            tr("启动标志有效")
                        } else {
                            tr("启动覆盖未启用")
                        },
                        if options.uefi { "UEFI" } else { "Legacy BIOS" }
                    ),
                ));
            }
            children.extend([
                select("device", tr("启动设备"), &choices, json!({"disabled":disabled || boot.options.is_none()})),
                field("nextBootOnly", tr("仅下次启动"), "checkbox", json!({"className":"checkbox","disabled":disabled || boot.options.is_none()})),

                group("div", "modal-actions", vec![button("button secondary", tr("关闭"), "", json!({"action":"close_dialog"}), false), node("button", json!({"className":"button primary","text":tr("应用"),"type":"submit","disabled":disabled || boot.phase != BootPhase::Ready || boot.options.is_none()}), vec![])]),
            ]);
            let values = json!({"device":boot.options.as_ref().map_or("no_change", |o|o.device.map_or("unknown", |d| d.value())),"nextBootOnly":boot.options.as_ref().is_none_or(|o|o.next_boot_only)});
            (
                format!("boot-{id}-{}", boot.revision),
                "modal",
                json!({"action":"boot_apply","id":id}),
                values,
                children,
            )
        }
        Dialog::Captures(id) => {
            let capture = ui.captures.get(&id).cloned().unwrap_or_default();
            let connected = sessions
                .iter()
                .any(|s| s.server_id == id && s.phase == "connected");
            let kind = capture
                .kind
                .unwrap_or(amikvm_core::video::remote_capture::Kind::Preview);
            let mut children = vec![
                heading(tr(kind.label()), "Camera"),
                group(
                    "div",
                    "capture-actions",
                    vec![
                        button(
                            "button secondary",
                            tr("刷新"),
                            "RefreshCw",
                            json!({"action":"capture_refresh","id":id,"kind":kind}),
                            capture.busy || !connected,
                        ),
                        button(
                            "button secondary",
                            tr("保存 JPEG"),
                            "Camera",
                            json!({"action":"capture_save","id":id}),
                            capture.width == 0 || capture.busy,
                        ),
                    ],
                ),
                group(
                    "div",
                    "captured-surface",
                    vec![node(
                        "video",
                        json!({"serverId":capture.id,"enabled":false,"streaming":connected,"visible":capture.width>0,"style":{"maxWidth":"100%","maxHeight":"100%","cursor":"default"}}),
                        vec![],
                    )],
                ),
            ];
            if capture.busy {
                children.push(group(
                    "div",
                    "capture-actions",
                    vec![
                        label("p", "", tr("正在抓取 BMC 画面…")),
                        button(
                            "button secondary",
                            tr("取消"),
                            "",
                            json!({"action":"capture_cancel","id":id}),
                            false,
                        ),
                    ],
                ));
            }
            if capture.width > 0 {
                children.push(label(
                    "p",
                    "capture-dimensions",
                    if capture.width == capture.source_width
                        && capture.height == capture.source_height
                    {
                        format!("{} × {}", capture.width, capture.height)
                    } else {
                        format!(
                            "{} × {} → {} × {}",
                            capture.source_width,
                            capture.source_height,
                            capture.width,
                            capture.height
                        )
                    },
                ));
            }
            if let Some(error) = &capture.error {
                children.push(label("p", "form-error", error));
            }
            if let Some(path) = &capture.saved_path {
                children.push(label(
                    "p",
                    "saved-path",
                    lformat!("已保存：{path}", path = path),
                ));
            }
            (
                format!("capture-{id}"),
                "modal capture-modal",
                json!({"action":"close_dialog"}),
                json!({}),
                children,
            )
        }
        Dialog::Recordings(id) => {
            let recordings = ui.recordings.get(&id).cloned().unwrap_or_default();
            let busy = recordings.phase != "idle" && !recordings.phase.is_empty();
            let connected = sessions
                .iter()
                .any(|s| s.server_id == id && s.phase == "connected");
            let mut children = vec![
                heading(tr("BMC 录像"), "Video"),
                button(
                    "button secondary",
                    tr("刷新列表"),
                    "RefreshCw",
                    json!({"action":"recordings_refresh","id":id}),
                    busy || !connected,
                ),
            ];
            if busy {
                let progress = if recordings.phase == "catalog" {
                    tr("正在读取录像列表…").into()
                } else if let Some(total) = recordings.total {
                    lformat!(
                        "正在下载：{} / {}",
                        byte_size(recordings.downloaded),
                        byte_size(total)
                    )
                } else {
                    lformat!("正在下载：{}", byte_size(recordings.downloaded))
                };
                children.push(group(
                    "div",
                    "macro-row",
                    vec![
                        label("span", "macro-content", progress),
                        button(
                            "button secondary",
                            tr("取消操作"),
                            "Square",
                            json!({"action":"recordings_cancel","id":id}),
                            false,
                        ),
                    ],
                ));
            }
            let mut rows = vec![];
            for entry in &recordings.entries {
                rows.push(group("div", "macro-row", vec![
                    group("div", "macro-content", vec![label("strong", "", &entry.name)]),
                    group("div", "macro-actions", vec![
                        button("button secondary", tr("下载"), "", json!({"action":"recordings_download","id":id,"file":entry.file,"play":false}), busy || !connected),
                        button("button primary", tr("回放"), "Play", json!({"action":"recordings_download","id":id,"file":entry.file,"play":true}), busy || !connected),
                    ]),
                ]));
            }
            children.push(group("div", "macro-list", rows));
            if let Some(path) = &recordings.saved_path {
                children.push(label(
                    "p",
                    "input-help",
                    lformat!("已保存：{path}", path = path),
                ));
            }
            if let Some(error) = &recordings.error {
                children.push(label("p", "inline-error", translated(error)));
            }
            if !connected {
                children.push(label(
                    "p",
                    "inline-error",
                    tr("服务器已断开；已下载的录像仍可从录像回放页面打开。"),
                ));
            }
            (
                format!("recordings-{id}"),
                "modal recordings-modal",
                json!({"action":"close_dialog"}),
                json!({}),
                children,
            )
        }
        Dialog::Macros(id) => {
            let controllable = id.is_some_and(|id| {
                sessions.iter().any(|s| {
                    s.server_id == id && s.video_connected && s.can_control && !s.mouse.active()
                })
            });
            let mut children = vec![
                heading(tr("用户组合键"), "Keyboard"),
                button(
                    "button primary",
                    tr("添加组合键"),
                    "Plus",
                    json!({"action":"macro_edit","id":id,"macro_id":null}),
                    ui.macros.len() >= amikvm_core::input::macros::MAX_MACROS,
                ),
            ];
            let mut rows = vec![];
            for m in &ui.macros {
                let mut buttons = vec![button(
                    "button secondary",
                    tr("编辑"),
                    "",
                    json!({"action":"macro_edit","id":id,"macro_id":m.id}),
                    false,
                )];
                if let Some(id) = id {
                    buttons.insert(
                        0,
                        button(
                            "button secondary",
                            tr("发送"),
                            "",
                            json!({"action":"macro_run","id":id,"macro_id":m.id}),
                            !controllable,
                        ),
                    );
                }
                let mut remove = titled(
                    "X",
                    tr("删除组合键"),
                    json!({"action":"macro_remove","macro_id":m.id}),
                    false,
                );
                remove.props["confirm"] = json!(lformat!("删除组合键“{}”？", m.name));
                buttons.push(remove);
                let mut row = group(
                    "div",
                    "macro-row",
                    vec![
                        group("div", "macro-content", vec![label("strong", "", &m.name)]),
                        group("div", "macro-actions", buttons),
                    ],
                );
                row.props["key"] = json!(m.id);
                rows.push(row);
            }
            children.push(group("div", "macro-list", rows));
            if let Some(id) = id {
                children.push(button(
                    "button secondary",
                    tr("服务器组合键"),
                    "Server",
                    json!({"action":"remote_macro_dialog","id":id}),
                    !sessions
                        .iter()
                        .any(|s| s.server_id == id && s.video_connected),
                ));
            }
            (
                format!("macros-{id:?}"),
                "modal",
                json!({"action":"close_dialog"}),
                json!({}),
                children,
            )
        }
        Dialog::RemoteMacros(id) => {
            let snapshot = sessions.iter().find(|s| s.server_id == id);
            let config = snapshot.and_then(|s| s.remote_macros.as_ref());
            let writable = snapshot
                .is_some_and(|s| s.video_connected && s.can_control && !s.remote_macros_busy());
            let connected = snapshot.is_some_and(|s| s.video_connected);
            let mut children = vec![
                heading(tr("服务器组合键"), "Keyboard"),
                group(
                    "div",
                    "macro-actions",
                    vec![
                        button(
                            "button primary",
                            tr("添加组合键"),
                            "Plus",
                            json!({"action":"remote_macro_edit","id":id,"slot":null}),
                            !writable || config.is_none_or(|c| c.vacant().is_none()),
                        ),
                        button(
                            "button secondary",
                            tr("刷新"),
                            "RefreshCw",
                            json!({"action":"remote_macro_refresh","id":id}),
                            !connected,
                        ),
                        button(
                            "button secondary",
                            tr("本地组合键"),
                            "Keyboard",
                            json!({"action":"macro_dialog","id":id}),
                            false,
                        ),
                    ],
                ),
            ];
            if let Some(message) = snapshot.and_then(|s| s.remote_macro_message.as_ref()) {
                children.push(label("span", "status", message));
            }
            let mut rows = vec![];
            if let Some(config) = config {
                for m in &config.entries {
                    let mut remove = titled(
                        "X",
                        tr("删除组合键"),
                        json!({"action":"remote_macro_remove","id":id,"slot":m.slot,"expected":config.slot_bytes(m.slot).unwrap_or_default()}),
                        !writable,
                    );
                    remove.props["confirm"] = json!(lformat!("删除服务器组合键“{}”？", m.name));
                    let mut row = group(
                        "div",
                        "macro-row",
                        vec![
                            group("div", "macro-content", vec![label("strong", "", &m.name)]),
                            group(
                                "div",
                                "macro-actions",
                                vec![
                                    button(
                                        "button secondary",
                                        tr("发送"),
                                        "",
                                        json!({"action":"remote_macro_run","id":id,"slot":m.slot}),
                                        !writable || !m.supported,
                                    ),
                                    button(
                                        "button secondary",
                                        tr("编辑"),
                                        "",
                                        json!({"action":"remote_macro_edit","id":id,"slot":m.slot}),
                                        !writable || !m.supported,
                                    ),
                                    remove,
                                ],
                            ),
                        ],
                    );
                    row.props["key"] = json!(m.slot);
                    rows.push(row);
                }
            }
            children.push(group("div", "macro-list", rows));
            (
                format!("remote-macros-{id}"),
                "modal",
                json!({"action":"close_dialog"}),
                json!({}),
                children,
            )
        }
        Dialog::RemoteMacroEdit(id, slot, ref expected) => {
            let entry = sessions
                .iter()
                .find(|s| s.server_id == id)
                .and_then(|s| s.remote_macros.as_ref())
                .and_then(|c| c.entries.iter().find(|m| m.slot == slot));
            let choices = amikvm_core::input::macros::remote_catalogue();
            let mut values = json!({});
            let mut fields = vec![];
            for i in 0..amikvm_core::input::macros::MAX_KEYS {
                values[format!("key{i}")] = json!(
                    entry
                        .and_then(|m| m.codes.get(i))
                        .map_or("", String::as_str)
                );
                let mut field = field(
                    &format!("key{i}"),
                    &lformat!("按键 {}", i + 1),
                    "select",
                    json!({}),
                );
                let mut options = vec![json!({"value":"","label":tr("未设置")})];
                options.extend(
                    choices
                        .iter()
                        .map(|(code, name)| json!({"value":code,"label":name})),
                );
                field.props["options"] = json!(options);
                fields.push(field);
            }
            (
                format!("remote-macro-edit-{id}-{slot}"),
                "modal",
                json!({"action":"remote_macro_save","id":id,"slot":slot,"expected":expected}),
                values,
                vec![
                    heading(
                        if entry.is_some() {
                            tr("编辑服务器组合键")
                        } else {
                            tr("添加服务器组合键")
                        },
                        "Keyboard",
                    ),
                    group("div", "form-grid", fields),
                    actions(tr("保存组合键")),
                ],
            )
        }
        Dialog::MacroEdit(id, macro_id) => {
            let m = macro_id.and_then(|id| ui.macros.iter().find(|m| m.id == id));
            let mut values = json!({"name":m.map_or("",|m|m.name.as_str())});
            let choices = amikvm_core::input::macros::catalogue();
            let mut fields = vec![field(
                "name",
                tr("名称"),
                "text",
                json!({"required":true,"maxLength":80,"autoFocus":true,"className":"span-2","placeholder":tr("例如：打开任务管理器")}),
            )];
            for i in 0..amikvm_core::input::macros::MAX_KEYS {
                values[format!("key{i}")] = json!(
                    m.and_then(|m| m.codes.get(i))
                        .map(String::as_str)
                        .unwrap_or("")
                );
                let mut f = field(
                    &format!("key{i}"),
                    &lformat!("按键 {}", i + 1),
                    "select",
                    json!({}),
                );
                let mut options = vec![json!({"value":"","label":tr("未设置")})];
                options.extend(
                    choices
                        .iter()
                        .map(|(code, name)| json!({"value":code,"label":name})),
                );
                f.props["options"] = json!(options);
                fields.push(f);
            }
            (
                format!("macro-edit-{macro_id:?}"),
                "modal",
                json!({"action":"macro_save","id":id,"macro_id":macro_id}),
                values,
                vec![
                    heading(
                        if m.is_some() {
                            tr("编辑组合键")
                        } else {
                            tr("添加组合键")
                        },
                        "Keyboard",
                    ),
                    group("div", "form-grid", fields),
                    actions(tr("保存组合键")),
                ],
            )
        }
        Dialog::Server(id) => {
            let s = id.and_then(|id| servers.iter().find(|s| s.id == id));
            let values = s.map_or_else(||json!({"name":"","host":"","webPort":443,"username":"admin","password":"","scheme":"https","apiMode":"auto","notes":"","tags":"","favorite":false,"rememberPassword":false,"trustInvalidCertificate":false}),|s|json!({"name":s.name,"host":s.host,"webPort":s.web_port,"username":s.username,"password":"","scheme":if s.https {"https"} else {"http"},"apiMode":s.api_mode,"notes":s.notes,"tags":s.tags.join(", "),"favorite":s.favorite,"rememberPassword":s.credential_saved,"trustInvalidCertificate":s.trust_invalid_certificate}));
            let fields = vec![
                field(
                    "name",
                    tr("服务器名称"),
                    "text",
                    json!({"className":"span-2","required":true,"maxLength":160,"autoFocus":true,"placeholder":tr("例如：机房 · 存储服务器")}),
                ),
                field(
                    "host",
                    tr("主机地址"),
                    "text",
                    json!({"required":true,"placeholder":tr("192.168.1.100 或 bmc.example.com")}),
                ),
                field(
                    "webPort",
                    tr("Web 端口"),
                    "number",
                    json!({"required":true,"min":1,"max":65535}),
                ),
                field(
                    "username",
                    tr("用户名"),
                    "text",
                    json!({"required":true,"maxLength":128,"autoComplete":"username"}),
                ),
                field(
                    "password",
                    tr("密码"),
                    "password",
                    json!({"autoComplete":"new-password","placeholder":if s.is_some_and(|s|s.credential_saved) {tr("留空保留已保存密码")} else {tr("连接时也可以输入")}}),
                ),
                select(
                    "scheme",
                    tr("通信协议"),
                    &[("https", "HTTPS"), ("http", "HTTP")],
                    json!({}),
                ),
                select(
                    "apiMode",
                    tr("BMC 接口"),
                    &[
                        ("auto", tr("自动识别")),
                        ("rest", "REST"),
                        ("rpc", "Legacy RPC"),
                    ],
                    json!({}),
                ),
                field(
                    "tags",
                    tr("标签（逗号分隔）"),
                    "text",
                    json!({"className":"span-2"}),
                ),
                field(
                    "notes",
                    tr("备注"),
                    "textarea",
                    json!({"className":"span-2","rows":2,"placeholder":tr("位置、用途或连接说明")}),
                ),
                field(
                    "rememberPassword",
                    tr("将密码保存到系统凭据库"),
                    "checkbox",
                    json!({"className":"checkbox span-2"}),
                ),
                field(
                    "trustInvalidCertificate",
                    tr("信任此服务器的自签名或无效证书"),
                    "checkbox",
                    json!({"className":"checkbox span-2"}),
                ),
            ];
            (
                format!("server-{}", id.map_or("new".into(), |id| id.to_string())),
                "modal",
                json!({"action":"save"}),
                values,
                vec![
                    heading(
                        if s.is_some() {
                            tr("编辑服务器")
                        } else {
                            tr("添加服务器")
                        },
                        "Server",
                    ),
                    group("div", "form-grid", fields),
                    actions(tr("保存服务器")),
                ],
            )
        }
        Dialog::Password(id, capture) => {
            let s = servers.iter().find(|s| s.id == id)?;
            (
                format!("password-{id}"),
                "modal small",
                json!({"action":"password","id":id,"capture":capture}),
                json!({"password":""}),
                vec![
                    heading(&lformat!("连接 {}", s.name), "KeyRound"),
                    field(
                        "password",
                        tr("连接密码"),
                        "password",
                        json!({"required":true,"autoFocus":true,"autoComplete":"current-password"}),
                    ),
                    actions(if capture.is_some() {
                        tr("登录并抓取画面")
                    } else {
                        tr("连接控制台")
                    }),
                ],
            )
        }
        Dialog::Folder(id) => {
            let snapshot = sessions.iter().find(|s| s.server_id == id)?;
            let config = snapshot.config.as_ref()?;
            let slots: Vec<_> = (0..config.hd_instances)
                .filter(|slot| {
                    !snapshot.media.iter().any(|m| {
                        m.kind != amikvm_core::media::scsi::Kind::Cdrom
                            && m.slot == *slot
                            && m.active()
                    }) && !ui.folders.iter().any(|f| {
                        f.server_id == id
                            && f.slot == *slot
                            && matches!(f.phase, "creating" | "connected")
                    })
                })
                .collect();
            let mut selector = field(
                "slot",
                tr("硬盘介质实例"),
                "select",
                json!({"required":true}),
            );
            selector.props["options"] = json!(slots.iter().map(|slot| json!({"value":slot.to_string(),"label":lformat!("实例 {}", slot + 1)})).collect::<Vec<_>>());
            (
                format!("folder-{id}"),
                "modal small",
                json!({"action":"folder_start","id":id}),
                json!({"slot":slots.first().map(|s|s.to_string()).unwrap_or_default(),"size":256,"readonly":true}),
                vec![
                    heading(tr("重定向文件夹"), "FolderOpen"),
                    selector,
                    field(
                        "size",
                        tr("工作镜像容量（MiB）"),
                        "number",
                        json!({"min":16,"max":2048,"required":true}),
                    ),
                    field(
                        "readonly",
                        tr("以只读方式连接"),
                        "checkbox",
                        json!({"className":"checkbox"}),
                    ),
                    actions(tr("选择文件夹和工作镜像")),
                ],
            )
        }
        Dialog::FolderSync(id) => {
            let folder = ui.folders.iter().find(|f| f.id == id)?;
            let conflicts = !folder.conflicts.is_empty();
            let mut children = vec![
                heading(tr("同步文件夹修改"), "FolderSync"),
                label("p", "record-path", &folder.root),
                label(
                    "p",
                    "input-help",
                    lformat!(
                        "远程变化 {} 项 · 本地冲突 {} 项",
                        folder.changes.len(),
                        folder.conflicts.len()
                    ),
                ),
            ];
            let changes = folder
                .changes
                .iter()
                .take(100)
                .map(|c| label("p", "mono", format!("{}  {}", c.operation, c.path)))
                .collect();
            children.push(group("div", "folder-changes", changes));
            if conflicts {
                children.push(label(
                    "p",
                    "media-error",
                    tr("以下文件在本地也有修改，覆盖会替换这些本地修改。保留工作镜像可以稍后处理。"),
                ));
                children.push(group(
                    "div",
                    "folder-changes",
                    folder
                        .conflicts
                        .iter()
                        .take(30)
                        .map(|p| label("p", "media-error mono", p))
                        .collect(),
                ));
            }
            let mut apply = button(
                "button primary",
                if conflicts {
                    tr("覆盖冲突并同步")
                } else {
                    tr("同步修改")
                },
                "",
                json!({"action":"folder_apply","id":id,"overwrite":conflicts}),
                folder.phase != "preview",
            );
            if conflicts {
                apply.props["confirm"] = json!(lformat!(
                    "覆盖预览中的 {} 项本地冲突，并同步远程修改？",
                    folder.conflicts.len()
                ));
            }
            children.push(group(
                "div",
                "modal-actions",
                vec![
                    button(
                        "button secondary",
                        tr("保留工作镜像"),
                        "",
                        json!({"action":"close_dialog"}),
                        false,
                    ),
                    apply,
                ],
            ));
            (
                format!("folder-sync-{id}"),
                "modal",
                json!({"action":"close_dialog"}),
                json!({}),
                children,
            )
        }
        Dialog::Media(id, kind) | Dialog::PhysicalMedia(id, kind) => {
            use amikvm_core::media::scsi::Kind;
            let physical = matches!(ui.dialog, Dialog::PhysicalMedia(..));
            let snapshot = sessions.iter().find(|s| s.server_id == id)?;
            let config = snapshot.config.as_ref()?;
            let cd = kind == Kind::Cdrom;
            let count = if cd {
                config.cd_instances
            } else {
                config.hd_instances
            };
            let slots: Vec<_> = (0..count)
                .filter(|slot| {
                    !snapshot
                        .media
                        .iter()
                        .any(|m| (m.kind == Kind::Cdrom) == cd && m.slot == *slot && m.active())
                        && (cd
                            || !ui.folders.iter().any(|f| {
                                f.server_id == id && f.slot == *slot && f.phase == "creating"
                            }))
                })
                .collect();
            let choices = slots
                .iter()
                .map(|slot| json!({"value":slot.to_string(),"label":lformat!("实例 {}",slot+1)}))
                .collect::<Vec<_>>();
            let mut selector = field("slot", tr("介质实例"), "select", json!({}));
            selector.props["options"] = json!(choices);
            let mut fields = vec![
                heading(
                    if physical {
                        match kind {
                            Kind::Cdrom => tr("重定向实体 CD/DVD"),
                            Kind::HardDisk => tr("重定向实体硬盘 / USB"),
                            Kind::Floppy => tr("重定向实体软盘"),
                        }
                    } else {
                        match kind {
                            Kind::Cdrom => tr("重定向 CD/DVD 镜像"),
                            Kind::HardDisk => tr("重定向硬盘 / USB 镜像"),
                            Kind::Floppy => tr("重定向软盘镜像"),
                        }
                    },
                    "HardDrive",
                ),
                selector,
            ];
            if physical {
                let entries: Vec<_> = ui
                    .devices
                    .entries
                    .iter()
                    .filter(|d| d.kind == kind)
                    .collect();
                let mut choice = field(
                    "device",
                    tr("实体设备"),
                    "select",
                    json!({"required":true,"disabled":ui.devices.loading}),
                );
                let mut options = vec![json!({"value":"","label":tr("请选择实体设备")})];
                options.extend(entries.iter().map(|d|json!({"value":super::devices::choice(d),"label":format!("{} · {} · {}{}",d.label,d.path.display(),byte_size(d.capacity),if d.readonly {format!(" · {}",tr("只读"))}else{String::new()})})));
                choice.props["options"] = json!(options);
                fields.push(choice);
                fields.push(button(
                    "button secondary",
                    if ui.devices.loading {
                        tr("正在读取设备列表")
                    } else {
                        tr("刷新设备列表")
                    },
                    "RefreshCw",
                    json!({"action":"physical_media_refresh"}),
                    ui.devices.loading,
                ));
                if let Some(error) = &ui.devices.error {
                    fields.push(label("p", "media-error", translated(error)));
                }
            }
            if !cd {
                fields.push(field(
                    "readonly",
                    tr("以只读方式连接"),
                    "checkbox",
                    json!({"className":"checkbox"}),
                ));
                if kind == Kind::HardDisk {
                    fields.push(field(
                        "usb",
                        tr("向 BMC 报告为 USB 存储"),
                        "checkbox",
                        json!({"className":"checkbox"}),
                    ));
                }
            } else {
                fields.push(field(
                    "boost",
                    tr("请求 BMC 介质加速模式"),
                    "checkbox",
                    json!({"className":"checkbox"}),
                ));
            }
            let mut footer = actions(if physical {
                tr("连接实体设备")
            } else {
                tr("选择镜像并连接")
            });
            if physical {
                footer.children[1].props["disabled"] = json!(
                    ui.devices.loading
                        || !ui.devices.entries.iter().any(|d| d.kind == kind)
                        || slots.is_empty()
                        || snapshot.phase != "connected"
                );
            }
            fields.push(footer);
            fields.push(button("button secondary wide",if physical{tr("改用镜像文件")}else{tr("选择实体设备")},"HardDrive",json!({"action":if physical{"media_dialog"}else{"physical_media_dialog"},"id":id,"kind":kind}),false));
            (
                format!("media-{id}-{kind:?}-{physical}"),
                if physical { "modal" } else { "modal small" },
                json!({"action":if physical{"physical_media_start"}else{"media_start"},"id":id,"kind":kind}),
                json!({"device":"","slot":slots.first().map(|v|v.to_string()).unwrap_or_default(),"readonly":true,"usb":true,"boost":false}),
                fields,
            )
        }
        Dialog::Text(id) => (
            format!("text-{id}"),
            "modal",
            json!({"action":"text","id":id}),
            json!({"mode":sessions.iter().find(|s| s.server_id == id).map_or("us", |s| s.keyboard_options.text_mode.id()),"text":""}),
            vec![
                heading(tr("输入文本"), "Keyboard"),
                select(
                    "mode",
                    tr("远程系统"),
                    &[
                        ("linux", "Linux · Ctrl + Shift + U"),
                        ("windows", tr("Windows · Unicode 十六进制输入")),
                        ("windows_word", tr("Word · Unicode 转换（Alt + X）")),
                        ("macos", "macOS · Unicode Hex Input"),
                        ("us", tr("直接输入 · US 键盘")),
                    ],
                    json!({}),
                ),
                field(
                    "text",
                    tr("文本"),
                    "textarea",
                    json!({"required":true,"autoFocus":true,"rows":6}),
                ),
                actions(tr("发送文本")),
            ],
        ),
    };
    let form = node(
        "form",
        json!({"key":key,"action":action,"values":values}),
        children,
    );
    Some(node(
        "dialog",
        json!({"key":key,"className":class,"action":{"action":"close_dialog"}}),
        vec![form],
    ))
}

fn console(ui: &UiState, s: &Server, snapshot: Option<&Snapshot>) -> Node {
    let connected = snapshot.is_some_and(|v| v.video_connected);
    let controllable = connected && snapshot.is_some_and(|v| v.can_control);
    let keyboard_enabled =
        controllable && snapshot.is_none_or(|v| !v.mouse.active() && !v.text_input.active());
    let paused = ui.paused.contains(&s.id);
    let zoom = ui.zoom.get(&s.id).copied().unwrap_or_default();
    let mut frame_style = if matches!(zoom, super::Zoom::Fit) {
        json!({"maxWidth":"100%","maxHeight":"100%"})
    } else {
        let percent = zoom.percent() as u64;
        json!({
            "width":snapshot.map_or(0, |v| (v.video_width as u64 * percent + 50) / 100),
            "height":snapshot.map_or(0, |v| (v.video_height as u64 * percent + 50) / 100),
            "maxWidth":"none","maxHeight":"none"
        })
    };
    frame_style["cursor"] = json!(if ui.hidden_local_cursor.contains(&s.id)
        || snapshot.is_some_and(|s| s.mouse_mode == Some(1) && !s.mouse.active())
    {
        "none"
    } else {
        "default"
    });
    let control = |value: Value| json!({"action":"control","id":s.id,"control":value});
    let mut toolbar = vec![
        titled(
            "Info",
            tr("连接信息"),
            json!({"action":"connection_info","id":s.id}),
            false,
        ),
        titled(
            "Keyboard",
            if ui.soft_keyboard.contains(&s.id) {
                tr("关闭软键盘")
            } else {
                tr("打开软键盘")
            },
            json!({"action":"soft_keyboard","id":s.id}),
            false,
        ),
        titled(
            "RefreshCw",
            tr("刷新画面"),
            control(json!({"action":"refresh"})),
            !connected,
        ),
        titled(
            if paused { "Play" } else { "Pause" },
            if paused {
                tr("恢复画面")
            } else {
                tr("暂停画面")
            },
            json!({"action":"pause","id":s.id}),
            !connected,
        ),
        titled(
            "Maximize",
            tr("全屏"),
            json!({"action":"fullscreen"}),
            false,
        ),
        titled(
            "Camera",
            tr("保存 JPEG 截图"),
            json!({"action":"capture","id":s.id}),
            !snapshot.is_some_and(|v| v.video_signal),
        ),
        group("span", "toolbar-divider", vec![]),
    ];
    let requests = snapshot.map_or(0, |s| s.sharing.requests.len());
    toolbar.push(button(
        if requests == 0 {
            "button secondary"
        } else {
            "button primary"
        },
        &if requests == 0 {
            tr("共享会话").to_owned()
        } else {
            lformat!("{requests} 个权限申请", requests = requests)
        },
        "Users",
        json!({"action":"sharing_dialog","id":s.id}),
        !connected,
    ));
    if snapshot.is_some_and(active) {
        if snapshot.is_some_and(|s| s.web_only) {
            toolbar.push(titled(
                "Plug",
                tr("连接控制台"),
                json!({"action":"connect","id":s.id}),
                false,
            ));
        }
        toolbar.push(titled(
            "Unplug",
            tr("断开连接"),
            json!({"action":"disconnect","id":s.id}),
            false,
        ));
    } else {
        toolbar.push(titled(
            "Plug",
            tr("重新连接"),
            json!({"action":"connect","id":s.id}),
            false,
        ));
    }
    let title = match snapshot.map(|v| v.phase.as_str()) {
        Some("authenticating") => tr("正在登录服务器"),
        Some("negotiating") => tr("正在建立控制台连接"),
        Some("reconnecting") => tr("正在自动重连"),
        Some("connected") if snapshot.is_some_and(|s| s.web_only) => tr("Web 会话已连接"),
        Some("connected") if !connected => tr("虚拟介质会话"),
        Some("connected") => tr("等待远程画面"),
        Some("error") => tr("连接失败"),
        _ => tr("控制台未连接"),
    };
    let config = snapshot.and_then(|v| v.config.as_ref());
    let mut main = group(
        "div",
        "console-main",
        vec![
            group(
                "div",
                "console-toolbar",
                vec![
                    group(
                        "div",
                        "toolbar-title",
                        vec![
                            icon("Monitor", 17),
                            label("strong", "", &s.name),
                            dot(status(snapshot)),
                        ],
                    ),
                    group("div", "toolbar-actions", toolbar),
                ],
            ),
            group(
                "div",
                "video-surface",
                vec![
                    node(
                        "video",
                        json!({"serverId":s.id,"enabled":controllable && !paused && snapshot.is_some_and(|v|v.video_signal) && matches!(ui.dialog,Dialog::None),"keyboardEvents":connected && matches!(ui.dialog,Dialog::None),"streaming":connected,"visible":connected && snapshot.is_some_and(|v|v.video_signal),"cursorFocus":snapshot.and_then(|v|v.local_cursor.focus),"captureToken":if matches!(ui.dialog,Dialog::None) && !paused { snapshot.and_then(|v|v.mouse_capture.token) } else {None},"style":frame_style}),
                        vec![],
                    ),
                    group(
                        "div",
                        if connected && snapshot.is_some_and(|v| v.video_signal) {
                            "video-state hidden"
                        } else {
                            "video-state"
                        },
                        vec![
                            group("div", "video-state-icon", vec![icon("Monitor", 34)]),
                            label("h3", "", title),
                            label(
                                "p",
                                "",
                                snapshot
                                    .filter(|s| s.phase == "reconnecting")
                                    .map(recovery_message)
                                    .or_else(|| {
                                        snapshot
                                            .and_then(|s| s.message.as_ref())
                                            .map(|message| translated(message).into_owned())
                                    })
                                    .unwrap_or_else(|| format!("{}:{}", s.host, s.web_port)),
                            ),
                        ],
                    ),
                ],
            ),
            group(
                "div",
                "console-status",
                vec![
                    group(
                        "span",
                        "",
                        vec![dot(status(snapshot)), label("span", "", phase(snapshot))],
                    ),
                    label(
                        "span",
                        "",
                        format!(
                            "{} · {}",
                            if config.is_some_and(|c| c.kvm_secure) {
                                "TLS"
                            } else {
                                "TCP"
                            },
                            if config.is_some_and(|c| c.single_port) {
                                tr("单端口")
                            } else {
                                tr("独立端口")
                            }
                        ),
                    ),
                    label(
                        "span",
                        "",
                        snapshot
                            .filter(|v| v.video_signal)
                            .map_or_else(String::new, |v| {
                                if v.video_source_width != v.video_width
                                    || v.video_source_height != v.video_height
                                {
                                    format!(
                                        "{} × {} → {} × {}",
                                        v.video_source_width,
                                        v.video_source_height,
                                        v.video_width,
                                        v.video_height
                                    )
                                } else {
                                    format!("{} × {}", v.video_width, v.video_height)
                                }
                            }),
                    ),
                    label(
                        "span",
                        "push-right",
                        if controllable {
                            tr("控制权限")
                        } else {
                            tr("仅查看")
                        },
                    ),
                ],
            ),
        ],
    );
    if ui.soft_keyboard.contains(&s.id) {
        let index = main.children.len().saturating_sub(1);
        main.children
            .insert(index, soft_keyboard(ui, s, snapshot, keyboard_enabled));
    }
    let mut shortcuts = vec![];
    for (name, title) in [
        ("alt_f2", "Alt + F2"),
        ("ctrl_h", "Ctrl + H"),
        ("win", "Win"),
        ("menu", tr("菜单键")),
        ("ctrl_alt_backspace", "Ctrl + Alt + ⌫"),
        ("alt_tab", "Alt + Tab"),
        ("ctrl_escape", "Ctrl + Esc"),
    ] {
        shortcuts.push(button(
            "",
            title,
            "",
            json!({"action":"shortcut","id":s.id,"name":name}),
            !keyboard_enabled,
        ));
    }
    let mouse = select(
        "mouse",
        tr("鼠标模式"),
        &[
            ("2", tr("绝对定位")),
            ("1", tr("相对移动")),
            ("3", tr("其他模式")),
        ],
        json!({"value":snapshot.and_then(|v|v.mouse_mode).unwrap_or(2).to_string(),"action":control(json!({"action":"mouse_mode","mode":2})),"disabled":!controllable}),
    );
    let mut keyboard_choices = vec![("AD", tr("自动识别"))];
    keyboard_choices.extend(
        amikvm_core::input::layout::ALL
            .into_iter()
            .filter(|l| l.physical())
            .map(|l| (l.id(), l.label())),
    );
    let keyboard = select(
        "layout",
        tr("键盘布局"),
        &keyboard_choices,
        json!({"value":config.map_or("AD",|c|c.keyboard_layout.as_str()),"action":control(json!({"action":"keyboard_layout","layout":"AD"})),"disabled":!keyboard_enabled}),
    );
    let mut powers = vec![];
    let power_allowed = controllable && config.is_some_and(|c| c.privileges & 256 != 0);
    let power = snapshot.map(|s| &s.power);
    use amikvm_core::protocol::PowerOperation;
    for (operation, title) in [
        (PowerOperation::On, tr("开机")),
        (PowerOperation::Shutdown, tr("正常关机")),
        (PowerOperation::Reset, tr("重启")),
        (PowerOperation::Cycle, tr("电源循环")),
        (PowerOperation::Off, tr("强制关机")),
    ] {
        let mut b = button(
            "",
            title,
            "",
            control(json!({"action":"power","operation":operation})),
            !power_allowed || !power.is_some_and(|p| p.available(operation)),
        );
        b.props["confirm"] = json!(lformat!("对 {} 执行“{title}”？", s.name, title = title));
        powers.push(b);
    }
    let mut power_children = vec![
        group(
            "div",
            "section-label",
            vec![
                icon("Power", 15),
                label("span", "", tr("服务器电源")),
                label(
                    "span",
                    "push-right power-state",
                    match power.and_then(|p| p.status) {
                        Some(1) => tr("已开机"),
                        Some(0) => tr("已关机"),
                        _ => tr("未知"),
                    },
                ),
            ],
        ),
        group("div", "power-grid", powers),
        button(
            "button secondary wide",
            if power.is_some_and(|p| p.query_pending) {
                tr("正在查询电源状态")
            } else {
                tr("刷新电源状态")
            },
            "RefreshCw",
            control(json!({"action":"power_status"})),
            !connected || power.is_some_and(|p| p.query_pending),
        ),
    ];
    if let Some(power) = power {
        if power.phase != amikvm_core::power::Phase::Idle {
            power_children.push(label("p", "help", translated(power.phase.label())));
        }
        if power.phase == amikvm_core::power::Phase::Rejected {
            if let Some(code) = power.response_code {
                power_children.push(label("p", "help", lformat!("回执状态码：{}", code)));
            }
        }
        if power.waiting_for_off {
            power_children.push(label(
                "p",
                "help",
                tr("等待服务器关机，正在重新查询电源状态"),
            ));
        }
        if power.query_failed {
            power_children.push(label("p", "help", tr("无法查询电源状态，可以重新刷新")));
        }
    }
    let mut keyboard_children = vec![
        group(
            "div",
            "section-label",
            vec![icon("Keyboard", 15), label("span", "", tr("键盘与鼠标"))],
        ),
        button(
            "button secondary wide",
            tr("发送 Ctrl + Alt + Del"),
            "",
            json!({"action":"shortcut","id":s.id,"name":"ctrl_alt_del"}),
            !keyboard_enabled,
        ),
        group("div", "shortcut-grid", shortcuts),
        button(
            "button secondary wide",
            tr("用户组合键"),
            "Keyboard",
            json!({"action":"macro_dialog","id":s.id}),
            false,
        ),
        button(
            "button secondary wide",
            if ui.soft_keyboard.contains(&s.id) {
                tr("关闭软键盘")
            } else {
                tr("打开软键盘")
            },
            "Keyboard",
            json!({"action":"soft_keyboard","id":s.id}),
            false,
        ),
        button(
            "button secondary wide",
            tr("输入 Unicode 文本"),
            "",
            json!({"action":"text_dialog","id":s.id}),
            !keyboard_enabled,
        ),
        keyboard_options(s, snapshot, keyboard_enabled),
        button(
            "button secondary wide",
            if snapshot.is_some_and(|s| s.input_encryption) {
                tr("关闭键鼠加密")
            } else {
                tr("开启键鼠加密")
            },
            "ShieldCheck",
            control(
                json!({"action":"input_encryption","enabled":!snapshot.is_some_and(|s|s.input_encryption)}),
            ),
            !controllable,
        ),
        label(
            "small",
            "keyboard-detection",
            if snapshot.is_some_and(|s| s.encryption_required) {
                tr("BMC 已要求启用键盘和鼠标加密")
            } else if snapshot.is_some_and(|s| s.input_encryption) {
                tr("键盘和鼠标输入已加密")
            } else {
                tr("键盘和鼠标输入未加密")
            },
        ),
        mouse,
        mouse_calibration(s, snapshot, controllable),
        mouse_capture(s, snapshot, controllable && !paused),
        button(
            "button secondary wide",
            if ui.hidden_local_cursor.contains(&s.id) {
                tr("显示本地指针")
            } else {
                tr("隐藏本地指针")
            },
            "",
            json!({"action":"local_cursor","id":s.id}),
            false,
        ),
        keyboard,
    ];
    {
        use crate::keyboard::locks::Phase;
        let locks = &ui.host_locks;
        let active = locks.server == Some(s.id);
        let message = match if active { locks.phase } else { Phase::Inactive } {
            Phase::Inactive => tr("锁定键同步：聚焦远程画面后启用"),
            Phase::Waiting => tr("锁定键同步：等待 BMC 状态"),
            Phase::Pending => tr("锁定键同步：等待状态确认"),
            Phase::Synchronized if locks.reverse => tr("锁定键同步：远端跟随本机"),
            Phase::Synchronized => tr("锁定键同步：本机跟随远端"),
            Phase::Unavailable => tr("系统不支持修改本机锁定键，请使用软键盘"),
            Phase::Failed => tr("锁定键同步失败"),
        };
        keyboard_children.push(label("small", "keyboard-lock-sync", message));
        if active && locks.mask != 0 && locks.mask != 7 {
            keyboard_children.push(label(
                "small",
                "keyboard-lock-partial",
                tr("系统仅支持部分锁定键状态同步"),
            ));
        }
        if let Some(error) = &locks.error {
            if active || locks.phase == Phase::Failed {
                keyboard_children.push(label(
                    "small",
                    "keyboard-lock-error",
                    &crate::locale::message(error),
                ));
            }
        }
    }
    if config.is_none_or(|c| c.keyboard_layout == "AD") {
        keyboard_children.push(label(
            "small",
            "keyboard-detection",
            &match ui.host_keyboard.layout {
                Some(layout) => lformat!("本机布局：{}", layout.label()),
                None if !ui.host_keyboard.ambiguous.is_empty() => lformat!(
                    "当前字符符合 {}，请手动选择具体布局。",
                    ui.host_keyboard
                        .ambiguous
                        .iter()
                        .map(|layout| layout.id())
                        .collect::<Vec<_>>()
                        .join(" / ")
                ),
                None => ui
                    .host_keyboard
                    .notice
                    .as_deref()
                    .map(|notice| crate::locale::message(notice).into_owned())
                    .unwrap_or_else(|| tr("正在识别本机键盘布局…").into()),
            },
        ));
    }
    if connected && !controllable {
        keyboard_children.push(button(
            "button secondary wide",
            tr("申请控制权限"),
            "",
            control(json!({"action":"request_control"})),
            !snapshot.is_some_and(|s| s.sharing.can_request()),
        ));
    }
    let panel = if snapshot.is_some_and(|s| s.web_only) {
        group(
            "aside",
            "control-panel",
            vec![
                capture_options(s, snapshot),
                group(
                    "div",
                    "panel-section",
                    vec![
                        button(
                            "button secondary wide",
                            tr("连接控制台"),
                            "Plug",
                            json!({"action":"connect","id":s.id}),
                            false,
                        ),
                        button(
                            "button secondary wide",
                            tr("查看 BMC 录像"),
                            "Video",
                            json!({"action":"recordings_dialog","id":s.id}),
                            false,
                        ),
                    ],
                ),
            ],
        )
    } else {
        group(
            "aside",
            "control-panel",
            vec![
                group("div", "panel-section", keyboard_children),
                video_options(s, snapshot, zoom),
                capture_options(s, snapshot),
                sharing_summary(s, snapshot),
                media(s, snapshot),
                folders(ui, s, snapshot),
                recording(ui, s, snapshot),
                group("div", "panel-section", power_children),
                group(
                    "div",
                    "panel-section",
                    vec![
                        group(
                            "div",
                            "section-label",
                            vec![icon("LockKeyhole", 15), label("span", "", tr("主机显示"))],
                        ),
                        button(
                            "button secondary wide",
                            match snapshot.and_then(|v| v.host_display) {
                                Some(0) => tr("锁定主机显示"),
                                Some(1) => tr("解锁主机显示"),
                                Some(2) => tr("主机显示已解锁（控制已禁用）"),
                                Some(3) => tr("主机显示已锁定（控制已禁用）"),
                                _ => tr("等待主机显示状态"),
                            },
                            "",
                            control(
                                json!({"action":"host_display","locked":snapshot.and_then(|v|v.host_display)!=Some(1)}),
                            ),
                            !controllable
                                || !snapshot.is_some_and(|s| {
                                    amikvm_core::video::config::host_display_available(
                                        s.host_display,
                                        s.host_display_supported,
                                    )
                                }),
                        ),
                    ],
                ),
                group(
                    "div",
                    "panel-section",
                    vec![
                        label("div", "section-label", tr("服务器启动与 IPMI")),
                        button(
                            "button secondary wide",
                            tr("启动选项"),
                            "Power",
                            json!({"action":"boot_dialog","id":s.id}),
                            !controllable,
                        ),
                        button(
                            "button secondary wide",
                            tr("IPMI 命令与响应"),
                            "TerminalSquare",
                            json!({"action":"ipmi_dialog","id":s.id}),
                            snapshot.is_none(),
                        ),
                    ],
                ),
            ],
        )
    };
    group("div", "console-layout", vec![main, panel])
}

fn mouse_capture(server: &Server, snapshot: Option<&Snapshot>, enabled: bool) -> Node {
    let Some(s) = snapshot.filter(|s| matches!(s.mouse_mode, Some(1 | 3))) else {
        return group("div", "", vec![]);
    };
    let requested = s.mouse_capture.requested();
    let mut children = vec![button(
        "button secondary wide",
        if requested {
            tr("释放鼠标捕获")
        } else {
            tr("开启鼠标捕获")
        },
        "Monitor",
        json!({"action":"mouse_capture","id":server.id,"enabled":!requested}),
        !requested && (!enabled || !crate::pointer_capture::eligible(s) || s.text_input.active()),
    )];
    if let Some(message) = &s.mouse_capture.message {
        children.push(label("p", "input-help", translated(message)));
    }
    group("div", "keyboard-text-options", children)
}

fn keyboard_options(server: &Server, snapshot: Option<&Snapshot>, enabled: bool) -> Node {
    let options = snapshot.map_or(Default::default(), |s| s.keyboard_options);
    let mut children = vec![
        select(
            "keyboard_host",
            tr("远端键盘主机类型"),
            &[("windows", tr("Windows 主机")), ("linux", tr("Linux 主机"))],
            json!({"value":options.host.id(),"disabled":!enabled,"action":{"action":"keyboard_option","id":server.id,"setting":"host"}}),
        ),
        field(
            "full_keyboard",
            tr("全键盘支持"),
            "checkbox",
            json!({"className":"checkbox","value":options.full_keyboard,"disabled":!enabled,"action":{"action":"keyboard_option","id":server.id,"setting":"full_keyboard"}}),
        ),
        field(
            "easy_paste",
            tr("Ctrl + V 输入本机剪贴板文本"),
            "checkbox",
            json!({"className":"checkbox","value":options.easy_paste,"disabled":!enabled,"action":{"action":"keyboard_option","id":server.id,"setting":"easy_paste"}}),
        ),
        select(
            "text_mode",
            tr("文本输入方式"),
            &[
                ("us", tr("直接输入 · US 键盘")),
                ("linux", "Linux · Ctrl + Shift + U"),
                ("windows", tr("Windows · Unicode 十六进制输入")),
                ("windows_word", tr("Word · Unicode 转换（Alt + X）")),
                ("macos", "macOS · Unicode Hex Input"),
            ],
            json!({"value":options.text_mode.id(),"disabled":!enabled,"action":{"action":"keyboard_option","id":server.id,"setting":"text_mode"}}),
        ),
        button(
            "button secondary wide",
            tr("发送本机剪贴板文本"),
            "Keyboard",
            json!({"action":"paste","id":server.id}),
            !enabled,
        ),
    ];
    if let Some(status) = snapshot.map(|s| &s.text_input) {
        use amikvm_core::input::TextPhase;
        let title = match status.phase {
            TextPhase::Idle => None,
            TextPhase::Running => Some(lformat!(
                "正在发送文本：{sent}/{total} 个字符",
                sent = status.sent,
                total = status.total
            )),
            TextPhase::Complete => {
                Some(lformat!("文本已发送：{total} 个字符", total = status.total))
            }
            TextPhase::Cancelled => Some(lformat!(
                "文本输入已停止：{sent}/{total} 个字符",
                sent = status.sent,
                total = status.total
            )),
            TextPhase::Failed => Some(lformat!(
                "文本发送失败：{sent}/{total} 个字符",
                sent = status.sent,
                total = status.total
            )),
        };
        if let Some(title) = title {
            children.push(label("p", "input-help", title));
        }
        if let Some(error) = &status.error {
            children.push(label("p", "error-inline", translated(error)));
        }
        if status.active() {
            children.push(button(
                "button secondary wide",
                tr("停止文本输入"),
                "Square",
                json!({"action":"text_stop","id":server.id}),
                false,
            ));
        }
    }
    group("div", "keyboard-text-options", children)
}

fn mouse_calibration(server: &Server, snapshot: Option<&Snapshot>, controllable: bool) -> Node {
    use amikvm_core::input::mouse::{Stage, State};
    let default = State::default();
    let state = snapshot.map_or(&default, |s| &s.mouse);
    if !state.active() && !snapshot.is_some_and(|s| s.mouse_mode == Some(1)) {
        return group("div", "", vec![]);
    }
    let enabled =
        controllable && snapshot.is_some_and(|s| s.mouse_mode == Some(1) && s.video_signal);
    let action = |command: Value| json!({"action":"mouse","id":server.id,"token":state.token,"command":command});
    let settings = state.candidate.unwrap_or(state.settings);
    let mut children = vec![label("strong", "", tr("鼠标校准与同步"))];
    if let Some(message) = snapshot.and_then(|s| s.local_cursor.message.as_deref()) {
        children.push(label("p", "input-help", translated(message)));
    }
    if !state.active() {
        children.push(node("form", json!({"key":format!("mouse-settings-{}-{}-{}",server.id,settings.threshold,settings.acceleration),"values":{"threshold":settings.threshold.to_string(),"gain":settings.multiplier()},"action":{"action":"mouse_settings","id":server.id}}), vec![
            field("threshold", tr("加速阈值"), "number", json!({"min":1,"max":65535,"required":true,"disabled":!enabled})),
            field("gain", tr("加速倍率"), "number", json!({"min":0.01,"step":0.01,"required":true,"disabled":!enabled})),
            node("button", json!({"className":"button secondary wide","text":tr("应用参数并同步"),"type":"submit","disabled":!enabled}),vec![]),
        ]));
        children.push(button(
            "button secondary wide",
            tr("开始两步校准"),
            "",
            action(json!({"operation":"start"})),
            !enabled,
        ));
    } else {
        children.push(label(
            "p",
            "input-help",
            lformat!(
                "阈值 {} · 倍率 {}{}",
                settings.threshold,
                settings.multiplier(),
                if state.paused {
                    tr(" · 已暂停")
                } else {
                    ""
                }
            ),
        ));
        if matches!(state.stage, Stage::Threshold | Stage::Acceleration) {
            let mut adjust = vec![
                button(
                    "button secondary",
                    "− 1",
                    "",
                    action(json!({"operation":"adjust","direction":-1})),
                    !enabled,
                ),
                button(
                    "button secondary",
                    "+ 1",
                    "",
                    action(json!({"operation":"adjust","direction":1})),
                    !enabled,
                ),
            ];
            if state.stage == Stage::Acceleration {
                adjust.extend([
                    button(
                        "button secondary",
                        "− 0.1",
                        "",
                        action(json!({"operation":"adjust","direction":-1,"fine":true})),
                        !enabled,
                    ),
                    button(
                        "button secondary",
                        "+ 0.1",
                        "",
                        action(json!({"operation":"adjust","direction":1,"fine":true})),
                        !enabled,
                    ),
                ]);
            }
            children.push(group("div", "shortcut-grid", adjust));
            children.push(button(
                "button primary wide",
                if state.stage == Stage::Threshold {
                    tr("记录首次不同步的阈值")
                } else {
                    tr("记录已同步的倍率")
                },
                "",
                action(json!({"operation":"detected"})),
                !enabled,
            ));
            children.push(button(
                "button secondary wide",
                if state.paused {
                    tr("继续校准")
                } else {
                    tr("暂停校准")
                },
                "",
                action(json!({"operation":"pause","paused":!state.paused})),
                !enabled,
            ));
        } else {
            children.push(button(
                "button primary wide",
                if state.stage == Stage::ThresholdReview {
                    tr("保存阈值并校准倍率")
                } else {
                    tr("保存加速倍率")
                },
                "",
                action(json!({"operation":"accept"})),
                !enabled,
            ));
            children.push(button(
                "button secondary wide",
                tr("返回继续调整"),
                "",
                action(json!({"operation":"retry"})),
                !enabled,
            ));
        }
        children.push(button(
            "button secondary wide",
            tr("取消未确认的校准"),
            "",
            action(json!({"operation":"cancel"})),
            !enabled,
        ));
    }
    children.push(button(
        "button secondary wide",
        tr("同步到左上角"),
        "",
        action(json!({"operation":"synchronize"})),
        !enabled,
    ));
    if let Some(message) = &state.message {
        children.push(label("small", "input-help", message));
    }
    group("div", "mouse-calibration", children)
}

fn capture_options(server: &Server, snapshot: Option<&Snapshot>) -> Node {
    let connected = snapshot.is_some_and(|s| s.phase == "connected");
    group(
        "div",
        "panel-section",
        vec![
            group(
                "div",
                "section-label",
                vec![icon("Camera", 15), label("span", "", tr("BMC 捕获画面"))],
            ),
            button(
                "button secondary wide",
                tr("抓取预览画面"),
                "Camera",
                json!({"action":"capture_dialog","id":server.id,"kind":"preview"}),
                !connected,
            ),
            button(
                "button secondary wide",
                tr("查看蓝屏捕获"),
                "Camera",
                json!({"action":"capture_dialog","id":server.id,"kind":"crash"}),
                !connected,
            ),
        ],
    )
}

fn soft_keyboard(
    ui: &UiState,
    server: &Server,
    snapshot: Option<&Snapshot>,
    enabled: bool,
) -> Node {
    use amikvm_core::input::layout::Layout;
    let physical = snapshot.and_then(|s| s.config.as_ref()).and_then(|c| {
        if c.keyboard_layout == "AD" {
            ui.host_keyboard.layout
        } else {
            Layout::parse(&c.keyboard_layout).ok()
        }
    });
    let resolved = ui.soft_layout.get(&server.id).copied().or(physical);
    let layout = resolved.unwrap_or(Layout::Us);
    let held = snapshot
        .map(|s| s.software_keys.as_slice())
        .unwrap_or_default();
    let leds = snapshot.map_or(0, |s| s.lock_leds);
    let shift = held.contains(&0xe1) || held.contains(&0xe5);
    let caps = leds & 2 != 0;
    let alt_gr = held.contains(&0xe6);
    let key = |code: &str, width: u8| {
        if code.is_empty() {
            return group("span", &format!("keyboard-space width-{width}"), vec![]);
        }
        let usage = amikvm_core::input::usage(code).expect("known software key");
        let modifier = (0xe0..=0xe7).contains(&usage);
        let lock = match code {
            "NumLock" => 1,
            "CapsLock" => 2,
            "ScrollLock" => 4,
            _ => 0,
        };
        let pressed = held.contains(&usage);
        let lit = lock != 0 && leds & lock != 0;
        let mut title = amikvm_core::input::macros::key_label(code);
        let caption = layout
            .caption(code, shift, caps, alt_gr)
            .unwrap_or_else(|| match code {
                "ControlLeft" | "ControlRight" => "Ctrl".into(),
                "ShiftLeft" | "ShiftRight" => "Shift".into(),
                "AltLeft" => "Alt".into(),
                "AltRight" => if matches!(layout, Layout::Us | Layout::Ru) || layout.japanese() {
                    "Alt"
                } else {
                    "AltGr"
                }
                .into(),
                "MetaLeft" | "MetaRight" => "Win / ⌘".into(),
                "CapsLock" => "Caps".into(),
                "NumLock" => "Num".into(),
                "Space" => tr("空格").into(),
                "PrintScreen" => "PrtSc".into(),
                "ScrollLock" => "ScrLk".into(),
                "Backspace" => "⌫".into(),
                "PageUp" => "PgUp".into(),
                "PageDown" => "PgDn".into(),
                "ContextMenu" => tr("菜单").into(),
                "NonConvert" => tr("無変換").into(),
                "Convert" => tr("変換").into(),
                "KanaMode" => "かな".into(),
                "NumpadAdd" => "+".into(),
                "NumpadSubtract" => "−".into(),
                "NumpadDivide" => "/".into(),
                "NumpadMultiply" => "*".into(),
                "NumpadEnter" => "Enter".into(),
                "Numpad0" if leds & 1 == 0 => "Ins".into(),
                "Numpad1" if leds & 1 == 0 => "End".into(),
                "Numpad2" if leds & 1 == 0 => "↓".into(),
                "Numpad3" if leds & 1 == 0 => "PgDn".into(),
                "Numpad4" if leds & 1 == 0 => "←".into(),
                "Numpad5" if leds & 1 == 0 => "·".into(),
                "Numpad6" if leds & 1 == 0 => "→".into(),
                "Numpad7" if leds & 1 == 0 => "Home".into(),
                "Numpad8" if leds & 1 == 0 => "↑".into(),
                "Numpad9" if leds & 1 == 0 => "PgUp".into(),
                "NumpadDecimal" => if leds & 1 != 0 {
                    layout.decimal()
                } else {
                    "Del"
                }
                .into(),
                _ if code.starts_with("Numpad") => code.trim_start_matches("Numpad").to_owned(),
                _ => title.clone(),
            });
        if modifier {
            title.push_str(tr(" · 点击保持或释放"));
        }
        let extra = match code {
            "NumpadAdd" | "NumpadEnter" => " keypad-tall",
            "Numpad0" => " keypad-zero",
            _ => "",
        };
        let mut b = button(
            &format!(
                "keyboard-key width-{width}{extra}{}{}",
                if pressed { " pressed" } else { "" },
                if lit { " led-on" } else { "" }
            ),
            &caption,
            "",
            Value::Null,
            !enabled,
        );
        b.props["key"] = json!(code);
        b.props["preserveFocus"] = json!(true);
        b.props["title"] = json!(title);
        if modifier || lock != 0 {
            b.props["pressed"] = json!(pressed || lit);
        }
        if modifier {
            b.props["action"] = json!({"action":"soft_modifier","id":server.id,"code":code});
        } else {
            b.props["inputOnDown"] =
                json!({"id":server.id,"event":{"type":"soft_key","code":code,"pressed":true}});
            b.props["inputOnUp"] =
                json!({"id":server.id,"event":{"type":"soft_key","code":code,"pressed":false}});
        }
        b
    };
    let row = |codes: &[(&str, u8)]| {
        group(
            "div",
            "keyboard-row",
            codes.iter().map(|(c, w)| key(c, *w)).collect(),
        )
    };
    let mut number_row = vec![
        ("Backquote", 2),
        ("Digit1", 2),
        ("Digit2", 2),
        ("Digit3", 2),
        ("Digit4", 2),
        ("Digit5", 2),
        ("Digit6", 2),
        ("Digit7", 2),
        ("Digit8", 2),
        ("Digit9", 2),
        ("Digit0", 2),
        ("Minus", 2),
        ("Equal", 2),
    ];
    let mut upper_row = vec![
        ("Tab", 3),
        ("KeyQ", 2),
        ("KeyW", 2),
        ("KeyE", 2),
        ("KeyR", 2),
        ("KeyT", 2),
        ("KeyY", 2),
        ("KeyU", 2),
        ("KeyI", 2),
        ("KeyO", 2),
        ("KeyP", 2),
        ("BracketLeft", 2),
        ("BracketRight", 2),
    ];
    let mut middle_row = vec![
        ("CapsLock", 4),
        ("KeyA", 2),
        ("KeyS", 2),
        ("KeyD", 2),
        ("KeyF", 2),
        ("KeyG", 2),
        ("KeyH", 2),
        ("KeyJ", 2),
        ("KeyK", 2),
        ("KeyL", 2),
        ("Semicolon", 2),
        ("Quote", 2),
    ];
    let mut lower_row = vec![("ShiftLeft", 4)];
    if layout.japanese() {
        number_row.push(("IntlYen", 2));
        middle_row.push(("Backslash", 2));
    } else {
        upper_row.push(("Backslash", 3));
        lower_row.push(("IntlBackslash", 2));
    }
    number_row.push(("Backspace", if layout.japanese() { 2 } else { 4 }));
    middle_row.push(("Enter", if layout.japanese() { 3 } else { 4 }));
    lower_row.extend([
        ("KeyZ", 2),
        ("KeyX", 2),
        ("KeyC", 2),
        ("KeyV", 2),
        ("KeyB", 2),
        ("KeyN", 2),
        ("KeyM", 2),
        ("Comma", 2),
        ("Period", 2),
        ("Slash", 2),
    ]);
    if layout.japanese() {
        lower_row.push(("IntlRo", 2));
    }
    lower_row.push(("ShiftRight", 4));
    let mut bottom_row = vec![("ControlLeft", 3), ("MetaLeft", 3), ("AltLeft", 3)];
    if layout.japanese() {
        bottom_row.push(("NonConvert", 3));
    }
    bottom_row.push(("Space", if layout.japanese() { 4 } else { 10 }));
    if layout.japanese() {
        bottom_row.extend([("Convert", 3), ("KanaMode", 3)]);
    }
    bottom_row.extend([
        ("AltRight", 3),
        ("MetaRight", 3),
        ("ContextMenu", 2),
        ("ControlRight", 3),
    ]);
    let main = group(
        "div",
        "keyboard-main",
        vec![
            row(&number_row),
            row(&upper_row),
            row(&middle_row),
            row(&lower_row),
            row(&bottom_row),
        ],
    );
    let navigation = group(
        "div",
        "keyboard-navigation",
        vec![
            row(&[("Insert", 2), ("Home", 2), ("PageUp", 2)]),
            row(&[("Delete", 2), ("End", 2), ("PageDown", 2)]),
            row(&[("", 2), ("", 2), ("", 2)]),
            row(&[("", 2), ("ArrowUp", 2), ("", 2)]),
            row(&[("ArrowLeft", 2), ("ArrowDown", 2), ("ArrowRight", 2)]),
        ],
    );
    let numpad = group(
        "div",
        "keyboard-numpad",
        [
            "NumLock",
            "NumpadDivide",
            "NumpadMultiply",
            "NumpadSubtract",
            "Numpad7",
            "Numpad8",
            "Numpad9",
            "NumpadAdd",
            "Numpad4",
            "Numpad5",
            "Numpad6",
            "Numpad1",
            "Numpad2",
            "Numpad3",
            "NumpadEnter",
            "Numpad0",
            "NumpadDecimal",
        ]
        .iter()
        .map(|c| key(c, 2))
        .collect(),
    );
    let mut macros = vec![button(
        "button secondary",
        tr("管理组合键"),
        "",
        json!({"action":"macro_dialog","id":server.id}),
        false,
    )];
    macros.push(button(
        "button secondary",
        "Ctrl + Alt + Del",
        "",
        json!({"action":"shortcut","id":server.id,"name":"ctrl_alt_del"}),
        !enabled,
    ));
    for m in &ui.macros {
        let b = button(
            "button secondary",
            &m.name,
            "",
            json!({"action":"macro_run","id":server.id,"macro_id":m.id}),
            !enabled,
        );
        macros.push(b);
    }
    let mut layout_choices = vec![("follow", tr("跟随键盘布局"))];
    layout_choices.extend(
        amikvm_core::input::layout::ALL
            .into_iter()
            .map(|l| (l.id(), l.label())),
    );
    let layout_picker = select(
        "softLayout",
        tr("软键盘字符"),
        &layout_choices,
        json!({"value":ui.soft_layout.get(&server.id).map_or("follow",|l|l.id()),"action":{"action":"soft_layout","id":server.id}}),
    );
    group(
        "section",
        "soft-keyboard",
        vec![
            group(
                "div",
                "keyboard-heading",
                vec![
                    label("strong", "", tr("软键盘")),
                    button(
                        "button secondary push-right",
                        tr("释放所有按键"),
                        "",
                        json!({"action":"input","id":server.id,"event":{"type":"release_all"}}),
                        !enabled,
                    ),
                    titled(
                        "X",
                        tr("关闭软键盘"),
                        json!({"action":"soft_keyboard","id":server.id}),
                        false,
                    ),
                ],
            ),
            group("div", "keyboard-layout-picker", vec![layout_picker]),
            group(
                "div",
                "keyboard-scroll",
                vec![group(
                    "div",
                    "keyboard-body",
                    vec![
                        row(&[
                            ("Escape", 2),
                            ("F1", 2),
                            ("F2", 2),
                            ("F3", 2),
                            ("F4", 2),
                            ("F5", 2),
                            ("F6", 2),
                            ("F7", 2),
                            ("F8", 2),
                            ("F9", 2),
                            ("F10", 2),
                            ("F11", 2),
                            ("F12", 2),
                            ("PrintScreen", 2),
                            ("ScrollLock", 2),
                            ("Pause", 2),
                        ]),
                        group("div", "keyboard-sections", vec![main, navigation, numpad]),
                    ],
                )],
            ),
            group("div", "keyboard-macros", macros),
        ],
    )
}

fn video_options(server: &Server, snapshot: Option<&Snapshot>, zoom: super::Zoom) -> Node {
    let options = [
        ("fit", tr("适应窗口")),
        ("actual", tr("实际尺寸 · 100%")),
        ("host", tr("窗口适应主机尺寸")),
        ("50", "50%"),
        ("60", "60%"),
        ("70", "70%"),
        ("80", "80%"),
        ("90", "90%"),
        ("100", "100%"),
        ("110", "110%"),
        ("120", "120%"),
        ("130", "130%"),
        ("140", "140%"),
        ("150", "150%"),
    ];
    let mut children = vec![
        group(
            "div",
            "section-label",
            vec![icon("Monitor", 15), label("span", "", tr("画面显示"))],
        ),
        select(
            "zoom",
            tr("画面缩放"),
            &options,
            json!({"value":zoom.value(),"action":{"action":"zoom","id":server.id}}),
        ),
        group(
            "div",
            "shortcut-grid",
            vec![
                button(
                    "",
                    tr("缩小"),
                    "",
                    json!({"action":"zoom_step","id":server.id,"direction":-1}),
                    zoom.percent() <= 50,
                ),
                button(
                    "",
                    tr("放大"),
                    "",
                    json!({"action":"zoom_step","id":server.id,"direction":1}),
                    zoom.percent() >= 150,
                ),
            ],
        ),
        label(
            "small",
            "",
            snapshot
                .filter(|s| s.video_signal)
                .map_or(tr("等待远程画面").into(), |s| {
                    format!("{} × {}", s.video_width, s.video_height)
                }),
        ),
    ];
    let engine = snapshot.and_then(|s| s.video_config);
    let can_configure =
        snapshot.is_some_and(|s| s.video_connected && s.can_control) && engine.is_some();
    children.push(select("compression", tr("视频压缩"), &[
        ("0", "YUV 420"), ("1", "YUV 444"), ("2", tr("YUV 444 · 2 色 VQ")), ("3", tr("YUV 444 · 4 色 VQ")),
    ], json!({"value":engine.map_or(0, |e| e.compression).to_string(),"disabled":!can_configure,"action":{"action":"video_config","id":server.id,"setting":"compression"}})));
    children.push(select("quality", tr("DCT 画质"), &[
        ("0", tr("0 · 最佳画质")), ("1", "1"), ("2", "2"), ("3", "3"), ("4", "4"), ("5", "5"), ("6", "6"), ("7", tr("7 · 最低画质")),
    ], json!({"value":engine.map_or(0, |e| e.quality).to_string(),"disabled":!can_configure,"action":{"action":"video_config","id":server.id,"setting":"quality"}})));
    let connected = snapshot.is_some_and(|s| s.video_connected);
    let measuring = snapshot.is_some_and(|s| s.bandwidth_measuring);
    children.push(select("bandwidth", tr("视频带宽"), &[
        ("32768", "256 Kbps"), ("65536", "512 Kbps"), ("131072", "1 Mbps"), ("1310720", "10 Mbps"), ("13107200", "100 Mbps"),
    ], json!({"value":snapshot.and_then(|s| s.bandwidth).unwrap_or(13_107_200).to_string(),"disabled":!connected || measuring,"action":{"action":"control","id":server.id,"control":{"action":"bandwidth","bytes_per_second":13_107_200}}})));
    children.push(button(
        "button secondary wide",
        if measuring {
            tr("正在测量带宽…")
        } else {
            tr("自动检测带宽")
        },
        "Activity",
        json!({"action":"control","id":server.id,"control":{"action":"detect_bandwidth"}}),
        !connected || measuring,
    ));
    if let Some(rate) = snapshot.and_then(|s| s.measured_bytes_per_second) {
        children.push(label(
            "small",
            "",
            lformat!("实测 {:.1} Mbps", rate as f64 * 8.0 / 1_048_576.0),
        ));
    }
    group("div", "panel-section", children)
}

fn sharing_summary(server: &Server, snapshot: Option<&Snapshot>) -> Node {
    let mut children = vec![group(
        "div",
        "section-label",
        vec![icon("Users", 15), label("span", "", tr("共享会话"))],
    )];
    if let Some(snapshot) = snapshot {
        let sharing = &snapshot.sharing;
        children.push(label(
            "p",
            "input-help",
            sharing
                .message
                .as_deref()
                .map(translated)
                .unwrap_or_else(|| tr(sharing.role.label()).into()),
        ));
        children.push(button(
            "button secondary wide",
            &lformat!("管理共享 · {} 个申请", sharing.requests.len()),
            "Users",
            json!({"action":"sharing_dialog","id":server.id}),
            !snapshot.video_connected,
        ));
    }
    group("div", "panel-section", children)
}

fn sharing(server: &Server, snapshot: Option<&Snapshot>, controllable: bool) -> Node {
    let mut children = vec![
        group(
            "div",
            "section-label",
            vec![icon("Users", 15), label("span", "", tr("共享会话"))],
        ),
        button(
            "button secondary wide",
            tr("刷新会话列表"),
            "",
            json!({"action":"control","id":server.id,"control":{"action":"active_users"}}),
            !snapshot.is_some_and(|s| s.video_connected),
        ),
    ];
    if let Some(snapshot) = snapshot {
        let sharing = &snapshot.sharing;
        children.push(label("p", "input-help", tr(sharing.role.label())));
        if snapshot.video_connected && !controllable {
            children.push(button(
                "button secondary",
                tr("申请控制权限"),
                "KeyRound",
                json!({"action":"control","id":server.id,"control":{"action":"request_control"}}),
                !sharing.can_request(),
            ));
        }
        children.push(select("policy", tr("后续控制权限申请"), &[
            ("ask", tr("逐次询问")),
            ("view_only", tr("自动仅允许查看")),
            ("deny", tr("自动拒绝访问")),
        ], json!({"key":format!("sharing-policy-{}",server.id),"value":sharing.policy.value(),"action":{"action":"sharing_policy","id":server.id},"disabled":!controllable})));

        children.push(label(
            "p",
            "input-help",
            sharing.message.as_deref().unwrap_or(""),
        ));
        for (waiting, title) in [
            (&sharing.waiting, tr("等待控制权限应答")),
            (&sharing.handoff, tr("正在移交控制权限")),
        ] {
            if let Some(waiting) = waiting {
                let user = waiting
                    .user
                    .as_ref()
                    .map(|u| format!(" · {}（{}）", u.name, u.address))
                    .unwrap_or_default();
                children.push(label(
                    "p",
                    "input-help",
                    lformat!(
                        "{title}{user} · 剩余 {} 秒",
                        waiting.seconds_remaining,
                        title = title,
                        user = user
                    ),
                ));
            }
        }
        for request in &sharing.requests {
            let user = &request.user;
            let mut row = vec![
                label(
                    "strong",
                    "",
                    lformat!(
                        "{}（{}）申请{}",
                        user.name,
                        user.address,
                        if request.existing_session {
                            tr("控制权限")
                        } else {
                            tr("访问")
                        }
                    ),
                ),
                label(
                    "small",
                    "",
                    lformat!(
                        "{} · 会话 #{} · 剩余 {} 秒",
                        tr(user.role_label()),
                        user.id,
                        request.seconds_remaining
                    ),
                ),
            ];
            let mut choices = vec![("grant", tr("允许控制")), ("partial", tr("仅查看"))];
            if !request.existing_session {
                choices.push(("deny", tr("拒绝访问")));
            }
            choices.extend([
                ("block_partial", tr("仅查看并自动应答")),
                ("block_deny", tr("拒绝并阻止后续申请")),
            ]);
            row.push(group("div", "shortcut-grid", choices.into_iter().map(|(operation,title)| {
                let mut action = button("", title, "", json!({"action":"share","id":server.id,"user_id":user.id,"request_token":request.token,"operation":operation}), !controllable);
                if operation == "grant" {
                    action.props["confirm"] = json!(lformat!("将控制权限交给 {}？活动介质重定向将终止。",user.name));
                }
                action
            }).collect()));
            let mut node = group("div", "session-user", row);
            node.props["key"] = json!(format!("sharing-request-{}", request.token));
            children.push(node);
        }
        for user in &snapshot.users {
            let own = snapshot.own_session_id == Some(user.id);
            let mut values = vec![
                label(
                    "strong",
                    "",
                    format!("{}{}", user.name, if own { tr(" · 当前会话") } else { "" }),
                ),
                label("small", "", &user.address),
                label(
                    "small",
                    "",
                    lformat!("会话 #{} · {}", user.id, tr(user.role_label())),
                ),
            ];
            if !own && controllable {
                let mut transfer = button(
                    "",
                    tr("转交控制"),
                    "",
                    json!({"action":"share","id":server.id,"user_id":user.id,"identity":user.identity(),"operation":"transfer"}),
                    false,
                );
                transfer.props["confirm"] = json!(lformat!(
                    "将控制权限转交给 {}？活动介质重定向将终止。",
                    user.name
                ));
                let mut disconnect = button(
                    "danger",
                    tr("断开会话"),
                    "",
                    json!({"action":"share","id":server.id,"user_id":user.id,"identity":user.identity(),"operation":"disconnect"}),
                    false,
                );
                disconnect.props["confirm"] = json!(lformat!("断开 {} 的远程会话？", user.name));
                values.push(group("div", "shortcut-grid", vec![transfer, disconnect]));
            }
            let mut row = group("div", "session-user", values);
            row.props["key"] = json!(format!(
                "sharing-user-{}-{}-{}",
                user.id, user.name, user.address
            ));
            children.push(row);
        }
    }
    group("div", "panel-section", children)
}

fn media(server: &Server, snapshot: Option<&Snapshot>) -> Node {
    use amikvm_core::media::scsi::Kind;
    let config = snapshot.and_then(|s| s.config.as_ref());
    let allowed = snapshot.is_some_and(|s| s.phase == "connected" && (s.web_only || s.can_control))
        && config.is_some_and(|c| c.privileges & 2 != 0);
    let mut children = vec![group(
        "div",
        "section-label",
        vec![icon("HardDrive", 15), label("span", "", tr("虚拟介质"))],
    )];
    for (kind, title) in [
        (Kind::Cdrom, tr("CD / DVD 镜像")),
        (Kind::HardDisk, tr("硬盘 / USB 镜像")),
        (Kind::Floppy, tr("软盘镜像")),
    ] {
        let cd = kind == Kind::Cdrom;
        let enabled = config.is_some_and(|c| if cd { c.cd_enabled } else { c.hd_enabled });
        let count = config.map_or(0, |c| if cd { c.cd_instances } else { c.hd_instances });
        let occupied = snapshot.map_or(0, |s| {
            s.media
                .iter()
                .filter(|m| (m.kind == Kind::Cdrom) == cd && m.active())
                .count()
        });
        children.push(button(
            "button secondary wide",
            title,
            "",
            json!({"action":"media_dialog","id":server.id,"kind":kind}),
            !allowed || !enabled || occupied >= count as usize,
        ));
    }
    if let Some(snapshot) = snapshot {
        for m in &snapshot.media {
            let phase = match m.phase.as_str() {
                "connecting" => tr("正在连接"),
                "connected" => tr("已连接"),
                "ejected" => tr("远程已弹出"),
                "terminated" => tr("BMC 已终止"),
                "service_restart" => tr("BMC 服务重启"),
                "removed" => tr("实体设备已移除"),
                "error" => tr("连接失败"),
                _ => tr("已断开"),
            };
            let mut item = vec![
                label(
                    "strong",
                    "",
                    lformat!(
                        "{} · 实例 {} · {phase}",
                        if m.kind == Kind::Cdrom {
                            "CD/DVD"
                        } else {
                            tr("磁盘")
                        },
                        m.slot + 1,
                        phase = phase
                    ),
                ),
                label("small", "record-path", &m.source),
                label(
                    "small",
                    "",
                    lformat!(
                        "{} · 读 {} KiB / 写 {} KiB{}",
                        if m.readonly {
                            tr("只读")
                        } else {
                            tr("读写")
                        },
                        m.bytes_read / 1024,
                        m.bytes_written / 1024,
                        if m.boost { " · Boost" } else { "" }
                    ),
                ),
            ];
            if m.physical {
                item.push(label(
                    "small",
                    "",
                    lformat!("实体设备 · 容量 {}", byte_size(m.capacity)),
                ));
            }
            if let Some(cache) = m.cache.filter(|c| c.enabled) {
                item.push(label(
                    "small",
                    "",
                    lformat!(
                        "本地预读 · 缓存 {:.1} MiB · 命中 {} 次",
                        cache.bytes as f64 / 1_048_576.,
                        cache.hits
                    ),
                ));
            }
            if m.phase == "connected" {
                item.push(button(
                    "button secondary wide",
                    tr("断开介质"),
                    "Unplug",
                    json!({"action":"media_stop","id":server.id,"kind":m.kind,"slot":m.slot}),
                    false,
                ));
            }
            if let Some(message) = &m.message {
                item.push(label("small", "media-error", translated(message)));
            }
            if let Some(reason) = m.rejection {
                if let Some(owner) = reason.owner() {
                    item.push(label(
                        "small",
                        "media-error",
                        lformat!("占用客户端：{}", owner),
                    ));
                }
                if let amikvm_core::error::MediaSessionError::Rejected { code } = reason {
                    item.push(label(
                        "small",
                        "media-error",
                        lformat!("BMC 介质返回码：{}", code),
                    ));
                }
            }
            children.push(group("div", "session-user", item));
        }
    }
    group("div", "panel-section", children)
}

fn folders(ui: &UiState, server: &Server, snapshot: Option<&Snapshot>) -> Node {
    let config = snapshot.and_then(|s| s.config.as_ref());
    let allowed = snapshot.is_some_and(|s| s.phase == "connected")
        && config.is_some_and(|c| c.privileges & 2 != 0 && c.hd_enabled);
    let free = config.is_some_and(|c| {
        (0..c.hd_instances).any(|slot| {
            !snapshot.is_some_and(|s| {
                s.media.iter().any(|m| {
                    m.kind != amikvm_core::media::scsi::Kind::Cdrom && m.slot == slot && m.active()
                })
            }) && !ui
                .folders
                .iter()
                .any(|f| f.server_id == server.id && f.slot == slot && f.phase == "creating")
        })
    });
    let mut children = vec![
        group(
            "div",
            "section-label",
            vec![
                icon("FolderOpen", 15),
                label("span", "", tr("文件夹重定向")),
            ],
        ),
        button(
            "button secondary wide",
            tr("连接本地文件夹"),
            "",
            json!({"action":"folder_dialog","id":server.id}),
            !allowed || !free,
        ),
    ];
    for folder in ui.folders.iter().filter(|f| f.server_id == server.id) {
        let busy = matches!(folder.phase, "creating" | "reading" | "applying");
        let phase = match folder.phase {
            "creating" => tr("正在创建"),
            "connected" => tr("已连接"),
            "reading" => tr("正在预览"),
            "applying" => tr("正在同步"),
            "preview" => tr("等待同步确认"),
            "error" => tr("创建失败"),
            _ => tr("工作镜像已保留"),
        };
        let mut row = vec![
            label(
                "strong",
                "",
                lformat!("实例 {} · {phase}", folder.slot + 1, phase = phase),
            ),
            label("small", "record-path", &folder.root),
            label("small", "record-path", &folder.image),
            label(
                "small",
                "",
                if folder.readonly {
                    tr("只读")
                } else {
                    tr("读写 · 同步前会检查本地冲突")
                },
            ),
        ];
        if busy {
            row.push(label(
                "small",
                "",
                lformat!(
                    "{} · {} 项 · {} / {} KiB",
                    folder.stage,
                    folder.files,
                    folder.bytes / 1024,
                    folder.total_bytes / 1024
                ),
            ));
            row.push(button(
                "button secondary wide",
                tr("取消操作"),
                "",
                json!({"action":"folder_cancel","id":folder.id}),
                false,
            ));
        } else {
            if !folder.readonly && folder.phase != "error" {
                row.push(button(
                    "button secondary wide",
                    if folder.phase == "connected" {
                        tr("停止并预览同步")
                    } else {
                        tr("预览同步")
                    },
                    "",
                    json!({"action":"folder_prepare","id":folder.id}),
                    false,
                ));
            }
            let mut discard = button(
                "button secondary wide",
                if folder.readonly {
                    tr("断开并删除工作镜像")
                } else {
                    tr("丢弃远程修改")
                },
                "",
                json!({"action":"folder_discard","id":folder.id}),
                false,
            );
            discard.props["confirm"] = json!(if folder.readonly {
                tr("断开映射并删除工作镜像？原始文件夹不会改变。")
            } else {
                tr("丢弃工作镜像中的远程修改？原始文件夹不会改变。")
            });
            row.push(discard);
        }
        if let Some(message) = &folder.message {
            row.push(label("small", "media-error", message));
        }
        children.push(group("div", "session-user", row));
    }
    group("div", "panel-section", children)
}

fn recording(ui: &UiState, server: &Server, snapshot: Option<&Snapshot>) -> Node {
    use amikvm_core::recording::Policy;
    let running = snapshot.is_some_and(|s| s.recording);
    let policy = if running {
        snapshot.unwrap().recording_policy
    } else {
        ui.recording_policies
            .get(&server.id)
            .copied()
            .unwrap_or_default()
    };
    let mut children = vec![group(
        "div",
        "section-label",
        vec![icon("Video", 15), label("span", "", tr("MP4 录制"))],
    )];
    if running {
        children.push(button(
            "button secondary wide",
            tr("停止并保存录制"),
            "Square",
            json!({"action":"record","id":server.id}),
            false,
        ));
    } else {
        children.push(node("form", json!({"key":format!("record-settings-{}",server.id),"values":{"seconds":ui.recording_seconds.get(&server.id).copied().unwrap_or(20).to_string(),"policy":policy.value()},"action":{"action":"record","id":server.id}}), vec![
            field("seconds", tr("录制时长（秒）"), "number", json!({"min":1,"max":1800,"required":true})),
            select("policy", tr("分辨率与无信号策略"), &[(Policy::Normalized.value(), tr(Policy::Normalized.label())), (Policy::NativeSegments.value(), tr(Policy::NativeSegments.label()))], json!({})),

            node("button", json!({"className":"button secondary wide","text":tr("开始录制"),"icon":"Circle","type":"submit","disabled":!snapshot.is_some_and(|s|s.video_connected && s.phase == "connected")}), vec![]),
        ]));
    }

    if let Some(snapshot) = snapshot.filter(|s| s.recording || s.recording_path.is_some()) {
        children.push(label(
            "small",
            "record-path",
            format!(
                "{} / {}",
                playback_time(snapshot.recording_elapsed_ms),
                playback_time(u64::from(snapshot.recording_limit_seconds) * 1000)
            ),
        ));
        if snapshot.recording_policy == Policy::NativeSegments {
            children.push(label(
                "small",
                "record-path",
                lformat!(
                    "成品 {} · 跳过无信号 {}",
                    playback_time(snapshot.recording_written_ms),
                    playback_time(snapshot.recording_skipped_ms)
                ),
            ));
        }
        if let Some(message) = &snapshot.recording_message {
            children.push(label("small", "record-path", message));
        }
    }
    if running {
        let paused = snapshot.is_some_and(|s| s.recording_paused);
        children.push(button(
            "button secondary wide",
            if paused {
                tr("继续录制")
            } else {
                tr("暂停录制")
            },
            if paused { "Play" } else { "Pause" },
            json!({"action":"record_pause","id":server.id}),
            false,
        ));
    }
    if let Some(snapshot) = snapshot {
        for (index, path) in snapshot.recording_outputs.iter().enumerate() {
            children.push(label(
                "small",
                "record-path",
                lformat!("文件 {}：{}", index + 1, path),
            ));
        }
        if snapshot.recording_outputs.is_empty() {
            if let Some(path) = &snapshot.recording_path {
                children.push(label(
                    "small",
                    "record-path",
                    lformat!("目标：{path}", path = path),
                ));
            }
        }
    }
    children.push(button(
        "button secondary wide",
        tr("查看 BMC 录像"),
        "Video",
        json!({"action":"recordings_dialog","id":server.id}),
        !snapshot.is_some_and(|s| s.phase == "connected"),
    ));
    group("div", "panel-section", children)
}

fn byte_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn playback_time(milliseconds: u64) -> String {
    let seconds = milliseconds / 1000;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn playback(ui: &UiState) -> Node {
    let Some(player) = &ui.playback else {
        return group(
            "section",
            "server-page playback-empty",
            vec![
                icon("Video", 42),
                label("h1", "", tr("录像回放")),
                button(
                    "button primary",
                    tr("打开录像文件"),
                    "FolderOpen",
                    json!({"action":"playback_open"}),
                    false,
                ),
            ],
        );
    };
    let control =
        |operation| json!({"action":"playback_control","id":player.id,"operation":operation});
    let stopped = matches!(player.phase.as_str(), "stopped" | "ended" | "error");
    let paused = player.phase == "paused";
    let title = match player.phase.as_str() {
        "loading" => tr("正在读取录像"),
        "seeking" => tr("正在定位画面"),
        "playing" => tr("正在回放"),
        "paused" => tr("已暂停"),
        "ended" => tr("回放结束"),
        "stopped" => tr("回放已停止"),
        _ => tr("回放失败"),
    };
    let mut details = vec![
        label("div", "section-label", tr("录像文件")),
        label("strong", "", &player.name),
        label("small", "record-path", &player.path),
        label("p", "input-help", &player.format),
        button(
            "button secondary wide",
            tr("打开其他录像"),
            "FolderOpen",
            json!({"action":"playback_open"}),
            false,
        ),
        button(
            "button secondary wide",
            tr("关闭录像"),
            "X",
            json!({"action":"playback_close","id":player.id}),
            false,
        ),
    ];
    if let Some(error) = &player.error {
        details.push(label("p", "inline-error", translated(error)));
    }
    let mut status = vec![
        label("span", "", title),
        label("span", "", format!("{} × {}", player.width, player.height)),
        label(
            "span",
            "push-right",
            format!(
                "{} / {}",
                playback_time(player.position_ms),
                player
                    .duration_ms
                    .map(playback_time)
                    .unwrap_or("--:--:--".into())
            ),
        ),
    ];
    if let Some(duration) = player.duration_ms {
        status.insert(0, field("position", tr("回放位置"), "range", json!({"className":"playback-position","value":(player.position_ms / 1000).to_string(),
            "min":0,"max":duration/1000,"disabled":player.phase=="error","action":{"action":"playback_seek","id":player.id}})));
    }
    group(
        "section",
        "console-layout",
        vec![
            group(
                "div",
                "console-main",
                vec![
                    group(
                        "div",
                        "console-toolbar",
                        vec![
                            group(
                                "div",
                                "toolbar-title",
                                vec![icon("Video", 17), label("strong", "", &player.name)],
                            ),
                            group(
                                "div",
                                "toolbar-actions",
                                vec![
                                    titled(
                                        if paused || stopped { "Play" } else { "Pause" },
                                        if paused || stopped {
                                            tr("播放")
                                        } else {
                                            tr("暂停")
                                        },
                                        control(if stopped { "play" } else { "pause" }),
                                        player.phase == "error",
                                    ),
                                    titled(
                                        "RefreshCw",
                                        tr("从头播放"),
                                        control("restart"),
                                        player.phase == "error",
                                    ),
                                    titled("Square", tr("停止回放"), control("stop"), stopped),
                                    titled(
                                        "Maximize",
                                        tr("全屏"),
                                        json!({"action":"fullscreen"}),
                                        false,
                                    ),
                                ],
                            ),
                        ],
                    ),
                    group(
                        "div",
                        "video-surface",
                        vec![
                            node(
                                "video",
                                json!({"serverId":player.id,"enabled":false,"streaming":true,"visible":player.signal,"style":{"maxWidth":"100%","maxHeight":"100%"}}),
                                vec![],
                            ),
                            group(
                                "div",
                                if player.signal {
                                    "video-state hidden"
                                } else {
                                    "video-state"
                                },
                                vec![
                                    group("div", "video-state-icon", vec![icon("Video", 34)]),
                                    label("h3", "", title),
                                    label(
                                        "p",
                                        "",
                                        player
                                            .error
                                            .as_deref()
                                            .unwrap_or(tr("录像当前没有视频信号")),
                                    ),
                                ],
                            ),
                        ],
                    ),
                    group("div", "console-status playback-status", status),
                ],
            ),
            group(
                "aside",
                "control-panel",
                vec![group("div", "panel-section", details)],
            ),
        ],
    )
}

fn exit_dialog(plan: &amikvm_core::sharing::exit::Plan) -> Node {
    use amikvm_core::sharing::exit::{Phase, Scope};
    let title = if plan.scope == Scope::Application {
        tr("退出前选择下一控制者")
    } else {
        tr("断开前选择下一控制者")
    };
    let choosing = plan.phase == Phase::Choosing;
    let cancellable = matches!(plan.phase, Phase::Preparing | Phase::Choosing);
    let cancel = json!({"action":"exit_cancel","plan":plan.id});
    let mut h = heading(title, "Users");
    h.children[2].props["action"] = cancel.clone();
    h.children[2].props["disabled"] = json!(!cancellable);
    let mut children = vec![
        h,
        label(
            "p",
            "input-help",
            match plan.phase {
                Phase::Preparing => tr("正在读取共享会话…").to_owned(),
                Phase::Choosing => lformat!(
                    "剩余 {} 秒。到时直接关闭；不会自动将控制权交给所选用户。",
                    plan.seconds_remaining
                ),
                _ => tr("正在结束介质、保存录像并关闭会话，请等待。").to_owned(),
            },
        ),
    ];
    for step in &plan.steps {
        let mut options = vec![json!({"value":"","label":tr("不转交控制权限")})];
        options.extend(step.candidates.iter().map(|c| json!({"value":c.token,"label":lformat!("{} · {} · 会话 #{} · {}",c.user.name,c.user.address,c.user.id,tr(c.user.role_label()))})));
        let mut choice = field(
            "next_master",
            &step.name,
            "select",
            json!({"key":format!("exit-step-{}",step.server_id),"value":step.selected.map(|v|v.to_string()).unwrap_or_default(),"disabled":!choosing,"action":{"action":"exit_choose","plan":plan.id,"server":step.server_id}}),
        );
        choice.props["options"] = json!(options);
        children.push(choice);
        if let Some(message) = &step.message {
            children.push(label("p", "media-error", translated(message)));
        }
    }

    for message in &plan.messages {
        children.push(label("p", "media-error", translated(message)));
    }
    children.push(group(
        "div",
        "modal-actions",
        vec![
            button(
                "button secondary",
                tr("取消关闭"),
                "",
                cancel.clone(),
                !cancellable,
            ),
            button(
                "button secondary",
                tr("直接关闭"),
                "",
                json!({"action":"exit_finish","plan":plan.id,"transfer":false}),
                !choosing,
            ),
            button(
                "button primary",
                tr("按选择转交并关闭"),
                "",
                json!({"action":"exit_finish","plan":plan.id,"transfer":true}),
                !choosing,
            ),
        ],
    ));
    node(
        "dialog",
        json!({"key":format!("close-plan-{}",plan.id),"className":"modal modal-wide","action":cancel}),
        children,
    )
}

//! 右侧内容区路由与共享列表组件。

use super::*;
use crate::app::SAVE_TEXT;
use crate::ui::i18n::Language;
use crate::ui::text_field::TextField;
#[cfg(target_os = "linux")]
use sodam_core::config::TitlebarMode;
use std::sync::Arc;

/// 字节 → 人类可读（KB / MB / GB）。
fn human_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let value = bytes as f64;
    if value >= KB * KB * KB {
        format!("{:.2} GB", value / (KB * KB * KB))
    } else if value >= KB * KB {
        format!("{:.1} MB", value / (KB * KB))
    } else if value >= KB {
        format!("{:.0} KB", value / KB)
    } else {
        format!("{bytes} B")
    }
}

/// 秒 → 「小时:分:秒 / 分:秒」。
pub(crate) fn duration_of(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (hours, minutes, secs) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    }
}

/// 设置页：音质偏好选择 + 账号信息 + 其他。
/// 设置页 → 账户板块：头像 + 昵称/ID/VIP；未登录给「去登录」并弹二维码 modal。
fn account_section(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let logged_in = !root.settings.cookie.trim().is_empty();
    let avatar_path = root
        .account
        .as_ref()
        .filter(|_| logged_in)
        .map(|info| info.avatar_url.clone())
        .filter(|url| !url.trim().is_empty())
        .and_then(|url| root.cover_of(&url));

    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(theme::space::LG))
        .p(px(theme::space::LG))
        .rounded(px(theme::radius::CARD))
        .bg(theme::surface_elevated())
        .border_1()
        .border_color(theme::border())
        .child(if logged_in {
            cover(avatar_path, 64.0, 999.0)
        } else {
            cover(None, 64.0, 999.0)
        })
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.0))
                .gap(px(theme::space::XS))
                .child(
                    div()
                        .text_size(theme::Text::Large.size())
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(if logged_in {
                            match &root.account {
                                Some(info) if !info.nickname.is_empty() => info.nickname.clone(),
                                _ => root.tr("已登录").to_string(),
                            }
                        } else {
                            root.tr("未登录").to_string()
                        }),
                )
                .child(
                    div()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(if logged_in {
                            match &root.account {
                                Some(info) => {
                                    let vip = if info.vip {
                                        root.tr("VIP")
                                    } else {
                                        root.tr("非 VIP")
                                    };
                                    root.localized(
                                        "用户 {} · {}",
                                        &[info.user_id.to_string(), vip.to_string()],
                                    )
                                }
                                None => root.tr("正在读取账号信息…").to_string(),
                            }
                        } else {
                            root.tr("登录后才能使用搜索、歌单与播放").to_string()
                        }),
                )
                .when(!logged_in, |this| {
                    this.child(
                        div()
                            .id("go-login")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(theme::space::SM))
                            .h(px(theme::size::CONTROL))
                            .px(px(theme::space::LG))
                            .mt(px(theme::space::SM))
                            .rounded(px(theme::radius::PILL))
                            .bg(theme::accent())
                            .cursor_pointer()
                            .hover(|style| style.opacity(0.9))
                            .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                                root.login_modal_open = true;
                                root.start_login(cx);
                            }))
                            .child(
                                svg()
                                    .path(icons::path("log-in"))
                                    .size(px(theme::ICON))
                                    .text_color(theme::accent_foreground()),
                            )
                            .child(
                                div()
                                    .text_size(theme::Text::Small.size())
                                    .text_color(theme::accent_foreground())
                                    .child(root.tr("去登录")),
                            ),
                    )
                }),
        )
        .when(logged_in, |this| {
            this.child(
                div()
                    .id("logout")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(theme::space::SM))
                    .flex_none()
                    .h(px(theme::size::CONTROL))
                    .px(px(theme::space::LG))
                    .rounded(px(theme::radius::PILL))
                    .bg(theme::surface_hover())
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::surface_selected()))
                    .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                        root.logout(cx);
                    }))
                    .child(
                        svg()
                            .path(icons::path("x"))
                            .size(px(theme::ICON))
                            .text_color(theme::text_muted()),
                    )
                    .child(
                        div()
                            .text_size(theme::Text::Small.size())
                            .text_color(theme::text_muted())
                            .child(root.tr("退出登录")),
                    ),
            )
        })
        .into_any_element()
}

/// Linux 专属：窗口控制按钮的显示策略。
///
/// 只有 Linux 需要这个开关：Windows 一定会去掉系统标题栏、macOS 用系统红绿灯，
/// 其它平台直接返回空，不占布局。
#[cfg(target_os = "linux")]
fn window_controls_section(root: &Root, cx: &mut Context<Root>) -> Option<AnyElement> {
    let current = TitlebarMode::from_key(&root.settings.titlebar);
    let rows: Vec<AnyElement> = [
        (
            TitlebarMode::Auto,
            root.tr("自动（跟随桌面）"),
            root.tr("由桌面是否要求客户端自绘决定：GNOME 等显示，Hyprland 等平铺桌面不显示"),
        ),
        (
            TitlebarMode::Always,
            root.tr("总是显示"),
            root.tr("即使桌面自己不管窗口，也显示最小化 / 最大化 / 关闭按钮"),
        ),
        (
            TitlebarMode::Never,
            root.tr("不显示"),
            root.tr("完全交给桌面管理窗口，不画任何按钮"),
        ),
    ]
    .into_iter()
    .map(|(mode, title, hint)| {
        let chosen = mode == current;
        div()
            .id(gpui::ElementId::Name(
                format!("titlebar-set-{}", mode.key()).into(),
            ))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(theme::space::LG))
            .px(px(theme::space::MD))
            .py(px(theme::space::SM))
            .rounded(px(theme::radius::ROW))
            .cursor_pointer()
            .when(chosen, |this| this.bg(theme::surface_selected()))
            .hover(|style| style.bg(theme::surface_hover()))
            .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                root.settings.titlebar = mode.key().to_string();
                if let Err(err) = root.settings.save() {
                    let message = root.failure_text("窗口按钮已切换，但保存失败", &err, SAVE_TEXT);
                    root.notify_error_once("titlebar-save-failed", message);
                } else {
                    let status = match mode {
                        TitlebarMode::Auto => root.tr("窗口按钮：自动（跟随桌面）").to_string(),
                        TitlebarMode::Always => root.tr("窗口按钮：总是显示").to_string(),
                        TitlebarMode::Never => root.tr("窗口按钮：不显示").to_string(),
                    };
                    root.set_status_text(status);
                }
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .text_size(theme::Text::Body.size())
                            .text_color(if chosen {
                                theme::text()
                            } else {
                                theme::text_muted()
                            })
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(theme::Text::Small.size())
                            .text_color(theme::text_muted())
                            .child(hint),
                    ),
            )
            .when(chosen, |this| {
                this.child(
                    svg()
                        .path(icons::path("check"))
                        .size(px(theme::ICON))
                        .text_color(theme::accent()),
                )
            })
            .into_any_element()
    })
    .collect();

    Some(
        div()
            .flex()
            .flex_col()
            .gap(px(theme::space::XS))
            .child(
                div()
                    .text_size(theme::Text::Large.size())
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child(root.tr("窗口")),
            )
            .child(
                div()
                    .text_size(theme::Text::Small.size())
                    .text_color(theme::text_muted())
                    .child(root.tr("Linux 下是否自绘最小化 / 最大化 / 关闭按钮")),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(theme::space::XS))
                    .children(rows),
            )
            .into_any_element(),
    )
}

#[cfg(not(target_os = "linux"))]
fn window_controls_section(_root: &Root, _cx: &mut Context<Root>) -> Option<AnyElement> {
    None
}

fn search_history_section(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let enabled = root.settings.search_history_is_enabled();
    let count = root.settings.search_history.len();
    let mut section = div()
        .flex()
        .flex_col()
        .gap(px(theme::space::XS))
        .child(
            div()
                .text_size(theme::Text::Large.size())
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(root.tr("搜索历史")),
        )
        .child(
            div()
                .text_size(theme::Text::Small.size())
                .text_color(theme::text_muted())
                .child(root.tr("仅保存在本机，最多保留 10 条；关闭后会清空记录")),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(theme::space::LG))
                .px(px(theme::space::MD))
                .py(px(theme::space::SM))
                .rounded(px(theme::radius::ROW))
                .bg(theme::surface())
                .child(
                    div()
                        .text_size(theme::Text::Body.size())
                        .text_color(theme::text())
                        .child(root.tr("保存搜索历史")),
                )
                .child(crate::ui::switch::render(
                    "search-history-toggle",
                    enabled,
                    |root, _window, cx| root.toggle_search_history(cx),
                    cx,
                )),
        );

    if count > 0 {
        section = section.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(theme::space::MD))
                .child(
                    div()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(root.localized("已保存 {} 条搜索历史", &[count.to_string()])),
                )
                .child(
                    div()
                        .id("settings-search-history-clear")
                        .cursor_pointer()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::accent())
                        .hover(|style| style.text_color(theme::text()))
                        .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                            root.clear_search_history(cx);
                        }))
                        .child(root.tr("清空")),
                ),
        );
    }

    section.into_any_element()
}

/// 设置页 → 诊断区块的一行开关：左侧标题与说明，右侧是既有的 switch 组件。
fn log_toggle_row(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(theme::space::LG))
        .px(px(theme::space::MD))
        .py(px(theme::space::SM))
        .rounded(px(theme::radius::ROW))
        .bg(theme::surface())
        .child(
            div()
                .flex()
                .flex_col()
                .min_w(px(0.0))
                .flex_1()
                .child(
                    div()
                        .text_size(theme::Text::Body.size())
                        .text_color(theme::text())
                        .child(root.tr("详细日志")),
                )
                .child(
                    div()
                        .text_size(theme::Text::Tiny.size())
                        .text_color(theme::text_muted())
                        .child(
                            root.tr(
                                "多记播放、预取与界面耗时等调试信息；仍不含 Cookie、签名与查询串",
                            ),
                        ),
                ),
        )
        .child(crate::ui::switch::render(
            "detailed-logs-toggle",
            root.settings.detailed_logs,
            |root, _window, cx| {
                let enabled = !root.settings.detailed_logs;
                root.set_detailed_logs(enabled, cx);
            },
            cx,
        ))
        .into_any_element()
}

/// 与「缓存」刻意分开：日志在数据目录，`清空缓存` 不碰它（见 #53）。
fn diagnostic_log_section(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let log_dir = sodam_core::log_outlet::log_dir();
    div()
        .flex()
        .flex_col()
        .gap(px(theme::space::SM))
        .child(
            div()
                .text_size(theme::Text::Large.size())
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(root.tr("诊断日志")),
        )
        .child(
            div()
                .text_size(theme::Text::Small.size())
                .text_color(theme::text_muted())
                .child(root.tr("默认记录在本机，用于排查播放问题；「清空缓存」不会删除它们")),
        )
        .child(
            div()
                .truncate()
                .text_size(theme::Text::Small.size())
                .text_color(theme::text_muted())
                .child(root.localized("日志目录：{}", &[log_dir.display().to_string()])),
        )
        .child(
            div()
                .text_size(theme::Text::Small.size())
                .text_color(theme::text_faint())
                .child(root.tr("日志可能含接口地址（查询串已移除），请勿公开上传")),
        )
        .child(log_toggle_row(root, cx))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(theme::space::SM))
                .children([
                    div()
                        .id("open-log-directory")
                        .flex()
                        .items_center()
                        .justify_center()
                        .h(px(theme::size::CONTROL))
                        .w(px(140.0))
                        .rounded(px(theme::radius::ROW))
                        .bg(theme::surface_elevated())
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::surface_hover()))
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text())
                        .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                            root.open_log_directory(cx);
                        }))
                        .child(root.tr("打开日志目录"))
                        .into_any_element(),
                    div()
                        .id("clear-diagnostic-logs")
                        .flex()
                        .items_center()
                        .justify_center()
                        .h(px(theme::size::CONTROL))
                        .w(px(140.0))
                        .rounded(px(theme::radius::ROW))
                        .bg(theme::surface_elevated())
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::surface_hover()))
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text())
                        .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                            root.clear_diagnostic_logs(cx);
                        }))
                        .child(if root.log_clearing {
                            root.tr("正在清除…")
                        } else {
                            root.tr("清除诊断日志")
                        })
                        .into_any_element(),
                ]),
        )
        .into_any_element()
}

pub(crate) fn settings_view(
    root: &mut Root,
    window: &Window,
    cx: &mut Context<Root>,
) -> AnyElement {
    // 档位与名称都从核心层派生（`TIERS` + `label_key`）。
    // 档位是“上限”而不是“目标”：libresoda 在没有任何候选低于上限时，会回退成全部候选
    //（实际可能比所选更高）。逐档「优先 X…回退」说明已合并为一条（#93 §5.2），
    // 在选项列表后只出现一次。
    let language = root.language;
    let label_for = |value: &str| crate::ui::player_bar::quality_label(value, language);
    let options: Vec<(&str, String)> = std::iter::once(sodam_core::quality::AUTO)
        .chain(sodam_core::quality::TIERS)
        .map(|value| (value, label_for(value)))
        .collect();
    // 批量升级同样从 `TIERS` 派生，避免播放档位能选、批量升级却少几档。
    let batch_tiers: Vec<(&str, String)> = sodam_core::quality::TIERS
        .into_iter()
        .map(|tier| (tier, label_for(tier)))
        .collect();
    // 先建行再交给 `children`：在 `children(map(...))` 里直接展开会让 rustfmt 反复改缩进。
    let batch_tier_rows: Vec<AnyElement> = batch_tiers
        .into_iter()
        .enumerate()
        .map(|(index, (tier, label))| {
            let is_selected = root.batch_upgrade_target
                == sodam_core::quality::QualityIntent::Tier(tier.to_string());
            div()
                .id(("batch-upgrade-tier", index))
                .h(px(theme::size::CONTROL_SM))
                .px(px(theme::space::MD))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(theme::radius::ROW))
                .bg(if is_selected {
                    theme::surface_selected()
                } else {
                    theme::surface_elevated()
                })
                .hover(|style| style.bg(theme::surface_hover()))
                .cursor_pointer()
                .text_size(theme::Text::Small.size())
                .child(label)
                .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                    if !root.batch_upgrading {
                        root.set_batch_upgrade_target(
                            sodam_core::quality::QualityIntent::Tier(tier.to_string()),
                            cx,
                        );
                    }
                }))
                .into_any_element()
        })
        .collect();
    let batch_target = root.batch_upgrade_target.clone();
    let batch_target_label = label_for(batch_target.as_setting());
    // 需要会员的档位只在账号**确认**有权益时才放行；与 `start_batch_upgrade` 共用同一条
    // 判定，按钮状态和执行结果不会各说各话。
    let batch_upgrade_blocked_by_vip =
        sodam_core::quality::batch_upgrade_needs_confirmed_entitlement(
            &batch_target,
            root.account.as_ref().map(|account| account.vip),
        );
    // 勾选的是**意图**；配置里的空串是历史写法，按「自动」显示。
    let current = root.settings.quality_intent().as_setting().to_string();

    // 来源提示与徽章：区分用户手动选择、程序推导与未知来源。
    // 在全新未配置的「自动」项上（Unknown + Auto）不展示徽章，避免新用户误解。
    // Derived = 程序写入（如 SODAM_QUALITY 覆盖），徽章与 hint 同一个语义（#77，
    // PR #348 审查意见）；旧配置缺来源是 Unknown，走下面的「来源未知」分支。
    let (source_badge, source_hint) =
        match (root.settings.quality_source, root.settings.quality_intent()) {
            (sodam_core::quality::QualitySource::Derived, _) => (
                Some(root.tr("程序推导")),
                Some(root.tr("当前档位由程序推导，点「自动」即可恢复跟随账号权益")),
            ),
            (
                sodam_core::quality::QualitySource::Unknown,
                sodam_core::quality::QualityIntent::Auto,
            ) => (None, None),
            (sodam_core::quality::QualitySource::Unknown, _) => (
                Some(root.tr("来源未知")),
                Some(root.tr("若不是你选的，点「自动」即可恢复跟随账号权益")),
            ),
            // 手动选择不再另写说明行（#93 §5.1）：徽章本身就说明了状态。
            (sodam_core::quality::QualitySource::User, _) => (Some(root.tr("手动选择")), None),
        };

    let theme_rows: Vec<AnyElement> = [
        (None, root.tr("跟随系统")),
        (Some(ThemeKind::Dark), root.tr("深色")),
        (Some(ThemeKind::Light), root.tr("浅色")),
    ]
    .into_iter()
    .map(|(value, title)| {
        // `None` = 跟随系统；`Some(kind)` = 固定主题。
        let chosen = match value {
            None => root.theme_follows_system,
            Some(kind) => !root.theme_follows_system && root.theme == kind,
        };
        let id = match value {
            None => "theme-set-system".to_string(),
            Some(kind) => format!("theme-set-{}", kind.key()),
        };
        // 跟随系统的色块用黑白对角渐变，直观表示「深色/浅色都由系统定」。
        let swatch: gpui::Background = match value {
            None => gpui::linear_gradient(
                135.0,
                gpui::linear_color_stop(theme::hex(0x0A0A0A), 0.0),
                gpui::linear_color_stop(theme::hex(0xFFFFFF), 1.0),
            ),
            Some(ThemeKind::Light) => theme::hex(0xFFFFFF).into(),
            Some(ThemeKind::Dark) => theme::hex(0x0A0A0A).into(),
        };
        div()
            .id(gpui::ElementId::Name(id.into()))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(theme::space::LG))
            .px(px(theme::space::MD))
            .py(px(theme::space::SM))
            .rounded(px(theme::radius::ROW))
            .cursor_pointer()
            .when(chosen, |this| this.bg(theme::surface_selected()))
            .hover(|style| style.bg(theme::surface_hover()))
            .on_click(
                cx.listener(move |root, _event: &ClickEvent, _window, cx| match value {
                    None => root.set_ui_theme_system(cx),
                    Some(kind) => root.set_ui_theme(kind, cx),
                }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(theme::space::SM))
                    .child(
                        div()
                            .size(px(14.0))
                            .rounded(px(theme::radius::PILL))
                            .border_1()
                            .border_color(theme::border())
                            .bg(swatch),
                    )
                    .child(
                        div()
                            .text_size(theme::Text::Body.size())
                            .text_color(if chosen {
                                theme::text()
                            } else {
                                theme::text_muted()
                            })
                            .child(title),
                    ),
            )
            .when(chosen, |this| {
                this.child(
                    svg()
                        .path(icons::path("check"))
                        .size(px(theme::ICON))
                        .text_color(theme::accent()),
                )
            })
            .into_any_element()
    })
    .collect();

    let rows: Vec<AnyElement> = options
        .iter()
        .map(|(value, title)| {
            let chosen = *value == current;
            // 回退说明不再逐档重复（#93 §5.2）：「自动」的含义留在自动行本身。
            let hint = if *value == sodam_core::quality::AUTO {
                root.tr("不设上限，按账号权益和单曲可用档位取最高")
            } else {
                ""
            };
            let value = value.to_string();
            div()
                .id(gpui::ElementId::Name(format!("quality-set-{value}").into()))
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(theme::space::LG))
                .px(px(theme::space::MD))
                .py(px(theme::space::SM))
                .rounded(px(theme::radius::ROW))
                .cursor_pointer()
                .when(chosen, |this| this.bg(theme::surface_selected()))
                .hover(|style| style.bg(theme::surface_hover()))
                .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                    root.set_quality(&value, cx);
                    cx.notify();
                }))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(theme::space::SM))
                                .child(
                                    div()
                                        .text_size(theme::Text::Body.size())
                                        .text_color(if chosen {
                                            theme::text()
                                        } else {
                                            theme::text_muted()
                                        })
                                        .child(title.clone()),
                                )
                                .when(chosen, |this| {
                                    this.when_some(source_badge, |this, badge| {
                                        this.child(
                                            div()
                                                .text_size(theme::Text::Tiny.size())
                                                .text_color(theme::text_muted())
                                                .px(px(6.0))
                                                .py(px(1.0))
                                                .rounded(px(theme::radius::PILL))
                                                .bg(theme::surface_hover())
                                                .child(badge),
                                        )
                                    })
                                }),
                        )
                        .when(!hint.is_empty(), |this| {
                            this.child(
                                div()
                                    .text_size(theme::Text::Tiny.size())
                                    .text_color(theme::text_faint())
                                    .child(hint),
                            )
                        }),
                )
                .when(chosen, |this| {
                    this.child(
                        svg()
                            .path(icons::path("check"))
                            .size(px(theme::ICON))
                            .text_color(theme::accent()),
                    )
                })
                .into_any_element()
        })
        .collect();

    let account_card = account_section(root, cx);

    div()
        .id("settings-scroll")
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        .gap(px(theme::space::LG))
        .overflow_y_scroll()
        .track_scroll(&root.settings_scroll)
        // 按下时上报自己是这次拖动的目标；之后移动、松开由根元素统一收。
        .on_mouse_down(
            MouseButton::Middle,
            cx.listener(|root, event: &MouseDownEvent, _window, _cx| {
                root.begin_middle_drag(f32::from(event.position.y));
            }),
        )
        .child(account_card)
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(theme::space::XS))
                .child(
                    div()
                        .text_size(theme::Text::Large.size())
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(root.tr("主题")),
                )
                .child(
                    div()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(if root.theme_follows_system {
                            root.tr("当前跟随系统偏好，手动选择后会固定主题")
                                .to_string()
                        } else {
                            root.tr("已固定主题；可随时切换深色或浅色").to_string()
                        }),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(theme::space::XS))
                .children(theme_rows),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(theme::space::XS))
                .child(
                    div()
                        .text_size(theme::Text::Large.size())
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(root.tr("语言")),
                )
                .child(
                    div()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(if root.language_follows_system {
                            root.tr("当前跟随系统语言；手动选择后会固定语言")
                        } else {
                            root.tr("已固定语言；可随时切换中文或 English")
                        }),
                ),
        )
        .child(
            div().flex().flex_col().gap(px(theme::space::XS)).children(
                [
                    (Language::System, root.tr("跟随系统")),
                    (Language::Chinese, "中文"),
                    (Language::English, "English"),
                ]
                .into_iter()
                .map(|(value, title)| {
                    let chosen = if root.language_follows_system {
                        matches!(value, Language::System)
                    } else {
                        !matches!(value, Language::System) && root.language == value
                    };
                    div()
                        .id(gpui::ElementId::Name(
                            format!("language-set-{:?}", value).into(),
                        ))
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap(px(theme::space::LG))
                        .px(px(theme::space::MD))
                        .py(px(theme::space::SM))
                        .rounded(px(theme::radius::ROW))
                        .cursor_pointer()
                        .when(chosen, |this| this.bg(theme::surface_selected()))
                        .hover(|style| style.bg(theme::surface_hover()))
                        .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                            match value {
                                Language::System => {
                                    root.language = Language::system_locale();
                                    root.language_follows_system = true;
                                    root.settings.language = "auto".to_string();
                                    if let Err(err) = root.settings.save() {
                                        let message = root.failure_text(
                                            "语言已恢复为跟随系统，但保存失败",
                                            &err,
                                            SAVE_TEXT,
                                        );
                                        root.notify_error_once("language-save-failed", message);
                                    } else {
                                        let status = if root.language.is_zh() {
                                            root.tr("语言已恢复为跟随系统").to_string()
                                        } else {
                                            "Language restored to system".to_string()
                                        };
                                        root.set_status_text(status);
                                    }
                                    // macOS 菜单栏的标题也要跟着换语言。
                                    #[cfg(target_os = "macos")]
                                    crate::menus::refresh(cx, root.language);
                                }
                                language => root.set_language(language, cx),
                            }
                            cx.notify();
                        }))
                        .child(
                            div()
                                .text_size(theme::Text::Body.size())
                                .text_color(if chosen {
                                    theme::text()
                                } else {
                                    theme::text_muted()
                                })
                                .child(title),
                        )
                        .when(chosen, |this| {
                            this.child(
                                svg()
                                    .path(icons::path("check"))
                                    .size(px(theme::ICON))
                                    .text_color(theme::accent()),
                            )
                        })
                })
                .collect::<Vec<_>>(),
            ),
        )
        .child(search_history_section(root, cx))
        .children(window_controls_section(root, cx))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(theme::space::XS))
                .child(
                    div()
                        .text_size(theme::Text::Large.size())
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(root.tr("音质偏好")),
                )
                .child(
                    div()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(root.tr("自动选择账号可用的最高音质")),
                )
                .children(source_hint.map(|hint| {
                    div()
                        .text_size(theme::Text::Tiny.size())
                        .text_color(theme::text_faint())
                        .child(hint)
                }))
                .child(
                    // 缓存按意图命名：自动档下同一首歌永远命中同一个文件，所以权益变了
                    // 也不会在旧曲上生效。这是主流客户端的一致行为，必须提前说明。
                    div()
                        .text_size(theme::Text::Tiny.size())
                        .text_color(theme::text_faint())
                        .child(
                            root.tr(
                                "已缓存的歌曲会保持原来的档位；想按新权益重取，清空缓存或换一首",
                            ),
                        ),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(theme::space::XS))
                .children(rows),
        )
        .child(
            // 回退规则只说一次（#93 §5.2）：档位是上限，曲库没有更低候选时由服务端回退。
            div()
                .px(px(theme::space::MD))
                .text_size(theme::Text::Tiny.size())
                .text_color(theme::text_faint())
                .child(root.tr("没有可用档位时自动回退可用档位")),
        )
        .child(super::shortcut_card::section(root, cx))
        .child({
            // 缓存：显示占用大小 + 一键清理（统计读后台快照，不在渲染路径扫盘）
            let (audio_bytes, audio_files, cover_bytes, cover_files) = root.cache_summary;
            let total = audio_bytes + cover_bytes;
            div()
                .flex()
                .flex_col()
                .gap(px(theme::space::SM))
                .child(
                    div()
                        .text_size(theme::Text::Large.size())
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(root.tr("缓存")),
                )
                .child(
                    div()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(if root.language.resolved().is_zh() {
                            root.localized(
                                "共 {}（歌曲 {} 首 {} / 封面 {} 张 {}）",
                                &[
                                    human_bytes(total),
                                    audio_files.to_string(),
                                    human_bytes(audio_bytes),
                                    cover_files.to_string(),
                                    human_bytes(cover_bytes),
                                ],
                            )
                        } else {
                            format!(
                                "Total {} ({} songs {}, {} covers {})",
                                human_bytes(total),
                                audio_files,
                                human_bytes(audio_bytes),
                                cover_files,
                                human_bytes(cover_bytes),
                            )
                        }),
                )
                .child(
                    div()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(root.localized(
                            "音频缓存上限：{}",
                            &[human_bytes(
                                root.settings.audio_cache_limit_mb * 1024 * 1024,
                            )],
                        )),
                )
                .child(
                    div().flex().flex_wrap().gap(px(theme::space::SM)).children(
                        [
                            (
                                "−1 GiB".to_string(),
                                root.settings.audio_cache_limit_mb.saturating_sub(1024),
                            ),
                            ("5 GiB".to_string(), 5 * 1024),
                            ("10 GiB".to_string(), 10 * 1024),
                            ("20 GiB".to_string(), 20 * 1024),
                            ("50 GiB".to_string(), 50 * 1024),
                            (
                                "+1 GiB".to_string(),
                                root.settings.audio_cache_limit_mb.saturating_add(1024),
                            ),
                        ]
                        .into_iter()
                        .enumerate()
                        .map(|(index, (label, limit))| {
                            div()
                                .id(("cache-limit", index))
                                .h(px(theme::size::CONTROL_SM))
                                .px(px(theme::space::MD))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(theme::radius::ROW))
                                .bg(if root.settings.audio_cache_limit_mb == limit {
                                    theme::surface_selected()
                                } else {
                                    theme::surface_elevated()
                                })
                                .hover(|style| style.bg(theme::surface_hover()))
                                .cursor_pointer()
                                .text_size(theme::Text::Small.size())
                                .child(label)
                                .on_click(cx.listener(
                                    move |root, _event: &ClickEvent, _window, cx| {
                                        root.set_cache_limit_mb(limit, cx);
                                    },
                                ))
                        }),
                    ),
                )
                .child(
                    div()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(root.tr("自动清理较早使用的歌曲；正在播放或下载的文件暂时保留")),
                )
                .child(
                    div()
                        .id("clear-cache")
                        .flex()
                        .items_center()
                        .justify_center()
                        .h(px(theme::size::CONTROL))
                        .w(px(140.0))
                        .rounded(px(theme::radius::ROW))
                        .bg(theme::surface_elevated())
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::surface_hover()))
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text())
                        .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                            root.clear_cache(cx);
                        }))
                        .child(if root.cache_clearing {
                            root.tr("正在清理…")
                        } else {
                            root.tr("清空缓存")
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(theme::space::XS))
                        .mt(px(theme::space::SM))
                        .child(
                            div()
                                .text_size(theme::Text::Large.size())
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::text())
                                .child(root.tr("升级已缓存歌曲")),
                        )
                        .child(
                            div()
                                .text_size(theme::Text::Small.size())
                                .text_color(theme::text_muted())
                                .child(
                                    root.tr(
                                        "将已有低档位缓存重新拉取为更高音质，失败不会损坏旧缓存",
                                    ),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap(px(theme::space::SM))
                                .children(batch_tier_rows),
                        )
                        .child({
                            // 档位未知的旧缓存另起一行（#89）：这几首不是「已知比目标低」，是旧版本
                            // 没记档位。被权益挡住、或这一轮全都抑制掉了，也要照样报出来——
                            // 「没有可升级的缓存歌曲」在这种存量下就不是实话（P5 的原话）。
                            // 只报事实、不写「需重下」：这一轮到底会不会重下已经由上一行的可升级数说过。
                            let legacy_note = if root.batch_upgrading {
                                None
                            } else {
                                root.batch_upgrade_estimate
                                    .as_ref()
                                    .filter(|estimate| estimate.unknown_rank_count > 0)
                                    .map(|estimate| {
                                        root.localized(
                                            "{} 首旧缓存未记录档位",
                                            &[estimate.unknown_rank_count.to_string()],
                                        )
                                    })
                            };
                            let estimate_text = if batch_upgrade_blocked_by_vip {
                                root.localized(
                                    "「{}」音质需 VIP 会员权限，当前账号未确认该权益",
                                    std::slice::from_ref(&batch_target_label),
                                )
                            } else if root.batch_upgrading {
                                root.localized(
                                    "正在升级 ({} / {})…",
                                    &[
                                        root.batch_upgrade_progress.done.to_string(),
                                        root.batch_upgrade_progress.total.to_string(),
                                    ],
                                )
                            } else if let Some(estimate) = &root.batch_upgrade_estimate {
                                if estimate.upgradeable_count == 0 {
                                    // 原因（已达目标 / 无更高档位 / 条件不支持）按需进「详情」，
                                    // 不在这里铺三条并列条件（#93 §5.2）。
                                    root.tr("没有可升级的缓存歌曲").to_string()
                                } else {
                                    let delta = estimate.net_disk_delta_bytes();
                                    let delta_str = if delta >= 0 {
                                        format!("+{}", human_bytes(delta as u64))
                                    } else {
                                        format!("-{}", human_bytes((-delta) as u64))
                                    };
                                    root.localized(
                                        "{} 首歌曲可升级 · 预估流量 {} · 存储变化 {}",
                                        &[
                                            estimate.upgradeable_count.to_string(),
                                            human_bytes(estimate.estimated_traffic_bytes),
                                            delta_str,
                                        ],
                                    )
                                }
                            } else {
                                root.tr("正在计算升级预估…").to_string()
                            };
                            div()
                                .text_size(theme::Text::Small.size())
                                .text_color(theme::text_muted())
                                .child(estimate_text)
                                .children(legacy_note)
                        })
                        .child({
                            let upgradeable = root
                                .batch_upgrade_estimate
                                .as_ref()
                                .map_or(0, |e| e.upgradeable_count);
                            let can_upgrade = !root.batch_upgrading
                                && !batch_upgrade_blocked_by_vip
                                && upgradeable > 0;
                            let mut btn = div()
                                .id("batch-upgrade")
                                .flex()
                                .items_center()
                                .justify_center()
                                .h(px(theme::size::CONTROL))
                                .w(px(140.0))
                                .rounded(px(theme::radius::ROW))
                                .text_size(theme::Text::Small.size());

                            if can_upgrade {
                                btn = btn
                                    .bg(theme::accent())
                                    .text_color(theme::accent_foreground())
                                    .cursor_pointer()
                                    .hover(|style| style.opacity(0.9))
                                    .on_click(cx.listener(
                                        |root, _event: &ClickEvent, _window, cx| {
                                            root.start_batch_upgrade(cx);
                                        },
                                    ));
                            } else {
                                btn = btn
                                    .bg(theme::surface_elevated())
                                    .text_color(theme::text_faint());
                            }

                            btn.child(if root.batch_upgrading {
                                root.tr("正在升级…")
                            } else {
                                root.tr("批量升级")
                            })
                        }),
                )
        })
        .child(diagnostic_log_section(root, cx))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(theme::space::XS))
                .child(
                    div()
                        .text_size(theme::Text::Large.size())
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(root.tr("其他")),
                )
                .child(
                    // 签名服务（#321）：点击展开面板——服务列表、测延时、切换、添加与恢复内置。
                    // token 只以掩码出现，真实值不进任何文案。
                    div()
                        .id("signer-row")
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(theme::space::SM))
                        .px(px(theme::space::MD))
                        .py(px(theme::space::SM))
                        .rounded(px(theme::radius::ROW))
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::surface_hover()))
                        .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                            root.settings_signers_open = !root.settings_signers_open;
                            cx.notify();
                        }))
                        .child(
                            div()
                                .text_size(theme::Text::Small.size())
                                .text_color(theme::text_muted())
                                .child(if root.settings.signer_url.trim().is_empty() {
                                    root.localized("签名服务：{}", &[root.tr("未配置").to_string()])
                                } else {
                                    root.localized(
                                        "签名服务：{}",
                                        &[sodam_core::signers::signer_display_name(
                                            &root.settings.signer_url,
                                        )],
                                    )
                                }),
                        )
                        .child(
                            svg()
                                .path(icons::path("chevron-right"))
                                .size(px(theme::ICON_SM))
                                .text_color(theme::text_faint())
                                .with_transformation(gpui::Transformation::rotate(
                                    gpui::percentage(if root.settings_signers_open {
                                        0.25
                                    } else {
                                        0.0
                                    }),
                                )),
                        ),
                )
                .when(root.settings_signers_open, |this| {
                    this.child(signers_panel(root, window, cx))
                })
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(theme::space::SM))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .truncate()
                                .text_size(theme::Text::Small.size())
                                .text_color(theme::text_muted())
                                .child(if root.language.resolved().is_zh() {
                                    root.localized(
                                        "配置：{}",
                                        &[sodam_core::Settings::config_path()
                                            .display()
                                            .to_string()],
                                    )
                                } else {
                                    format!(
                                        "Config: {}",
                                        sodam_core::Settings::config_path().display()
                                    )
                                }),
                        )
                        .child(
                            div()
                                .id("edit-config")
                                .flex()
                                .flex_row()
                                .items_center()
                                .justify_center()
                                .flex_none()
                                .h(px(theme::size::CONTROL_SM))
                                .px(px(theme::space::MD))
                                .rounded(px(theme::radius::ROW))
                                .bg(theme::surface_elevated())
                                .cursor_pointer()
                                .hover(|style| style.bg(theme::surface_hover()))
                                .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                                    root.open_config_file(cx);
                                }))
                                .child(
                                    div()
                                        .text_size(theme::Text::Tiny.size())
                                        .text_color(theme::text_muted())
                                        .child(root.tr("编辑")),
                                ),
                        ),
                ),
        )
        .child({
            let repository = env!("CARGO_PKG_REPOSITORY");
            let contributors = root.contributors_image.clone();
            // 渲染路径：失败态下不自动重试，否则 notify → 重绘 → 再拉取成死循环。
            root.ensure_contributors(cx, false);
            div()
                .id("about-footer")
                .flex()
                .flex_col()
                .gap(px(theme::space::MD))
                .p(px(theme::space::MD))
                .rounded(px(theme::radius::CARD))
                .bg(theme::surface_elevated())
                .border_1()
                .border_color(theme::border())
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(theme::space::MD))
                        .child(
                            img("icons/sodam-logo.svg")
                                .size(px(36.0))
                                .flex_none()
                                .rounded(px(theme::radius::ROW))
                                .object_fit(ObjectFit::Cover),
                        )
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .min_w(px(0.0))
                                .gap(px(theme::space::XS))
                                .child(
                                    div()
                                        .text_size(theme::Text::Small.size())
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(theme::text())
                                        .child(format!("SodaM v{}", env!("CARGO_PKG_VERSION"))),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(theme::Text::Tiny.size())
                                        .text_color(theme::text_muted())
                                        .child(format!("GitHub: {repository}")),
                                ),
                        )
                        .child(
                            div()
                                .id("open-repository")
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .h(px(theme::size::CONTROL_SM))
                                .px(px(theme::space::MD))
                                .rounded(px(theme::radius::ROW))
                                .cursor_pointer()
                                .hover(|style| style.bg(theme::surface_hover()))
                                .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                                    root.open_github_repository(cx);
                                }))
                                .child(
                                    svg()
                                        .path(icons::path("chevron-right"))
                                        .size(px(theme::ICON_SM))
                                        .text_color(theme::text_faint()),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(theme::space::SM))
                        .pt(px(theme::space::SM))
                        .border_t_1()
                        .border_color(theme::border())
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(theme::Text::Small.size())
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(theme::text())
                                        .child(root.tr("贡献者")),
                                )
                                .child(
                                    div()
                                        .text_size(theme::Text::Tiny.size())
                                        .text_color(theme::text_faint())
                                        .child("contrib.rocks"),
                                ),
                        )
                        .child(if let Some(image) = contributors {
                            div()
                                .flex()
                                .w_full()
                                .justify_center()
                                .child(
                                    img(Arc::clone(&image))
                                        .w_full()
                                        .h(px(64.0))
                                        .object_fit(ObjectFit::Contain),
                                )
                                .into_any_element()
                        } else if root.contributors_failed {
                            div()
                                .id("retry-contributors")
                                .h(px(64.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(theme::radius::ROW))
                                .bg(theme::surface_hover())
                                .cursor_pointer()
                                .hover(|style| style.opacity(0.85))
                                .text_size(theme::Text::Tiny.size())
                                .text_color(theme::text_muted())
                                .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                                    // 用户显式重试：force 越过失败态再拉一次。
                                    root.ensure_contributors(cx, true);
                                }))
                                .child(root.tr("加载失败，点击重试"))
                                .into_any_element()
                        } else {
                            div()
                                .h(px(64.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(theme::radius::ROW))
                                .bg(theme::surface_hover())
                                .text_size(theme::Text::Tiny.size())
                                .text_color(theme::text_faint())
                                .child(root.tr("正在加载贡献者…"))
                                .into_any_element()
                        }),
                )
        })
        .into_any_element()
}
/// 来源/状态小徽章：当前 / 内置。
fn badge_pill(label: impl Into<String>) -> AnyElement {
    div()
        .flex_none()
        .text_size(theme::Text::Tiny.size())
        .text_color(theme::text_muted())
        .px(px(6.0))
        .py(px(1.0))
        .rounded(px(theme::radius::PILL))
        .bg(theme::surface_hover())
        .child(label.into())
        .into_any_element()
}

/// 面板里的一句三级说明。
fn row_note(text: String) -> AnyElement {
    div()
        .px(px(theme::space::MD))
        .text_size(theme::Text::Tiny.size())
        .text_color(theme::text_faint())
        .child(text)
        .into_any_element()
}

/// 面板/列表行内小按钮（签名服务的测速 / 切换 / 删除 / 添加等）。
/// 样式跟随旧版式的小按钮语言（surface_elevated 底、Tiny 文本）。
fn signer_button(id: impl Into<gpui::ElementId>, label: String) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .h(px(theme::size::CONTROL_SM))
        .px(px(theme::space::MD))
        .rounded(px(theme::radius::ROW))
        .bg(theme::surface_elevated())
        .cursor_pointer()
        .hover(|style| style.bg(theme::surface_hover()))
        .text_size(theme::Text::Tiny.size())
        .text_color(theme::text_muted())
        .child(label)
}

/// 签名服务面板（#321，旧版式视觉）：服务列表（展示名 / 完整地址 / 激活与内置徽章 /
/// token 掩码 / 延时，行操作测速 / 全部测速 / 切换 / 删除）、「添加服务」表单与
/// 「恢复内置」。红线：token 只以掩码出现，真实值不进任何文案。
fn signers_panel(root: &mut Root, window: &Window, cx: &mut Context<Root>) -> AnyElement {
    let mut panel = div()
        .flex()
        .flex_col()
        .gap(px(theme::space::XS))
        .px(px(theme::space::MD))
        .pb(px(theme::space::SM));

    // 标题行：「全部测速」靠右；进行中不可点（换「测速中…」态），避免并发轰炸。
    panel = panel.child(
        div()
            .flex()
            .flex_row()
            .justify_end()
            .child(if root.signer_bulk_testing {
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(theme::size::CONTROL_SM))
                    .px(px(theme::space::MD))
                    .rounded(px(theme::radius::ROW))
                    .bg(theme::surface())
                    .text_size(theme::Text::Tiny.size())
                    .text_color(theme::text_faint())
                    .child(root.tr("测速中…"))
                    .into_any_element()
            } else {
                signer_button("signer-test-all", root.tr("全部测速").to_string())
                    .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                        root.measure_all_signers(cx);
                    }))
                    .into_any_element()
            }),
    );

    for (index, service) in root.settings.signers.clone().into_iter().enumerate() {
        // 「当前」徽章按**运行时实际服务**（镜像字段）标注（PR #348 三轮审查）：
        // env 覆盖生效时 active_signer 指向的保存行并不是正在用的那台，按
        // 下标标会把「当前」标错行、还把「设为当前」按钮藏掉。
        let is_active = service.url.trim() == root.settings.signer_url.trim();
        // 行回调按地址解析（PR #348 二轮审查）：几个按钮各捕获一份 clone，
        // 点击时以当时列表里的地址重新定位，不捕获渲染时下标。
        let url_for_activate = service.url.clone();
        let url_for_remove = service.url.clone();
        let is_builtin = sodam_core::Settings::is_builtin_signer(&service.url);
        let display = sodam_core::signers::signer_display_name(&service.url);
        let masked = sodam_core::signers::mask_token(&service.token);
        let latency = match root.signer_latency.get(service.url.trim()) {
            Some(None) => root.tr("测速中…").to_string(),
            Some(Some(Ok(ms))) => root.localized("{} ms", &[ms.to_string()]),
            Some(Some(Err(_))) => root.tr("失败").to_string(),
            None => String::new(),
        };

        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(theme::space::SM))
            .px(px(theme::space::MD))
            .py(px(theme::space::SM))
            .rounded(px(theme::radius::ROW))
            .bg(theme::surface())
            // 左：展示名（激活项用主文本色）、徽章、token 掩码；第二行完整地址。
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w(px(0.0))
                    .flex_1()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(theme::space::SM))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(theme::Text::Body.size())
                                    .text_color(if is_active {
                                        theme::text()
                                    } else {
                                        theme::text_muted()
                                    })
                                    .child(display.clone()),
                            )
                            .when(is_active, |this| this.child(badge_pill(root.tr("当前"))))
                            .when(is_builtin, |this| this.child(badge_pill(root.tr("内置"))))
                            .when(!masked.is_empty(), |this| {
                                this.child(
                                    div()
                                        .flex_none()
                                        .text_size(theme::Text::Tiny.size())
                                        .text_color(theme::text_faint())
                                        .child(format!("Token {masked}")),
                                )
                            }),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(theme::Text::Tiny.size())
                            .text_color(theme::text_faint())
                            // 展示脱敏端点（PR #348 二轮审查，安全项）：去掉
                            // userinfo 与 query，这两段可能携带凭据，不上屏。
                            .child(sodam_core::signers::display_endpoint(&service.url)),
                    ),
            );
        if !latency.is_empty() {
            row = row.child(
                div()
                    .flex_none()
                    .text_size(theme::Text::Small.size())
                    .text_color(theme::text_muted())
                    .child(latency),
            );
        }
        if !is_active {
            row = row.child(
                signer_button(("signer-activate", index), root.tr("设为当前").to_string())
                    .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                        // 点击时按地址解析（PR #348 二轮审查）：不捕获渲染时下标，
                        // 配置在两次绘制之间被外部重载也不会点错服务。
                        root.set_active_signer(url_for_activate.clone(), cx);
                    })),
            );
        }
        row = row.child(
            signer_button(("signer-test", index), root.tr("测速").to_string()).on_click(
                cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                    root.measure_signer_latency(service.url.clone(), cx);
                }),
            ),
        );
        if !is_builtin {
            row = row.child(
                signer_button(("signer-remove", index), root.tr("删除").to_string()).on_click(
                    cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                        root.remove_signer(url_for_remove.clone(), cx);
                    }),
                ),
            );
        }
        panel = panel.child(row);
    }

    panel = panel
        .child(row_note(root.tr("内置公共签名服务不可删除").to_string()))
        .child(signer_input_box(
            "signer-url-input",
            root.tr("服务地址").to_string(),
            &root.signer_url_field,
            &root.signer_url_focus,
            &root.signer_url_bounds,
            false,
            window,
            cx,
        ))
        .child(signer_input_box(
            "signer-token-input",
            root.tr("Token（可选，只存本机配置）").to_string(),
            &root.signer_token_field,
            &root.signer_token_focus,
            &root.signer_token_bounds,
            true,
            window,
            cx,
        ))
        .child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(theme::space::SM))
                .child(
                    signer_button("signer-add", root.tr("添加").to_string()).on_click(cx.listener(
                        |root, _event: &ClickEvent, _window, cx| {
                            root.submit_signer_form(cx);
                        },
                    )),
                )
                .child(
                    signer_button(
                        "signer-restore-builtin",
                        root.tr("恢复内置公共签名").to_string(),
                    )
                    .on_click(cx.listener(
                        |root, _event: &ClickEvent, _window, cx| {
                            root.restore_builtin_signer(cx);
                        },
                    )),
                ),
        );

    panel.into_any_element()
}

/// 单行输入框（#321 表单）：搜索框同款交互的简化版——点击聚焦并把光标放到末尾，
/// 键盘编辑按键与 IME 走全局统一分发（`app_text` / `InputHandler` 按焦点路由）。
/// `mask=true`（token）时显示用掩码串：真实值不上屏，光标与选区映射到**掩码串**
/// 下标测量绘制（见 `masked_display_index`）；文本超宽时带水平滚动偏移，尾部与
/// 光标保持可见；提交时才读出真实值写入本地配置。
#[allow(clippy::too_many_arguments)]
fn signer_input_box(
    id: &'static str,
    placeholder: String,
    field: &TextField,
    focus: &gpui::FocusHandle,
    bounds_cell: &std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    mask: bool,
    window: &Window,
    cx: &mut Context<Root>,
) -> AnyElement {
    let focused = focus.is_focused(window);
    let input_focus = focus.clone();
    let input_entity = cx.entity();
    let bounds_cell = bounds_cell.clone();
    let raw = field.text().to_string();
    let display = if mask {
        sodam_core::signers::mask_token(&raw)
    } else {
        raw.clone()
    };
    let empty = display.is_empty();
    let caret = field.caret();
    let selection = field.selection_range();
    // 测量与绘制用同一个坐标系（PR #348 审查意见）：token 掩码时光标/选区
    // 先映射到掩码串的下标再测量（掩码串只有 8 个字形，按原文测量会把光标
    // 画到圆点右边很远或被裁掉）；普通输入框直接按原文测量。
    let measure_line = if mask {
        None
    } else {
        shape_input_line(&raw, window, theme::text())
    };
    let paint_line = shape_input_line(&display, window, theme::text());
    let (caret_x, selection_edges) = if mask {
        let map = |raw_byte: usize| {
            let display_index = masked_display_index(&raw, &display, raw_byte);
            paint_line
                .as_ref()
                .map(|line| line.x_for_index(display_index))
                .unwrap_or_else(|| px(0.0))
        };
        let (lead_chars, hidden_chars, _trimmed) = mask_char_layout(&raw);
        // 选区两端映射到掩码串后收缩成同一个点、且整个选区都落在隐藏前缀里
        // （PR #348 三轮审查）：展开成整段圆点的高亮——零宽高亮看起来是
        // 「没选中」，Backspace 却仍会删字符。只高亮圆点段，绝不露出原文。
        let end_char = raw[..selection.end.min(raw.len())].chars().count();
        let collapsed_in_hidden = !selection.is_empty() && end_char <= lead_chars + hidden_chars;
        let selection_edges = if selection.is_empty() {
            None
        } else if collapsed_in_hidden {
            let dots_end_byte = masked_display_index(
                &raw,
                &display,
                raw.char_indices()
                    .nth(lead_chars + hidden_chars)
                    .map(|(byte, _)| byte)
                    .unwrap_or(raw.len()),
            );
            let dots_end_x = paint_line
                .as_ref()
                .map(|line| line.x_for_index(dots_end_byte))
                .unwrap_or_else(|| px(0.0));
            Some((px(0.0), dots_end_x))
        } else {
            Some((map(selection.start), map(selection.end)))
        };
        (map(caret.min(field.len())), selection_edges)
    } else {
        let measure = |raw_byte: usize| {
            measure_line
                .as_ref()
                .map(|line| line.x_for_index(raw_byte))
                .unwrap_or_else(|| px(0.0))
        };
        (
            measure(caret.min(field.len())),
            if selection.is_empty() {
                None
            } else {
                Some((measure(selection.start), measure(selection.end)))
            },
        )
    };
    // 文本宽度供绘制闭包算水平滚动偏移（超宽时把尾部滚进可视区）。
    let text_width = paint_line
        .as_ref()
        .map(|line| line.width())
        .unwrap_or(px(0.0));

    div()
        .id(id)
        .relative()
        .track_focus(focus)
        .key_context(crate::shortcuts::INPUT_CONTEXT)
        .flex()
        .flex_row()
        .items_center()
        .h(px(theme::size::CONTROL_SM))
        .px(px(theme::space::SM))
        .rounded(px(theme::radius::ROW))
        .bg(theme::surface())
        .border_1()
        .border_color(if focused {
            theme::accent()
        } else {
            theme::border()
        })
        .text_color(theme::text())
        .cursor_text()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |root, _event: &MouseDownEvent, window, cx| {
                let focus_handle = if mask {
                    &root.signer_token_focus
                } else {
                    &root.signer_url_focus
                };
                window.focus(focus_handle, cx);
                // 简化交互：点击聚焦并把光标放到末尾（URL / token 都是整段输入）。
                if mask {
                    root.signer_token_field
                        .set_caret(root.signer_token_field.len());
                } else {
                    root.signer_url_field.set_caret(root.signer_url_field.len());
                }
                cx.notify();
            }),
        )
        .on_action(cx.listener(Root::text_backspace))
        .on_action(cx.listener(Root::text_delete))
        .on_action(cx.listener(Root::text_delete_word_backward))
        .on_action(cx.listener(Root::text_left))
        .on_action(cx.listener(Root::text_right))
        .on_action(cx.listener(Root::text_word_left))
        .on_action(cx.listener(Root::text_word_right))
        .on_action(cx.listener(Root::text_select_left))
        .on_action(cx.listener(Root::text_select_right))
        .on_action(cx.listener(Root::text_select_home))
        .on_action(cx.listener(Root::text_select_end))
        .on_action(cx.listener(Root::text_select_word_left))
        .on_action(cx.listener(Root::text_select_word_right))
        .on_action(cx.listener(Root::text_select_all))
        .on_action(cx.listener(Root::text_home))
        .on_action(cx.listener(Root::text_end))
        // token 字段禁用复制/剪切（PR #348 二轮审查，安全项）：掩码只改显示，
        // 共享的 copy/cut 会把明文 token 写进系统剪贴板，其他应用与剪贴板
        // 历史都能读到。Paste / Delete 不受影响，URL 字段的剪贴板动作保留。
        .when(!mask, |this| {
            this.on_action(cx.listener(Root::text_copy))
                .on_action(cx.listener(Root::text_cut))
        })
        .on_action(cx.listener(Root::text_paste))
        .on_action(cx.listener(Root::text_undo))
        .on_action(cx.listener(Root::text_redo))
        .on_action(cx.listener(Root::text_submit))
        .on_action(cx.listener(Root::text_cancel))
        .child(
            div()
                .relative()
                .flex()
                .items_center()
                .flex_1()
                .min_w(px(0.0))
                // 显式定高：容器里只有 absolute 的 canvas（无行盒高度），不显式
                // 定高整盒会塌成 0，canvas 拿到的 bounds 高度为 0、文字画到
                // 错误的位置（对照搜索框同款处理）。
                .h(px(theme::size::CONTROL_SM))
                .overflow_hidden()
                .when(empty && !focused, |this| {
                    this.child(
                        div()
                            .text_size(theme::Text::Small.size())
                            .text_color(theme::text_faint())
                            .child(placeholder),
                    )
                })
                .child(
                    canvas(
                        {
                            let bounds_cell = bounds_cell.clone();
                            move |bounds, _window, _cx| {
                                bounds_cell.set(Some(bounds));
                                bounds
                            }
                        },
                        move |bounds, _state, window, cx| {
                            let line_height = window.line_height();
                            let top = bounds.origin.y
                                + px((f32::from(bounds.size.height) - f32::from(line_height))
                                    / 2.0);
                            // 水平滚动偏移（PR #348 审查意见）：文本比输入框宽时，
                            // 文本、选区、光标一起平移，光标始终可见。每帧由当前
                            // 光标位置现算，无需额外状态：光标滚出右边→贴右沿，
                            // 再钳到文本实际宽度内。可见性与上限都算上 CARET_WIDTH
                            // （PR #348 二轮审查）：否则滚到最右时光标正好压在裁剪
                            // 线上整个被裁掉，看起来「末尾没有光标」（同搜索框）。
                            let available = f32::from(bounds.size.width);
                            let text_w = f32::from(text_width);
                            let caret_f = f32::from(caret_x);
                            let caret_w = f32::from(crate::app::CARET_WIDTH);
                            let max_offset = (text_w + caret_w - available).max(0.0);
                            let mut offset = 0.0f32;
                            if caret_f + caret_w - offset > available {
                                offset = caret_f + caret_w - available;
                            }
                            offset = offset.clamp(0.0, max_offset);
                            let offset = px(offset);
                            if let Some((start, end)) = selection_edges {
                                let quad = gpui::fill(
                                    gpui::Bounds::from_corners(
                                        gpui::point(
                                            bounds.origin.x + start - offset,
                                            bounds.origin.y,
                                        ),
                                        gpui::point(
                                            bounds.origin.x + end - offset,
                                            bounds.origin.y + bounds.size.height,
                                        ),
                                    ),
                                    theme::surface_selected(),
                                );
                                window.paint_quad(quad);
                            }
                            if let Some(line) = &paint_line {
                                let _ = line.paint(
                                    gpui::point(bounds.origin.x - offset, bounds.origin.y),
                                    line_height,
                                    gpui::TextAlign::Left,
                                    None,
                                    window,
                                    cx,
                                );
                            }
                            if focused {
                                let caret = gpui::fill(
                                    gpui::Bounds::new(
                                        gpui::point(bounds.origin.x + caret_x - offset, top),
                                        gpui::size(crate::app::CARET_WIDTH, line_height),
                                    ),
                                    theme::accent(),
                                );
                                window.paint_quad(caret);
                            }
                            window.handle_input(
                                &input_focus,
                                gpui::ElementInputHandler::new(bounds, input_entity.clone()),
                                cx,
                            );
                        },
                    )
                    .absolute()
                    .size_full(),
                ),
        )
        .into_any_element()
}

/// 按窗口当前 text style 把一行文本 shape 出来；空串返回 None（画占位即可）。
fn shape_input_line(text: &str, window: &Window, color: gpui::Rgba) -> Option<gpui::ShapedLine> {
    if text.is_empty() {
        return None;
    }
    let style = window.text_style();
    let run = gpui::TextRun {
        len: text.len(),
        font: style.font(),
        color: gpui::Hsla::from(color),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    Some(window.text_system().shape_line(
        text.into(),
        style.font_size.to_pixels(window.rem_size()),
        &[run],
        None,
    ))
}

/// 原文字节下标 → 掩码显示串字节下标（PR #348 审查意见）：token 输入框画的是
/// `mask_token` 的输出（圆点段 + 末四位），光标与选区必须落在画出来的字形上。
/// 映射在 **trim 后的原文**上做（PR #348 二轮审查：掩码串由 trim 后的 token
/// 生成，`abcde ` 的光标在 `e` 之后要画在 `e` 之后，不能落进 `d` 里）；落在
/// 首尾空白上的下标映射到对应的显示端。规则：下标 0 在圆点段之前；隐藏区间
/// 内的下标吸附到圆点段末尾（选区罩住隐藏部分时不会出现没有字形的空白高亮）；
/// 露出段按字符平移；越界钳到显示串末尾。
/// 掩码布局（PR #348 三轮审查）：`(前导空白字符数, 隐藏字符数, trim 段字符数)`。
/// 掩码串由 trim 后的 token 生成，光标/选区换算都要在这套坐标里做。
fn mask_char_layout(raw: &str) -> (usize, usize, usize) {
    let lead_chars = raw[..raw.len() - raw.trim_start().len()].chars().count();
    let trimmed_chars = raw.trim().chars().count();
    let hidden = trimmed_chars.min(4);
    (lead_chars, trimmed_chars - hidden, trimmed_chars)
}

fn masked_display_index(raw: &str, display: &str, raw_byte: usize) -> usize {
    let raw_byte = raw_byte.min(raw.len());
    let char_index = raw[..raw_byte].chars().count();
    let (lead_chars, _hidden, trimmed_chars) = mask_char_layout(raw);
    let rel = char_index.saturating_sub(lead_chars).min(trimmed_chars);
    let visible = trimmed_chars.min(4);
    let hidden = trimmed_chars - visible;
    let display_char_count = display.chars().count();
    let dots = display_char_count.saturating_sub(visible);
    let target = if rel == 0 {
        0
    } else if rel <= hidden {
        dots
    } else {
        dots + (rel - hidden)
    };
    if target >= display_char_count {
        return display.len();
    }
    display
        .char_indices()
        .nth(target)
        .map(|(byte, _)| byte)
        .unwrap_or_else(|| display.len())
}

#[cfg(test)]
mod masked_input_tests {
    use super::masked_display_index;

    /// 映射规则：0 在圆点前；隐藏区间吸附圆点末尾；露出段平移；越界钳尾。
    #[test]
    fn maps_caret_into_masked_glyphs() {
        // "abcde" → "••••bcde"：隐藏 1 字符、露出 4 字符（按字符计，不含字节）。
        let raw = "abcde";
        let display = "••••bcde";
        assert_eq!(masked_display_index(raw, display, 0), 0);
        // 光标在 a 之后（隐藏区间）：吸附到圆点段末尾。
        assert_eq!(masked_display_index(raw, display, 1), "••••".len());
        // 光标在 c 之后（露出段第一个字符后）。
        assert_eq!(masked_display_index(raw, display, 2), "••••b".len());
        // 光标在末尾：钳到显示串末尾。
        assert_eq!(masked_display_index(raw, display, 5), display.len());
        // 越界字节下标同样钳尾。
        assert_eq!(masked_display_index(raw, display, 99), display.len());
    }

    /// `abcde `（尾随空格）：掩码按 trim 后的 token 生成，光标在 `e` 之后
    /// （字节 5）要画在 `e` 之后，而不是落进 `d` 里（PR #348 二轮审查）。
    #[test]
    fn maps_caret_against_trimmed_token() {
        let raw = "abcde ";
        let display = "••••bcde";
        // 光标在 `e` 之后、空格之前：显示串末尾。
        assert_eq!(masked_display_index(raw, display, 5), display.len());
        // 光标在最末尾（空格之后）：同样钳到显示端。
        assert_eq!(masked_display_index(raw, display, 6), display.len());
        // 前导空格：` abcde` 的光标在 `a` 之前（字节 1）映射到显示串开头。
        let raw = " abcde";
        assert_eq!(masked_display_index(raw, display, 0), 0);
        assert_eq!(masked_display_index(raw, display, 1), 0);
        assert_eq!(masked_display_index(raw, display, 2), "••••".len());
    }
}

//! 右侧内容区路由与共享列表组件。

use super::*;
use crate::app::SAVE_TEXT;
use crate::ui::i18n::Language;
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

pub(crate) fn settings_view(root: &mut Root, cx: &mut Context<Root>) -> AnyElement {
    // 档位与名称都从核心层派生（`TIERS` + `label_key`），这里只保留每档的说明文案：
    // 否则往 `TIERS` 加一档时，设置页会静默少一项。
    // 档位是“上限”而不是“目标”：libresoda 在没有任何候选低于上限时，会回退成全部候选
    //（实际可能比所选更高），所以这里说“优先 / 由服务端回退”，不承诺“只降一档”。
    let hint_key = |value: &str| -> &'static str {
        match value {
            "hires" => "优先 Hi-Res 无损；没有可用档位时由服务端回退",
            "lossless" => "优先无损；没有可用无损时由服务端回退",
            "dolby" => "优先杜比全景声；没有可用档位时由服务端回退",
            "highest" => "优先极高（≈320k）；没有可用档位时由服务端回退",
            "medium" => "优先较高；没有可用档位时由服务端回退",
            "low" => "优先流畅（≈128k）；没有可用档位时由服务端回退",
            _ => "不设上限，按账号权益和单曲可用档位取最高",
        }
    };
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
    let batch_upgrade_blocked_by_vip =
        batch_target.requires_membership() && root.account.as_ref().is_some_and(|a| !a.vip);
    // 勾选的是**意图**；配置里的空串是历史写法，按「自动」显示。
    let current = root.settings.quality_intent().as_setting().to_string();

    // 来源提示与徽章：区分用户手动选择、程序推导与未知来源。
    // 在全新未配置的「自动」项上（Unknown + Auto）不展示徽章，避免新用户误解。
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
            (sodam_core::quality::QualitySource::User, _) => (
                Some(root.tr("手动选择")),
                Some(root.tr("当前档位由手动选择")),
            ),
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
            let hint = root.tr(hint_key(value));
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
                        .child(
                            div()
                                .text_size(theme::Text::Tiny.size())
                                .text_color(theme::text_faint())
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

    let account_card = account_section(root, cx);

    div()
        .id("settings-scroll")
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        .gap(px(theme::space::LG))
        .overflow_y_scroll()
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
                        .child(
                            root.tr("自动 = 取当前账号能拿到的最高档，单曲没有该档位时自动降一档"),
                        ),
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
                                .child(root.tr("批量升级缓存")),
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
                            let estimate_text = if batch_upgrade_blocked_by_vip {
                                root.localized(
                                    "「{}」音质需 VIP 会员权限，当前账号无法升级",
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
                                    root.tr("所有缓存歌曲均已达到或超过该档位").to_string()
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
                    div()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(if root.language.resolved().is_zh() {
                            root.localized(
                                "签名服务：{}",
                                &[if root.settings.signer_url.trim().is_empty() {
                                    root.tr("（未配置）").to_string()
                                } else {
                                    root.settings.signer_url.trim().to_string()
                                }],
                            )
                        } else {
                            format!(
                                "Signer: {}",
                                if root.settings.signer_url.trim().is_empty() {
                                    root.tr("（未配置）")
                                } else {
                                    root.settings.signer_url.trim()
                                }
                            )
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
                                        .child(root.tr("Edit")),
                                ),
                        ),
                ),
        )
        .child({
            let repository = env!("CARGO_PKG_REPOSITORY");
            let contributors = root.contributors_image.clone();
            root.ensure_contributors(cx);
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
                                )
                                .child(
                                    div()
                                        .text_size(theme::Text::Tiny.size())
                                        .text_color(theme::text_faint())
                                        .child(format!(
                                            "{} · {}",
                                            root.tr("维护者"),
                                            "ZephyrCheung"
                                        )),
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
                                    root.ensure_contributors(cx);
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

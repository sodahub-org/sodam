//! 右侧内容区路由与共享列表组件。

use super::*;
use crate::ui::i18n::Language;
use sodam_core::cache::{CacheUsageSummary, MaintenanceMark};
use sodam_core::config::CloseAction;
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

/// 缓存用量那一行：读全了写「共」，读不全写「至少」，整棵树读不到的那段写「读不到」。
///
/// 只有一棵读不到、另一棵是空的时候没有总数可写，只报读得到的那棵。
fn cache_usage_line(language: Language, usage: CacheUsageSummary) -> String {
    let total = usage.total_bytes();
    let incomplete = usage.incomplete();
    let audio_hidden = usage.audio.unreadable();
    let covers_hidden = usage.covers.unreadable();
    if audio_hidden && covers_hidden {
        return language
            .text("暂时读不到缓存占用，稍后会自动重试")
            .to_string();
    }
    if audio_hidden {
        return if total == 0 {
            language.textf(
                "歌曲读不到（封面 {} 张 {}）",
                &[
                    usage.covers.files.to_string(),
                    human_bytes(usage.covers.bytes),
                ],
            )
        } else {
            language.textf(
                "至少 {}（歌曲读不到 / 封面 {} 张 {}）",
                &[
                    human_bytes(total),
                    usage.covers.files.to_string(),
                    human_bytes(usage.covers.bytes),
                ],
            )
        };
    }
    if covers_hidden {
        return if total == 0 {
            language.textf(
                "歌曲 {} 首 {}（封面读不到）",
                &[
                    usage.audio.songs.to_string(),
                    human_bytes(usage.audio.bytes),
                ],
            )
        } else {
            language.textf(
                "至少 {}（歌曲 {} 首 {} / 封面读不到）",
                &[
                    human_bytes(total),
                    usage.audio.songs.to_string(),
                    human_bytes(usage.audio.bytes),
                ],
            )
        };
    }
    let args = [
        human_bytes(total),
        usage.audio.songs.to_string(),
        human_bytes(usage.audio.bytes),
        usage.covers.files.to_string(),
        human_bytes(usage.covers.bytes),
    ];
    // key 用字面量：i18n 守卫只扫调用点上的字面量。
    if incomplete {
        language.textf("至少 {}（歌曲 {} 首 {} / 封面 {} 张 {}）", &args)
    } else {
        language.textf("共 {}（歌曲 {} 首 {} / 封面 {} 张 {}）", &args)
    }
}

/// 读不全时的解释：数字可能偏小、稍后会自动重试。
///
/// 整棵树读不到时数字行已经写了「读不到」，这里不再重复；不写具体原因，代码只知道
/// 有地方没读到，分不清是被别的程序占着还是没权限。
fn cache_usage_hint(language: Language, usage: CacheUsageSummary) -> Option<&'static str> {
    usage_hint_shown(usage)
        .then(|| language.text("有缓存文件暂时读不到，数字可能偏小；稍后会自动重试"))
}

/// 数字行下面那句会不会出现：读不全，但两棵树都读到了数。
fn usage_hint_shown(usage: CacheUsageSummary) -> bool {
    usage.incomplete() && !usage.audio.unreadable() && !usage.covers.unreadable()
}

/// 上限旁边那句「没删干净」：音频树读不全就提醒上限可能没生效，否则看是不是真删不动。
///
/// 两种提示都只在现在真超限时才出现，读不全只看音频这棵树，封面读不全不影响音频上限。
/// 读不全这条不等维护结论：数字最可疑的时候，恰好可能是维护正在跑、旧结论被收起的时候。
/// 删不动那条要上一轮和现在都超着才说，用户刚清过缓存时旧结论已经不作数。
/// 「稍后会自动重试」交给数字行下面那句；那句不出现时，这条自己补上。
fn cache_blocked_hint(
    language: Language,
    mark: Option<&MaintenanceMark>,
    usage: CacheUsageSummary,
    audio_limit_bytes: u64,
) -> Option<&'static str> {
    let over = usage.audio.bytes > audio_limit_bytes;
    if usage.audio.incomplete {
        if !over {
            return None;
        }
        // 数字行下面那句没出现时，重试承诺由这里补上。
        return Some(if usage_hint_shown(usage) {
            language.text("上限可能暂时没生效")
        } else {
            language.text("上限可能暂时没生效；稍后会自动重试")
        });
    }
    let mark = mark?;
    if mark.audio_bytes <= audio_limit_bytes || !over {
        return None;
    }
    Some(if usage_hint_shown(usage) {
        language.text("上次清理没能删到上限以下：有文件正在使用或暂时删不掉")
    } else {
        language.text("上次清理没能删到上限以下：有文件正在使用或暂时删不掉，稍后会自动重试")
    })
}

/// 设置页的浅色小字提示：解释数字、原因或删不动的那几句共用一层样式。
fn cache_hint_line(text: impl IntoElement) -> impl IntoElement {
    div()
        .text_size(theme::Text::Tiny.size())
        .text_color(theme::text_faint())
        .child(text)
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

/// 设置页里的一行「单选」：标题 + 说明 + 选中勾。
///
/// 窗口按钮与关闭行为两处共用；`enabled = false` 时置灰且不可点。
fn choice_row(
    id: String,
    title: &'static str,
    hint: &'static str,
    chosen: bool,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let row = div()
        .id(gpui::ElementId::Name(id.into()))
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(theme::space::LG))
        .px(px(theme::space::MD))
        .py(px(theme::space::SM))
        .rounded(px(theme::radius::ROW))
        .when(chosen, |this| this.bg(theme::surface_selected()))
        .when(!enabled, |this| this.opacity(0.45))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(theme::space::XS))
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
        });
    if enabled {
        row.cursor_pointer()
            .hover(|style| style.bg(theme::surface_hover()))
            .on_click(on_click)
            .into_any_element()
    } else {
        row.into_any_element()
    }
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
        choice_row(
            format!("titlebar-set-{}", mode.key()),
            title,
            hint,
            mode == current,
            true,
            cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                root.settings.titlebar = mode.key().to_string();
                root.save_settings();
                let status = match mode {
                    TitlebarMode::Auto => root.tr("窗口按钮：自动（跟随桌面）").to_string(),
                    TitlebarMode::Always => root.tr("窗口按钮：总是显示").to_string(),
                    TitlebarMode::Never => root.tr("窗口按钮：不显示").to_string(),
                };
                root.set_status_text(status);
                cx.notify();
            }),
        )
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

/// 关闭主窗口时的行为：退出 / 收进托盘 / 每次询问。
///
/// 三端共用。没有托盘宿主时「隐藏到托盘」直接置灰——收进一个不存在的托盘，
/// 正是让用户找不到窗口的做法（#324）。
fn close_behavior_section(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let tray_available = root.tray_available_now();
    let effective = CloseAction::decide(&root.settings.close_action, tray_available);
    let rows: Vec<AnyElement> = [
        (
            CloseAction::Quit,
            root.tr("退出应用"),
            root.tr("直接结束进程，不再后台播放"),
            true,
        ),
        (
            CloseAction::Hide,
            root.tr("隐藏到托盘继续播放"),
            if tray_available {
                root.tr("收进系统托盘，音乐继续播放")
            } else {
                root.tr("当前桌面没有系统托盘，这项不可用")
            },
            tray_available,
        ),
        (
            CloseAction::Ask,
            root.tr("每次询问"),
            root.tr("每次关闭都弹窗让你选"),
            true,
        ),
    ]
    .into_iter()
    .map(|(action, title, hint, enabled)| {
        choice_row(
            format!("close-action-{}", action.key()),
            title,
            hint,
            effective == action && enabled,
            enabled,
            cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                root.settings.close_action = action.key().to_string();
                root.save_settings();
                let status = match action {
                    CloseAction::Quit => root.tr("关闭按钮：退出应用").to_string(),
                    CloseAction::Hide => root.tr("关闭按钮：隐藏到托盘").to_string(),
                    CloseAction::Ask => root.tr("关闭按钮：每次询问").to_string(),
                };
                root.set_status_text(status);
                cx.notify();
            }),
        )
    })
    .collect();

    div()
        .flex()
        .flex_col()
        .gap(px(theme::space::XS))
        .child(
            div()
                .text_size(theme::Text::Large.size())
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(root.tr("关闭主窗口时")),
        )
        .child(
            div()
                .text_size(theme::Text::Small.size())
                .text_color(theme::text_muted())
                .child(if tray_available {
                    root.tr("点关闭按钮时怎么处理；托盘菜单里也能退出应用")
                } else {
                    root.tr("当前桌面没有系统托盘，关闭窗口会直接退出应用")
                }),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(theme::space::XS))
                .children(rows),
        )
        .into_any_element()
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
                                    root.save_settings();
                                    let status = if root.language.is_zh() {
                                        root.tr("语言已恢复为跟随系统").to_string()
                                    } else {
                                        "Language restored to system".to_string()
                                    };
                                    root.set_status_text(status);
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
        .child(close_behavior_section(root, cx))
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
            let usage = root.cache_summary;
            let limit_bytes = root.settings.audio_cache_limit_mb * 1024 * 1024;
            let usage_hint = cache_usage_hint(root.language, usage);
            // 维护正在跑、或强制请求还排着队时，旧结论已经不作数，别拿它报「上次没删动」。
            let mark = if root.cache_maintenance_running || root.cache_maintenance_pending {
                None
            } else {
                root.cache_mark.as_ref()
            };
            let blocked_hint = cache_blocked_hint(root.language, mark, usage, limit_bytes);
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
                        .child(cache_usage_line(root.language, usage)),
                )
                .children(usage_hint.map(cache_hint_line))
                .child(
                    // 合计里含封面，上限却只管音频：写在同一行里说清楚，别让用户以为上限坏了。
                    div()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(root.localized(
                            "音频缓存上限：{}（合计还含封面，封面另有 {} 上限）",
                            &[
                                human_bytes(limit_bytes),
                                human_bytes(sodam_core::cache::COVER_LIMIT_MB * 1024 * 1024),
                            ],
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
                .children(blocked_hint.map(cache_hint_line))
                .child(
                    div()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(root.tr("自动清理较早使用的歌曲；占用偶尔会暂时超过上限")),
                )
                .child({
                    // 灰态仍接收点击，让 clear_cache 说明为什么现在不能清。
                    let mut btn = div()
                        .id("clear-cache")
                        .flex()
                        .items_center()
                        .justify_center()
                        .h(px(theme::size::CONTROL))
                        .w(px(140.0))
                        .rounded(px(theme::radius::ROW))
                        .text_size(theme::Text::Small.size())
                        .bg(theme::surface_elevated())
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::surface_hover()))
                        .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                            root.clear_cache(cx);
                        }));
                    if root.batch_upgrading && !root.batch_upgrade_cancel {
                        btn = btn.text_color(theme::text_faint());
                    } else {
                        btn = btn.text_color(theme::text());
                    }
                    btn.child(if root.cache_clearing {
                        root.tr("正在清理…")
                    } else {
                        root.tr("清空缓存")
                    })
                })
                .child(cache_hint_line(root.tr(
                    "正在播放或下载的文件会保留，等它们停用后再清一次",
                )))
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
                            // 档位未知的旧缓存另起一行：这几首不是「已知比目标低」，是旧版本
                            // 没记档位。被权益挡住、或这一轮全都抑制掉了，也要照样报出来——
                            // 「没有可升级的歌曲」在这种存量下就不是实话：这几首会被当成不存在。
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
                                    root.tr(
                                        "没有可升级的歌曲：已达目标、30 天后会自动重探该档位，或当前缓存条件不支持升级",
                                    )
                                    .to_string()
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

                            if root.batch_upgrading && root.batch_upgrade_cancel {
                                // 已经点过取消，等正在下的那一首跑完：按钮不再响应，
                                // 文案说清在等什么，别让第二下点了没反应。
                                btn = btn
                                    .bg(theme::surface_elevated())
                                    .text_color(theme::text_faint());
                            } else if root.batch_upgrading {
                                // 升级中变「取消」：只停在下一首之前，正在下的那一首不打断。
                                btn = btn
                                    .bg(theme::surface_elevated())
                                    .text_color(theme::text())
                                    .cursor_pointer()
                                    .hover(|style| style.bg(theme::surface_hover()))
                                    .on_click(cx.listener(
                                        |root, _event: &ClickEvent, _window, cx| {
                                            root.cancel_batch_upgrade(cx);
                                        },
                                    ));
                            } else if can_upgrade {
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
                                if root.batch_upgrade_cancel {
                                    root.tr("正在停止…")
                                } else {
                                    root.tr("取消")
                                }
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
        // 设置页最下方的退出入口：没有托盘的桌面上，这里是唯一的退出口（#324）。
        .child(
            div()
                .id("quit-app")
                .flex()
                .flex_row()
                .items_center()
                .justify_center()
                .h(px(theme::size::CONTROL))
                .rounded(px(theme::radius::PILL))
                .bg(theme::surface_elevated())
                .border_1()
                .border_color(theme::border())
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_hover()))
                .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                    // 走退出闸门：先等设置写队列刷完，再真正退出（#198）。
                    root.request_quit(cx);
                }))
                .child(
                    div()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(root.tr("退出应用")),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sodam_core::cache::CacheUsage;

    fn usage(bytes: u64, files: usize, songs: usize, incomplete: bool) -> CacheUsage {
        CacheUsage {
            bytes,
            files,
            songs,
            incomplete,
        }
    }

    fn both(audio: CacheUsage, covers: CacheUsage) -> CacheUsageSummary {
        CacheUsageSummary { audio, covers }
    }

    #[test]
    fn cache_usage_line_says_at_least_when_a_tree_could_not_be_read() {
        let totals = both(
            usage(2 * 1024 * 1024 * 1024, 10, 10, false),
            usage(1024 * 1024 * 1024, 20, 0, true),
        );

        let text = cache_usage_line(Language::Chinese, totals);
        assert!(
            text.starts_with("至少 3.00 GB"),
            "读不全时不能写成准确值：{text}"
        );
        assert!(text.contains("封面 20 张"), "{text}");

        let english = cache_usage_line(Language::English, totals);
        assert!(english.starts_with("At least "), "{english}");
    }

    #[test]
    fn cache_usage_line_is_exact_when_everything_was_read() {
        let totals = both(
            usage(2 * 1024 * 1024, 10, 3, false),
            usage(1024 * 1024, 2, 0, false),
        );

        let text = cache_usage_line(Language::Chinese, totals);
        assert!(text.starts_with("共 3.0 MB"), "{text}");
        assert!(text.contains("歌曲 3 首"), "{text}");
    }

    #[test]
    fn cache_usage_line_never_says_at_least_zero() {
        let nothing = usage(0, 0, 0, true);
        let text = cache_usage_line(Language::Chinese, both(nothing, nothing));
        assert_eq!(text, "暂时读不到缓存占用，稍后会自动重试");
        assert!(!text.contains("至少 0 B"));
    }

    #[test]
    fn cache_usage_line_says_unreadable_for_a_tree_that_read_nothing() {
        let audio_hidden = both(
            usage(0, 0, 0, true),
            usage(3 * 1024 * 1024 * 1024, 3, 0, false),
        );
        let text = cache_usage_line(Language::Chinese, audio_hidden);
        assert!(text.starts_with("至少 3.00 GB"), "{text}");
        assert!(text.contains("歌曲读不到"), "{text}");
        assert!(!text.contains("0 B"), "读不到的树不能写成 0 B：{text}");

        let covers_hidden = both(usage(2 * 1024 * 1024, 10, 3, false), usage(0, 0, 0, true));
        let text = cache_usage_line(Language::Chinese, covers_hidden);
        assert!(text.contains("封面读不到"), "{text}");
        assert!(!text.contains("0 B"), "读不到的树不能写成 0 B：{text}");

        // 另一棵是空的时候没有总数可写：报读得到的那棵，别整行说读不到。
        let audio_hidden_empty_covers = both(usage(0, 0, 0, true), usage(0, 0, 0, false));
        let text = cache_usage_line(Language::Chinese, audio_hidden_empty_covers);
        assert_eq!(text, "歌曲读不到（封面 0 张 0 B）", "{text}");
        assert!(!text.contains("至少"), "{text}");

        let covers_hidden_empty_audio = both(usage(0, 0, 0, false), usage(0, 0, 0, true));
        let text = cache_usage_line(Language::Chinese, covers_hidden_empty_audio);
        assert_eq!(text, "歌曲 0 首 0 B（封面读不到）", "{text}");
        assert!(!text.contains("至少"), "{text}");

        let english = cache_usage_line(Language::English, audio_hidden);
        assert!(english.contains("can't be read"), "{english}");
    }

    #[test]
    fn cache_usage_hint_only_explains_the_low_numbers() {
        let read = usage(1024, 1, 1, false);
        // 读到了数，但这一轮没读全。
        let partial = usage(2048, 2, 2, true);
        // 整棵树一个文件都没读到。
        let hidden = usage(0, 0, 0, true);

        let hint = cache_usage_hint(Language::Chinese, both(read, partial)).unwrap();
        assert!(hint.contains("数字可能偏小"), "{hint}");
        assert!(!hint.contains("上限"), "上限那句在上限旁边那一行：{hint}");

        assert!(
            cache_usage_hint(Language::Chinese, both(hidden, partial)).is_none(),
            "数字行已经写了「读不到」，别再说一遍"
        );
        assert!(cache_usage_hint(Language::Chinese, both(hidden, hidden)).is_none());
        assert!(cache_usage_hint(Language::Chinese, both(read, read)).is_none());
    }

    #[test]
    fn cache_blocked_hint_says_the_limit_may_not_apply_when_the_audio_tree_is_incomplete() {
        let mark = MaintenanceMark {
            completed_at: std::time::SystemTime::UNIX_EPOCH,
            audio_bytes: 0,
            cover_bytes: 0,
            incomplete: false,
        };

        let over_unreadable = both(usage(8192, 3, 3, true), usage(2048, 2, 2, true));
        assert_eq!(
            cache_blocked_hint(Language::Chinese, Some(&mark), over_unreadable, 4096),
            Some("上限可能暂时没生效"),
            "数字行下面那句会出现，重试承诺由它说"
        );

        let small_unreadable = both(usage(1024, 2, 2, true), usage(2048, 2, 2, true));
        assert!(
            cache_blocked_hint(Language::Chinese, Some(&mark), small_unreadable, 4096).is_none(),
            "读不全但没超限时，别凭一条 incomplete 就说上限没生效"
        );

        // 只有封面读不全：音频自己读全了，超限就该走「删不动」，不能说上限没生效。
        let over_with_cover_gap = both(usage(8192, 3, 3, false), usage(0, 0, 0, true));
        let over_mark = MaintenanceMark {
            audio_bytes: 8192,
            ..mark
        };
        assert!(
            cache_blocked_hint(
                Language::Chinese,
                Some(&over_mark),
                over_with_cover_gap,
                4096
            )
            .is_some_and(|hint| hint.contains("上次清理没能删到上限以下")),
            "音频读全了、上一轮也超着，就该说删不动"
        );
        assert!(
            cache_blocked_hint(Language::Chinese, Some(&mark), over_with_cover_gap, 4096).is_none(),
            "封面读不全不影响音频上限的判断"
        );

        let nothing = both(usage(0, 0, 0, true), usage(0, 0, 0, true));
        assert!(
            cache_blocked_hint(Language::Chinese, Some(&mark), nothing, 4096).is_none(),
            "一个字节都没读到时，数字那行已经说明读不到"
        );
        assert_eq!(
            cache_blocked_hint(Language::Chinese, None, over_unreadable, 4096),
            Some("上限可能暂时没生效"),
            "读不全这条不等维护结论：维护正在跑的时候也要提示"
        );

        // 封面整棵读不到时，数字行下面那句被收起：重试承诺由上限这条自己补上。
        let over_with_cover_hidden = both(usage(8192, 3, 3, true), usage(0, 0, 0, true));
        assert_eq!(
            cache_blocked_hint(Language::Chinese, Some(&mark), over_with_cover_hidden, 4096),
            Some("上限可能暂时没生效；稍后会自动重试")
        );
        assert!(cache_usage_hint(Language::Chinese, over_with_cover_hidden).is_none());
    }

    #[test]
    fn cache_blocked_hint_needs_both_the_last_round_and_now_over_limit() {
        let over = both(usage(2048, 1, 1, false), usage(0, 0, 0, false));
        let cleared = both(usage(0, 0, 0, false), usage(0, 0, 0, false));
        let mark = MaintenanceMark {
            completed_at: std::time::SystemTime::UNIX_EPOCH,
            audio_bytes: 2048,
            cover_bytes: 0,
            incomplete: false,
        };

        assert_eq!(
            cache_blocked_hint(Language::Chinese, Some(&mark), over, 1024),
            Some("上次清理没能删到上限以下：有文件正在使用或暂时删不掉，稍后会自动重试"),
            "数字行下面那句没出现，重试承诺由这条自己说"
        );

        // 封面读不全但读到了数：数字行下面那句会出现，这条不重复重试承诺。
        let over_with_cover_gap = both(usage(2048, 1, 1, false), usage(512, 2, 0, true));
        assert_eq!(
            cache_blocked_hint(Language::Chinese, Some(&mark), over_with_cover_gap, 1024),
            Some("上次清理没能删到上限以下：有文件正在使用或暂时删不掉")
        );
        assert!(
            cache_blocked_hint(Language::Chinese, Some(&mark), cleared, 1024).is_none(),
            "用户刚清过缓存，别再报上次删不动"
        );
        assert!(cache_blocked_hint(Language::Chinese, Some(&mark), over, 4096).is_none());
        assert!(cache_blocked_hint(Language::Chinese, None, over, 1024).is_none());
    }
}

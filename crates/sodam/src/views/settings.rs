//! 右侧内容区路由与共享列表组件。

use super::*;
use crate::ui::i18n::Language;

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

pub(crate) fn settings_view(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let options = [
        (
            "auto",
            root.tr("自动"),
            root.tr("按账号权益和单曲实际可用的档位择优"),
        ),
        (
            "lossless",
            root.tr("无损"),
            root.tr("封顶无损；单曲没有无损时自动降级"),
        ),
        ("highest", root.tr("极高"), root.tr("封顶极高（≈320k）")),
        ("medium", root.tr("较高"), root.tr("封顶较高")),
        ("low", root.tr("标准"), root.tr("省流")),
    ];
    let current = {
        let value = root.settings.quality.trim();
        if value.is_empty() {
            "auto"
        } else {
            value
        }
    };
    let vip = root.account.as_ref().map(|info| info.vip).unwrap_or(false);

    let theme_rows: Vec<AnyElement> = [
        (ThemeKind::Dark, root.tr("深色")),
        (ThemeKind::Light, root.tr("浅色")),
    ]
    .into_iter()
    .map(|(value, title)| {
        let chosen = root.theme == value;
        div()
            .id(gpui::ElementId::Name(
                format!("theme-set-{}", value.key()).into(),
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
                root.set_ui_theme(value, cx);
            }))
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
                            .bg(if value == ThemeKind::Light {
                                theme::hex(0xFFFFFF)
                            } else {
                                theme::hex(0x0A0A0A)
                            }),
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
        .map(|(value, title, hint)| {
            let chosen = *value == current;
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
                                .text_size(theme::Text::Body.size())
                                .text_color(if chosen {
                                    theme::text()
                                } else {
                                    theme::text_muted()
                                })
                                .child(title.to_string()),
                        )
                        .child(
                            div()
                                .text_size(theme::Text::Tiny.size())
                                .text_color(theme::text_faint())
                                .child(hint.to_string()),
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
                                    let _ = root.settings.save();
                                    root.status = if root.language.is_zh() {
                                        root.tr("语言已恢复为跟随系统").to_string()
                                    } else {
                                        "Language restored to system".to_string()
                                    };
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
                        .child(if vip {
                            root.tr("当前账号是 VIP：登录时默认无损，单曲没有无损会自动降级")
                                .to_string()
                        } else {
                            root.tr("当前账号非 VIP：默认自动，取免费档里实际可用的最高一档")
                                .to_string()
                        }),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(theme::space::XS))
                .children(rows),
        )
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
                            let removed = sodam_core::audio::clear_cache();
                            root.status =
                                root.localized("已清理缓存：{} 个文件", &[removed.to_string()]);
                            // 清理后立即刷新统计（后台扫盘）
                            root.refresh_cache_stats(cx);
                            cx.notify();
                        }))
                        .child(root.tr("清除歌曲缓存")),
                )
        })
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
            div()
                .id("about-footer")
                .flex()
                .flex_row()
                .items_center()
                .gap(px(theme::space::MD))
                .px(px(theme::space::MD))
                .py(px(theme::space::SM))
                .rounded(px(theme::radius::CARD))
                .bg(theme::surface_elevated())
                .border_1()
                .border_color(theme::border())
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_hover()))
                .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                    root.open_github_repository(cx);
                }))
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
                                .child("Author: ZephyrCheung"),
                        ),
                )
                .child(
                    svg()
                        .path(icons::path("chevron-right"))
                        .size(px(theme::ICON_SM))
                        .text_color(theme::text_faint()),
                )
        })
        .into_any_element()
}

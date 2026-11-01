//! 底部播放条 + 右侧队列抽屉。
//!
//! 布局（参考官方客户端）：左「封面 + 标题/歌手」、中「上一首 / 播放 / 下一首 + 进度」、
//! 右「音量图标 + 内联音量滑条 + 队列开关」。音质徽章在左侧曲目信息里，点开选意图。

use gpui::prelude::*;
use gpui::{
    canvas, div, px, svg, uniform_list, Animation, AnimationExt, ClickEvent, Context, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent,
};

use crate::app::Root;
use crate::ui::artwork::{cover, icon_button};
use crate::ui::{icons, theme};

/// 内联音量条宽度与滑块直径。
/// 档位菜单宽度：最宽的一项是「自动」加它的说明，200 足够且不挤压播放条。
const QUALITY_MENU_W: f32 = 200.0;


/// 官方音量弹层：点音量图标后在上方弹出的竖向滑条。
fn volume_popover(root: &Root, cx: &mut Context<Root>) -> impl IntoElement {
    let volume = root.settings.volume;
    let slot = root.volume_pop_bounds.clone();
    let apply = move |y: f32, root: &mut Root, cx: &mut Context<Root>| {
        let bounds = slot.lock().ok().and_then(|value| *value);
        let Some(bounds) = bounds else { return };
        let top = f32::from(bounds.origin.y);
        let height = f32::from(bounds.size.height);
        if height <= 1.0 {
            return;
        }
        let ratio = ((top + height - y) / height).clamp(0.0, 1.0);
        root.set_volume(ratio);
        cx.notify();
    };
    let apply_down = apply.clone();
    let apply_move = apply.clone();
    let slot_canvas = root.volume_pop_bounds.clone();

    div()
        .id("volume-popover")
        .on_hover(cx.listener(|root, hovered: &bool, _window, cx| {
            root.volume_inside = *hovered;
            if *hovered {
                root.volume_open = true;
            }
            cx.notify();
        }))
        .absolute()
        .left(gpui::relative(0.5))
        .ml(px(-20.0))
        .bottom(px(40.0))
        .w(px(40.0))
        .h(px(140.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(12.0))
        .bg(theme::surface_elevated())
        .border_1()
        .border_color(theme::border())
        .shadow_lg()
        .child(
            div()
                .relative()
                .w(px(4.0))
                .h(px(96.0))
                .rounded_full()
                .bg(theme::surface_hover())
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |root, event: &MouseDownEvent, _window, cx| {
                        apply_down(f32::from(event.position.y), root, cx);
                    }),
                )
                .on_mouse_move(cx.listener(move |root, event: &MouseMoveEvent, _window, cx| {
                    if event.dragging() {
                        apply_move(f32::from(event.position.y), root, cx);
                    }
                }))
                .child(
                    div()
                        .absolute()
                        .left(px(0.0))
                        .right(px(0.0))
                        .bottom(px(0.0))
                        .h(gpui::relative(volume))
                        .rounded_full()
                        .bg(theme::accent()),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(-4.0))
                        .bottom(gpui::relative(volume))
                        .mb(px(-5.0))
                        .size(px(12.0))
                        .rounded_full()
                        .bg(gpui::rgb(0xFFFFFF)),
                )
                .child(
                    canvas(
                        move |bounds: gpui::Bounds<gpui::Pixels>, _window, _cx| {
                            if let Ok(mut guard) = slot_canvas.lock() {
                                *guard = Some(bounds);
                            }
                        },
                        |_bounds, _state, _window, _cx| {},
                    )
                    .absolute()
                    .size_full(),
                ),
        )
}

/// 官方控制栏的宽按钮：前后各 64×48，播放键 84×48。
fn wide_transport_button(
    id: &'static str,
    icon: &'static str,
    width: f32,
    height: f32,
    icon_size: f32,
    cx: &mut Context<Root>,
    action: fn(&mut Root, &mut Context<Root>),
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .w(px(width))
        .h(px(height))
        .rounded(px(100.0))
        .cursor_pointer()
        .hover(|style| style.bg(gpui::Rgba { a: 0.1, ..theme::text() }))
        .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
            action(root, cx);
            cx.notify();
        }))
        .child(
            svg()
                .path(icons::path(icon))
                .size(px(icon_size))
                .text_color(theme::text()),
        )
}

/// 官方音量弹层：点音量图标后在上方弹出的竖向滑条。
const PROGRESS_GROUP: &str = "player-progress";

fn progress_bar(
    root: &Root,
    progress: f32,
    chorus: Option<f32>,
    loading: bool,
    cx: &mut Context<Root>,
) -> impl IntoElement {
    let shown = root.progress_preview.unwrap_or(progress).clamp(0.0, 1.0);
    let bounds_slot = root.progress_track_bounds.clone();
    let fraction_at = move |x: f32| -> Option<f32> {
        let (left, width) = bounds_slot.lock().ok().and_then(|value| {
            value.map(|rect| (f32::from(rect.left()), f32::from(rect.size.width)))
        })?;
        if width <= 1.0 {
            return None;
        }
        Some(((x - left) / width).clamp(0.0, 1.0))
    };
    let fraction_for_down = fraction_at.clone();
    let fraction_for_move = fraction_at.clone();
    let slot_for_canvas = root.progress_track_bounds.clone();

    div()
        .relative()
        .group(PROGRESS_GROUP)
        .flex()
        .items_center()
        .h(px(14.0))
        .flex_1()
        .min_w(px(120.0))
        .when(loading, |this| this.opacity(0.4))
        .child(
            div()
                .absolute()
                .top(px(5.5))
                .left(px(0.0))
                .right(px(0.0))
                .h(px(3.0))
                .rounded(px(2.0))
                .bg(theme::surface_hover()),
        )
        .child(
            div()
                .absolute()
                .top(px(5.5))
                .left(px(0.0))
                .h(px(3.0))
                .rounded(px(2.0))
                .bg(theme::accent())
                .w(gpui::relative(shown)),
        )
        .child(
            div()
                .absolute()
                .top(px(5.5))
                .left(gpui::relative(shown))
                .size(px(3.0))
                .rounded_full()
                .bg(theme::text()),
        )
        // 悬停时的加粗层：单独画一条 6px，避免和基础条抢同一个高度样式。
        .child(
            div()
                .absolute()
                .top(px(4.0))
                .left(px(0.0))
                .right(px(0.0))
                .h(px(6.0))
                .rounded(px(3.0))
                .bg(theme::surface_hover())
                .opacity(0.0)
                .group_hover(PROGRESS_GROUP, |style| style.opacity(1.0)),
        )
        .child(
            div()
                .absolute()
                .top(px(4.0))
                .left(px(0.0))
                .h(px(6.0))
                .rounded(px(3.0))
                .bg(theme::accent())
                .w(gpui::relative(shown))
                .opacity(0.0)
                .group_hover(PROGRESS_GROUP, |style| style.opacity(1.0)),
        )
        .child(
            // 悬停时出现的 10px 圆点（官方 slider-thumb，平时隐藏）。
            div()
                .absolute()
                .top(px(2.0))
                .left(gpui::relative(shown))
                .size(px(10.0))
                .rounded_full()
                .bg(theme::text())
                .opacity(0.0)
                .group_hover(PROGRESS_GROUP, |style| style.opacity(1.0)),
        )
        .when_some(chorus, |this, fraction| {
            this
                .child(
                    // 高潮点：高度跟随轨道（平时 3px）。
                    div()
                        .absolute()
                        .top(px(5.5))
                        .left(gpui::relative(fraction))
                        .size(px(3.0))
                        .rounded_full()
                        .bg(gpui::Rgba { a: 0.8, ..theme::text() })
                        .opacity(1.0)
                        .group_hover(PROGRESS_GROUP, |style| style.opacity(0.0)),
                )
                .child(
                    // 悬停时跟着轨道一起变粗（6px）。
                    div()
                        .absolute()
                        .top(px(4.0))
                        .left(gpui::relative(fraction))
                        .size(px(6.0))
                        .rounded_full()
                        .bg(gpui::Rgba { a: 0.8, ..theme::text() })
                        .opacity(0.0)
                        .group_hover(PROGRESS_GROUP, |style| style.opacity(1.0)),
                )
        })
        .child(
            canvas(
                move |bounds: gpui::Bounds<gpui::Pixels>, _window, _cx| {
                    if let Ok(mut guard) = slot_for_canvas.lock() {
                        *guard = Some(bounds);
                    }
                },
                |_bounds, _state, _window, _cx| {},
            )
            .absolute()
            .size_full(),
        )
        .child(
            div()
                .id("progress-track")
                .absolute()
                .size_full()
                .when(!loading, |this| this.cursor_pointer())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |root, event: &MouseDownEvent, _window, cx| {
                        if root.pending_track.is_some() {
                            return;
                        }
                        if let Some(fraction) = fraction_for_down(f32::from(event.position.x)) {
                            root.progress_preview = Some(fraction);
                            cx.notify();
                        }
                    }),
                )
                .on_mouse_move(
                    cx.listener(move |root, event: &MouseMoveEvent, _window, cx| {
                        if root.pending_track.is_some()
                            || event.pressed_button != Some(MouseButton::Left)
                        {
                            return;
                        }
                        if let Some(fraction) = fraction_for_move(f32::from(event.position.x)) {
                            root.progress_preview = Some(fraction);
                            cx.notify();
                        }
                    }),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(move |root, _event: &gpui::MouseUpEvent, _window, cx| {
                        if let Some(fraction) = root.progress_preview.take() {
                            if root.pending_track.is_none() {
                                root.seek_fraction(fraction);
                            }
                            cx.notify();
                        }
                    }),
                ),
        )
}

/// 档位 key（`auto` / `lossless` …）的显示名。设置页与播放条共用同一套说法。
pub(crate) fn quality_label(value: &str, language: crate::ui::i18n::Language) -> String {
    let intent = sodam_core::quality::QualityIntent::parse(value);
    match intent.label_key() {
        Some(key) => language.text(key).to_string(),
        // 认不出的档位原样显示，不硬翻成某一档
        None => intent.as_setting().to_string(),
    }
}

/// 引擎里的音质标签由核心层生成；这里按当前语言显示。
/// 档位名翻译。试听标注不在这里——它由 `PlaybackSnapshot::preview` 驱动，
/// 见 [`render`] 里那句 `quality.push_str(...)`。
pub(crate) fn localized_quality(raw: String, language: crate::ui::i18n::Language) -> String {
    if language.resolved().is_zh() {
        return raw;
    }
    // 档位名由核心层 `tier_label` 产出，新增档位时只要同步 `TIER_LABELS` 与词典，
    // 这里不用再维护一串手写 replace（漏一个就会在英文界面露出中文）。
    let mut text = raw;
    for label in sodam_core::quality::TIER_LABELS {
        let translated = language.text(label);
        if translated != label {
            text = text.replace(label, translated);
        }
    }
    // 旧缓存 sidecar 里 50 分档曾写作「标准」；按新名「流畅」的译法处理：
    // 既不能在英文界面露出中文，也不能翻成 libresoda 的 Standard(70)。
    text.replace("标准", language.text("流畅"))
}

/// 播放条的档位菜单：勾选的是**意图**，徽章上显示的是**实际**档位。
/// 官方音质菜单的展示项（顺序、标题、副标题、SVIP 标记都照官方）。
struct QualityOption {
    key: &'static str,
    title: &'static str,
    desc: &'static str,
    svip: bool,
}

const QUALITY_OPTIONS: [QualityOption; 5] = [
    QualityOption {
        key: "hires",
        title: "录音室音质",
        desc: "还原录音室效果，更通透纯净",
        svip: true,
    },
    QualityOption {
        key: "dolby",
        title: "全景声",
        desc: "无损全面升级，沉浸式空间环绕音感",
        svip: true,
    },
    QualityOption {
        key: "lossless",
        title: "无损音质",
        desc: "真无损，官方出品，原汁原味",
        svip: true,
    },
    QualityOption {
        key: "highest",
        title: "极高音质",
        desc: "",
        svip: false,
    },
    QualityOption {
        key: "medium",
        title: "标准音质",
        desc: "",
        svip: false,
    },
];

/// 音质在菜单/设置里的官方名称。
pub fn official_quality_label(value: &str) -> &'static str {
    match value {
        "hires" => "录音室音质",
        "dolby" => "全景声",
        "lossless" => "无损音质",
        "highest" => "极高音质",
        "medium" => "标准音质",
        "low" => "低音质",
        _ => "自动",
    }
}

pub(crate) fn quality_menu(
    root: &Root,
    cx: &mut Context<Root>,
    position: gpui::Point<gpui::Pixels>,
    viewport: gpui::Size<gpui::Pixels>,
    pill_bounds: Option<gpui::Bounds<gpui::Pixels>>,
) -> impl IntoElement {
    let playing = root.engine.snapshot().quality.to_ascii_lowercase();
    let current_key = if playing.is_empty() {
        root.settings.quality_intent().as_setting().to_string()
    } else {
        playing
    };
    let svip = root
        .account
        .as_ref()
        .is_some_and(|account| account.vip || !account.vip_stage.is_empty());

    // 官方深色弹层：不跟随主题，固定 #252525。
    let surface = gpui::rgb(0x252525);
    let text_primary = gpui::Rgba {
        a: 0.85,
        ..gpui::rgb(0xffffff)
    };
    let text_secondary = gpui::Rgba {
        a: 0.5,
        ..gpui::rgb(0xffffff)
    };

    let mut items: Vec<gpui::AnyElement> = Vec::new();
    if svip {
        items.push(
            div()
                .mx(px(10.0))
                .my(px(6.0))
                .px(px(10.0))
                .py(px(8.0))
                .rounded(px(8.0))
                .bg(gpui::rgb(0x3a3a2e))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .child(
                    svg()
                        .path(icons::path("svip"))
                        .w(px(44.0))
                        .h(px(12.0))
                        .text_color(gpui::rgb(0xffd98a)),
                )
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(text_primary)
                        .child("权益生效中，尊享音质、音效特权"),
                )
                .into_any_element(),
        );
    }
    for option in QUALITY_OPTIONS {
        let chosen = current_key == option.key
            || (option.key == "dolby" && current_key == "spatial");
        let key = option.key;
        items.push(
            div()
                .id(gpui::ElementId::Name(
                    format!("quality-menu-{key}").into(),
                ))
                .flex()
                .flex_row()
                .items_center()
                .h(px(56.0))
                .px(px(10.0))
                .py(px(0.0))
                .cursor_pointer()
                .hover(|style| style.bg(gpui::Rgba {
                    a: 0.1,
                    ..gpui::rgb(0xffffff)
                }))
                .on_click(cx.listener(move |root, _event: &ClickEvent, _window, cx| {
                    root.set_quality(key, cx);
                    root.quality_menu = None;
                    cx.notify();
                }))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .flex_1()
                        .min_w(px(0.0))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(4.0))
                                .when(option.svip, |this| {
                                    this.child(
                                        gpui::img("icons/spatial-quality-mini.png")
                                            .w(px(20.0))
                                            .h(px(15.0)),
                                    )
                                })
                                .child(
                                    div()
                                        .text_size(if option.svip { px(13.0) } else { px(12.0) })
                                        .line_height(px(20.0))
                                        .font_weight(if option.svip {
                                            gpui::FontWeight::BOLD
                                        } else {
                                            gpui::FontWeight::MEDIUM
                                        })
                                        .text_color(if option.svip {
                                            gpui::rgb(0xffe3a3)
                                        } else {
                                            text_primary
                                        })
                                        .child(option.title),
                                )
                                .when(option.svip, |this| {
                                    this.child(
                                        div()
                                            .px(px(4.0))
                                            .h(px(16.0))
                                            .rounded(px(4.0))
                                            .bg(gpui::rgb(0x4a4636))
                                            .flex()
                                            .items_center()
                                            .child(
                                                svg()
                                                    .path(icons::path("svip"))
                                                    .w(px(24.0))
                                                    .h(px(8.0))
                                                    .text_color(gpui::rgb(0xffd98a)),
                                            ),
                                    )
                                }),
                        )
                        .when(!option.desc.is_empty(), |this| {
                            this.child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(4.0))
                                    .text_size(px(11.0))
                                    .line_height(px(18.0))
                                    .text_color(text_secondary)
                                    .child(option.desc)
                                    .child(
                                        svg()
                                            .path(icons::path("info"))
                                            .size(px(14.0))
                                            .text_color(text_secondary),
                                    ),
                            )
                        }),
                )
                .when(chosen, |this| {
                    this.child(
                        svg()
                            .path(icons::path("check"))
                            .size(px(14.0))
                            .text_color(gpui::rgb(0xffffff)),
                    )
                })
                .into_any_element(),
        );
    }

    // 官方锚点规则：左边框在封面右缘与歌词文字之间，上边框在封面上下中点，
    // 右边框与循环图标右缘齐平，底部箭头对准音质胶囊中心。
    let width = f32::from(viewport.width);
    let height = f32::from(viewport.height);
    let content_h = (height - 48.0 - theme::PLAYER_H).max(320.0);
    let vh = (height - theme::PLAYER_H).max(360.0);
    let mut cover = (vh * 0.40).min(width * 0.30);
    let lyrics_w = (width * 0.30).clamp(300.0, 500.0);
    let centered = (width - theme::SIDEBAR_W - 100.0).max(220.0);
    let natural = cover + 50.0 + lyrics_w;
    if natural > centered {
        cover *= centered / natural;
    }
    let cover_left = (width - (cover + 50.0 + lyrics_w)) / 2.0;
    let _ = cover_left;
    let bar_top = height - theme::PLAYER_H;
    let cover_center = 48.0 + (content_h - (cover + 110.0)).max(0.0) / 2.0 + cover / 2.0;
    // 右侧：右距 20 + 队列 32 + 间距 12 + 音量 32 + 间距 12 = 循环右缘
    // 菜单以音质胶囊为中心：中心与底边都取胶囊的真实区域，避免估算偏差。
    let menu_width = 280.0;
    let pill_center = pill_bounds
        .map(|bounds| f32::from(bounds.center().x))
        .unwrap_or(width - 302.0);
    let pill_top = pill_bounds
        .map(|bounds| f32::from(bounds.top()))
        .unwrap_or(height - theme::PLAYER_H + 28.0);
    let menu_left = (pill_center - menu_width / 2.0).max(12.0);
    // 用「底边贴胶囊」定位：菜单高度怎么变，箭头与胶囊的间距都固定。
    let _ = cover_center;
    let _ = bar_top;
    let arrow_gap = 10.0;
    let menu_bottom_offset = (height - pill_top + arrow_gap).max(0.0);
    // 箭头宽 20：要让其中线对准胶囊中心，左边界 = 中心 - 10。
    let arrow_x = (pill_center - menu_left - 8.0).clamp(12.0, menu_width - 32.0);
    {
        use std::sync::atomic::{AtomicU64, Ordering};
        static LAST: AtomicU64 = AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or_default();
        if now.saturating_sub(LAST.load(Ordering::Relaxed)) > 500 {
            LAST.store(now, Ordering::Relaxed);
            crate::debug_log(&format!(
                "quality-menu geom: viewport={width}x{height} pill={pill_bounds:?} left={menu_left} bottom_off={menu_bottom_offset} w={menu_width} arrow_x={arrow_x} gap={arrow_gap}"
            ));
        }
    }
    let _ = position;
    gpui::deferred(
        div()
            .id("quality-menu")
            .absolute()
            .left(px(menu_left))
            .bottom(px(menu_bottom_offset))
            .w(px(menu_width))
            .flex()
            .flex_col()
            .rounded(px(10.0))
            .py(px(4.0))
            .bg(surface)
            .border_1()
            .border_color(gpui::Rgba { a: 0.06, ..gpui::rgb(0xffffff) })
            .shadow_lg()
            .occlude()
            .child(div().relative().flex().flex_col().children(items))
            .child(
                div()
                    .absolute()
                    .left(px(arrow_x))
                    .bottom(px(-6.0))
                    .child(
                        svg()
                            .path(icons::path("triangle-down"))
                            .w(px(16.0))
                            .h(px(8.0))
                            .text_color(surface),
                    ),
            ),
    )
    .with_priority(3)
}

/// 渲染播放条。
pub fn render(root: &Root, cx: &mut Context<Root>) -> impl IntoElement {
    root.marquee_needed.set(false);
    let language = root.language;
    let snapshot = root.engine.snapshot();
    let loading = root.pending_track.is_some();
    // 加载中显示「正在准备的这首」，而不是还在响的上一首 —— 否则会出现
    // 「界面显示 A、耳朵听到 B」的错位。
    let track = root
        .pending_track
        .clone()
        .or_else(|| root.queue.current().cloned());
    // 引擎错误（解码/设备）与一次播放尝试的失败都显示在这一行。
    // 一律过词典：引擎给的是运行时 String，`==` 梯子只覆盖写死的那两条，其余原样
    // 上屏——英文界面就会露出中文。查不到的（已经翻过的）原样返回。
    let error = if !snapshot.error.is_empty() {
        language.engine_error(&snapshot.error)
    } else {
        match root.playback_error.as_deref() {
            Some(text) => language.engine_error(text),
            None => String::new(),
        }
    };
    let progress = snapshot.progress_fraction();
    let chorus = track
        .as_ref()
        .and_then(|track| root.chorus_cache.get(&track.id).copied())
        .filter(|seconds| *seconds > 0.0 && snapshot.duration_seconds > 0.0)
        .map(|seconds| (seconds / snapshot.duration_seconds) as f32)
        .filter(|fraction| *fraction > 0.01 && *fraction < 0.99);
    // 实际拉到的流音质（不是设置里的偏好档位）；是不是试听片段由布尔决定标注，
    // 不再从标签字符串里 `contains("试听")` —— 那样显示与状态就同一个事实两处真相。
    let mut quality = localized_quality(snapshot.quality.clone(), language);
    // 官方按钮直接显示服务端档位名（去掉“音质”）：录音室 / 标准 / 较高 / 极高 / 无损 / 全景声
    // 胶囊只显示稳定档位名，不显示码率：官方那颗按钮不会因为
    // 「320k / 1594k」到达而突然变字。
    let raw_quality = format!("{} {}", snapshot.quality, quality);
    let quality_name = ["录音室", "全景声", "无损", "极高", "标准"]
        .into_iter()
        .find(|label| raw_quality.contains(label))
        .map(str::to_string)
        .unwrap_or_else(|| {
            let raw = raw_quality.to_ascii_lowercase();
            let label = if raw.contains("hi_res") || raw.contains("hires") {
                "录音室"
            } else if raw.contains("dolby") || raw.contains("spatial") {
                "全景声"
            } else if raw.contains("lossless") {
                "无损"
            } else if raw.contains("highest") || raw.contains("higher") {
                "极高"
            } else if raw.contains("medium") || raw.contains("standard") {
                "标准"
            } else {
                official_quality_label(root.settings.quality_intent().as_setting())
                    .trim_end_matches("音质")
            };
            label.to_string()
        });
    if snapshot.preview {
        quality.push_str(&format!(" · {}", root.tr("试听")));
    }
    // 降档：实际档位低于所选档位。每次现算——用户改了意图，旧结论就不该继续成立。
    // 实际档位未知（旧缓存）或意图为「自动」时一律不算降档；两边显示名相同时也不徽章，
    // 避免出现「降档·X」这种看起来没降的提示（分数拆档后真实降档都已不同名）。
    let volume = root.settings.volume;
    let mode_icon = root.queue.mode.icon();

    div()
        .relative()
        .occlude()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(theme::space::LG))
        .h(px(theme::PLAYER_H))
        .w_full()
        .flex_none()
        .px(px(theme::space::LG))
        .bg(theme::bg())
        // 官方把进度条贴在控制栏上沿（top: -9），横跨整个窗口。
        .child(
            div()
                .absolute()
                .top(px(-9.0))
                .left(px(0.0))
                .right(px(0.0))
                .child(progress_bar(root, progress, chorus, loading, cx)),
        )
        // 左：封面 + 标题/歌手/进度文案
        .child(
            // 官方：track 绝对定位在左，left 20、宽 50vw-150（这里用 50% 近似）。
            div()
                .absolute()
                .left(px(20.0))
                .top(px((theme::PLAYER_H - 32.0) / 2.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(theme::space::MD))
                .w(gpui::relative(0.5))
                .pr(px(150.0))
                
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w(px(0.0))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(theme::space::SM))
                                .min_w(px(0.0))
                                
                                
                        )
                        
                        .when(!error.is_empty(), |this| {
                            this.child(
                                div()
                                    .truncate()
                                    .text_size(theme::Text::Tiny.size())
                                    .text_color(theme::accent())
                                    .child(error),
                            )
                        }),
                ),
        )
        // 官方 .controls.left：评论 / 添加到歌单 / 少推荐 / 更多 + 时长
        // 只在播放页出现；非播放页换成下面的信息块（直接不渲染，避免盖色差）。
        .when(root.nav == crate::app::Nav::Lyrics, |this| {
        this.child(
            div()
                .absolute()
                .left(px(20.0))
                .top(px((theme::PLAYER_H - 32.0) / 2.0))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(12.0))
                .child({
                    let count = track
                        .as_ref()
                        .and_then(|track| root.comment_cache.get(&track.id).copied())
                        .unwrap_or(0);
                    div()
                        .id("bar-comment")
                        .relative()
                        .cursor_pointer()
                        .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                            if let Some(track) = root.queue.current().cloned() {
                                root.open_comments(track, cx);
                            }
                        }))
                        .size(px(32.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(8.0))
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::surface_hover()))
                        .child(
                            div()
                                .relative()
                                .size(px(20.0))
                                .child(
                                    svg()
                                        .path(icons::path("comment-bubble"))
                                        .size(px(20.0))
                                        .text_color(theme::text_muted()),
                                )
                                .when(count > 0, |this| {
                                    let label = if count >= 10_000 {
                                        format!("{:.1}w", count as f64 / 10_000.0)
                                    } else {
                                        count.to_string()
                                    };
                                    let align_right = label.chars().count() > 4;
                                    this.child(
                                        div()
                                            .absolute()
                                            .top(px(-7.0))
                                            .right(px(-14.0))
                                            .w(px(22.0))
                                            .h(px(12.0))
                                            .flex()
                                            .when(align_right, |this| this.justify_end())
                                            .child(
                                                div()
                                                    .px(px(2.0))
                                                    .rounded(px(6.0))
                                                    .bg(theme::surface_hover())
                                                    .text_size(px(9.0))
                                                    .font_weight(gpui::FontWeight::MEDIUM)
                                                    .text_color(theme::text_muted())
                                                    .child(label),
                                            ),
                                    )
                                }),
                        )
                })
                .child(
                    icon_button("bar-add", "note-plus", theme::ICON, theme::text_muted())
                        .on_click(cx.listener(|root, event: &ClickEvent, _window, cx| {
                            crate::debug_log("bar-add click");
                            if let Some(track) = root.queue.current().cloned() {
                                root.load_playlists(cx);
                                root.add_menu = Some((track, event.position()));
                                cx.notify();
                            }
                        })),
                )
                .child(
                    icon_button("bar-dislike", "thumb-down", theme::ICON, theme::text_muted())
                        .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                            if let Some(track) = root.queue.current().cloned() {
                                root.dislike_track(track, cx);
                            }
                        })),
                )
                .when(track.is_some(), |this| {
                    this.child(
                        icon_button("bar-more", "three-points-vertical", theme::ICON, theme::text_muted())
                            .on_click(cx.listener(|root, event: &ClickEvent, _window, cx| {
                                crate::debug_log("bar-more click");
                                if let Some(track) = root.queue.current().cloned() {
                                    root.queue_menu = None;
                                    root.track_menu = None;
                                    root.more_menu = Some((track, event.position()));
                                    cx.notify();
                                }
                            })),
                    )
                })
                .child(
                    div()
                        .px(px(6.0))
                        .text_size(px(13.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::text_muted())
                        .child(format!(
                            "{} / {}",
                            snapshot.position_label(),
                            snapshot.duration_label()
                        )),
                ),
        )
        })
        // 非播放页：左侧换成官方的「封面 + 歌名-歌手 + 操作图标」，
        // 盖住上面那排纯控制图标（截图核对过：此时不显示时长）。
        .when(root.nav != crate::app::Nav::Lyrics, |this| {
            this.child(track_info_left(root, &track, cx))
        })
        // 中：传输控制 + 进度
        .child(
            div()
                .absolute()
                .left(gpui::relative(0.5))
                .ml(px(-122.0))
                .top(px(0.0))
                .bottom(px(0.0))
                .flex()
                .flex_row()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(theme::space::LG))
                        .child(wide_transport_button(
                            "prev",
                            "skip-back",
                            64.0,
                            48.0,
                            20.0,
                            cx,
                            |root, cx| {
                                root.prev_track(cx);
                            },
                        ))
                        .child(
                            div()
                                .id("play-toggle")
                                .flex()
                                .items_center()
                                .justify_center()
                                .w(px(84.0))
                                .h(px(48.0))
                                .rounded(px(100.0))
                                .bg(gpui::Rgba { a: 0.05, ..theme::text() })
                                .hover(|style| style.bg(gpui::Rgba { a: 0.1, ..theme::text() }))
                                .cursor_pointer()
                                .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                                    root.toggle_play(cx);
                                    cx.notify();
                                }))
                                .child(if loading {
                                    crate::ui::spinner::spinner(
                                        24.0,
                                        theme::text(),
                                    )
                                    .into_any_element()
                                } else {
                                    svg()
                                        .path(icons::path(if snapshot.playing {
                                            "pause"
                                        } else {
                                            "play"
                                        }))
                                        .size(px(24.0))
                                        .text_color(theme::text())
                                        .into_any_element()
                                }),
                        )
                        .child(wide_transport_button(
                            "next",
                            "skip-forward",
                            64.0,
                            48.0,
                            20.0,
                            cx,
                            |root, cx| {
                                root.next_track(cx);
                            },
                        )),
                )
        )
        // 右：音量（内联滑条）+ 队列抽屉开关
        .child(
            div()
                .absolute()
                .right(px(20.0))
                .top(px(0.0))
                .bottom(px(0.0))
                .flex()
                .flex_row()
                .items_center()
                .justify_end()
                .gap(px(12.0))
                .flex_shrink(1.0)
                // 官方 actions-right 顺序：音质 → 音效 → 歌词 → 循环 → 音量 → 队列
                .child(
                    div()
                        .id("quality-button")
                        .relative()
                        .child({
                            let slot = root.quality_bounds.clone();
                            canvas(
                                move |bounds: gpui::Bounds<gpui::Pixels>, _window, _cx| {
                                    slot.set(Some(bounds));
                                },
                                |_bounds, _state, _window, _cx| {},
                            )
                            .absolute()
                            .size_full()
                        })
                        .h(px(26.0))
                        .px(px(6.0))
                        .min_w(px(32.0))
                        .flex()
                        .items_center()
                        .rounded(px(8.0))
                        .bg(gpui::Rgba { a: 0.07, ..theme::text() })
                        .cursor_pointer()
                        .hover(|style| style.bg(gpui::Rgba { a: 0.12, ..theme::text() }))
                        .text_size(px(13.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .on_click(cx.listener(|root, event: &ClickEvent, _window, cx| {
                            root.quality_menu = if root.quality_menu.is_some() {
                                None
                            } else {
                                Some(event.position())
                            };
                            cx.notify();
                        }))
                        .child(quality_name.clone()),
                )
                .child(
                    div()
                        .id("effect-button")
                        .h(px(26.0))
                        .px(px(6.0))
                        .min_w(px(32.0))
                        .flex()
                        .items_center()
                        .rounded(px(8.0))
                        .bg(gpui::Rgba { a: 0.07, ..theme::text() })
                        .cursor_pointer()
                        .hover(|style| style.bg(gpui::Rgba { a: 0.12, ..theme::text() }))
                        .text_size(px(13.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text())
                        .child(root.tr("音效").to_string()),
                )
                .child(
                    div()
                        .id("lyrics-toggle")
                        .size(px(32.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(8.0))
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::surface_hover()))
                        .text_size(px(13.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text_muted())
                        .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                            root.open_playback(cx);
                        }))
                        .child(root.tr("词").to_string()),
                )
                .child(
                    icon_button("play-mode", mode_icon, theme::ICON, theme::text_muted())
                        .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                            root.cycle_play_mode(cx);
                        })),
                )
                .child(
                    div()
                        .id("volume-anchor")
                        .relative()
                        .on_hover(cx.listener(|root, hovered: &bool, _window, cx| {
                            if *hovered {
                                root.volume_open = true;
                                cx.notify();
                                return;
                            }
                            // 鼠标要跨过按钮与浮层之间的空隙：延迟关闭，
                            // 进入浮层会取消这次关闭。
                            cx.spawn(async move |this, cx| {
                                cx.background_executor()
                                    .timer(std::time::Duration::from_millis(350))
                                    .await;
                                let _ = this.update(cx, |root, cx| {
                                    if !root.volume_inside {
                                        root.volume_open = false;
                                        cx.notify();
                                    }
                                });
                            })
                            .detach();
                        }))
                        .child(
                            icon_button(
                                "volume",
                                if volume <= 0.01 { "volume-x" } else { "volume-2" },
                                theme::ICON,
                                theme::text_muted(),
                            )
                            .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                                root.toggle_mute();
                                cx.notify();
                            })),
                        )
                        .when(root.volume_open, |this| {
                            this.child(
                                gpui::deferred(volume_popover(root, cx))
                                    .with_priority(6)
                                    .into_any_element(),
                            )
                        }),
                )
                .child(
                    icon_button("queue", "list-end", theme::ICON, theme::text_muted()).on_click(
                        cx.listener(|root, _event: &ClickEvent, _window, cx| {
                            root.queue_open = !root.queue_open;
                            if root.queue_open {
                                root.anchor_queue_scroll();
                            }
                            cx.notify();
                        }),
                    ),
                ),
        )
}

// ---------------------------------------------------------------------------
// 右侧队列抽屉（常驻侧栏，虚拟列表）
// ---------------------------------------------------------------------------

/// 抽屉里的一行：分区标题或曲目。用**同一个高度**排布，
/// 这样可以用 `uniform_list` 虚拟化（只渲染可见的十来行）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum QueueSlot {
    /// 分区标题（带该段数量）。
    History(usize),
    NowPlaying,
    Upcoming(usize),
    Track {
        index: usize,
        playing: bool,
    },
}

/// 和「我喜欢的音乐」列表保持一致的行高（虚拟列表要求等高）。
const QUEUE_ROW_H: f32 = theme::size::LIST_ROW;

fn queue_slots(root: &Root) -> Vec<QueueSlot> {
    let tracks = root.queue.tracks();
    let current = root.queue.index();
    let mut slots = Vec::new();

    // 顺序仍是「已播放 → 正在播放 → 接下来」；
    // 打开抽屉时把滚动位置锚到「正在播放」（向上滑看已播放，向下滑看接下来）。
    // 已播放 = 真实播过的歌（root.played_history 按播放顺序追加，最近在后），
    // 展示保持时序：最老的在顶，刚播完的一首紧邻「正在播放」。
    // 不是「当前下标之前的所有歌」——那会把跳过的歌也算进去。
    if !root.played_history.is_empty() {
        let history: Vec<usize> = root
            .played_history
            .iter()
            .filter_map(|id| tracks.iter().position(|track| &track.id == id))
            .filter(|index| *index != current)
            .collect();
        if !history.is_empty() {
            slots.push(QueueSlot::History(history.len()));
            for index in history {
                slots.push(QueueSlot::Track {
                    index,
                    playing: false,
                });
            }
        }
    }
    if current < tracks.len() {
        slots.push(QueueSlot::NowPlaying);
        slots.push(QueueSlot::Track {
            index: current,
            playing: true,
        });
    }
    // 「接下来」按真实播放顺序排：列表循环是列表顺序，随机播放是预先算好的
    // 随机轮次（`Queue::upcoming`），单曲循环则为空。不能再按 `current + 1..len`
    // 顺序给——随机模式下那样显示的和实际播放不符。
    let upcoming = root.queue.upcoming();
    if !upcoming.is_empty() {
        slots.push(QueueSlot::Upcoming(upcoming.len()));
        for index in upcoming {
            if index < tracks.len() {
                slots.push(QueueSlot::Track {
                    index,
                    playing: false,
                });
            }
        }
    }
    slots
}

/// 「正在播放」所在的行号（打开抽屉时锚定到它）。
pub fn now_playing_slot(root: &Root) -> usize {
    queue_slots(root)
        .iter()
        .position(|slot| matches!(slot, QueueSlot::NowPlaying))
        .unwrap_or(0)
}

/// 抽屉里的分区标题行（和曲目同高，方便虚拟列表统一排版）。
fn queue_section_row(title: &str) -> gpui::AnyElement {
    div()
        .w_full()
        .flex()
        .items_center()
        .h(px(QUEUE_ROW_H))
        .px(px(theme::space::MD))
        .text_size(theme::Text::Tiny.size())
        .text_color(theme::text_faint())
        .child(title.to_string())
        .into_any_element()
}

/// 主界面的右侧队列抽屉。
pub fn queue_drawer(root: &Root, cx: &mut Context<Root>, own_titlebar: bool) -> impl IntoElement {
    queue_drawer_inner(root, cx, false, own_titlebar)
}

/// 歌词页内嵌队列：无侧边分割线，背景融入歌词页。
pub fn queue_panel(root: &Root, cx: &mut Context<Root>) -> impl IntoElement {
    // 内嵌面板不贴窗口顶部，不需要为标题栏让位。
    queue_drawer_inner(root, cx, true, false)
}

/// 队列内容：主界面为 320px 抽屉；歌词页为无边界内嵌面板。
fn queue_drawer_inner(
    root: &Root,
    cx: &mut Context<Root>,
    embedded: bool,
    own_titlebar: bool,
) -> impl IntoElement {
    use std::sync::Arc;

    let language = root.language;

    let slots = Arc::new(queue_slots(root));
    let tracks = root.queue_cache.1.clone();
    let covers = root.covers.clone();
    let cover_requests = root.cover_requests.clone();
    let liked_ids = root.liked_ids.clone();
    let entity = cx.entity();
    let count = slots.len();
    let total = root.queue.len();

    let list = uniform_list("queue-drawer-list", count, move |range, _window, _cx| {
        range
            .map(|slot_index| {
                let slot = slots[slot_index];
                match slot {
                    QueueSlot::History(count) => {
                        queue_section_row(&language.textf("已播放 · {} 首", &[count.to_string()]))
                    }
                    QueueSlot::NowPlaying => queue_section_row(language.text("正在播放")),
                    QueueSlot::Upcoming(count) => {
                        queue_section_row(&language.textf("接下来 · {} 首", &[count.to_string()]))
                    }
                    QueueSlot::Track { index, playing } => {
                        let Some(track) = tracks.get(index) else {
                            return div().h(px(QUEUE_ROW_H)).into_any_element();
                        };
                        let url = track.cover.clone();
                        let cached = covers.get(&url).cloned();
                        // 只有渲染出来的行才排队取封面（虚拟列表只渲染可见行）
                        if cached.is_none() && !url.is_empty() {
                            if let Ok(mut queue) = cover_requests.lock() {
                                if !queue.iter().any(|item| item == &url) {
                                    queue.push(url.clone());
                                }
                            }
                        }
                        let title = track.title.clone();
                        let artist = track.artist.clone();
                        let duration = track.duration_label();
                        let entity = entity.clone();
                        div()
                            .id(("queue-slot", slot_index))
                            .w_full()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(theme::space::MD))
                            .h(px(QUEUE_ROW_H))
                            .px(px(theme::space::MD))
                            .rounded(px(theme::radius::ROW))
                            .cursor_pointer()
                            .when(playing, |this| this.bg(theme::accent_soft()))
                            .hover(|style| style.bg(theme::surface_hover()))
                            .on_click({
                                let entity = entity.clone();
                                move |_event: &ClickEvent, _window, cx: &mut gpui::App| {
                                    entity.update(cx, |root, cx| {
                                        root.queue_menu = None;
                                        root.jump_in_queue(index, cx);
                                        cx.notify();
                                    });
                                }
                            })
                            .on_mouse_down(MouseButton::Right, {
                                let entity = entity.clone();
                                let track_id = track.id.clone();
                                move |event: &MouseDownEvent, _window, cx: &mut gpui::App| {
                                    let position = event.position;
                                    entity.update(cx, |root, cx| {
                                        // 正在播放的曲目不弹菜单：它不能从队列移除
                                        // （还挂在引擎上），也不需要「下一首播放」自己。
                                        // 除了当前下标，还要拦「引擎仍在播但下标已跳走」
                                        // 的 pending 窗口（收尾瞬间切歌）。
                                        let engine_id = root.engine.snapshot().track_id;
                                        if playing || track_id == engine_id {
                                            return;
                                        }
                                        root.queue_menu = Some((index, position));
                                        cx.notify();
                                    });
                                }
                            })
                            .child(cover(cached, theme::size::ROW_ART, theme::radius::ART))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(theme::Text::Body.size())
                                            .text_color(if playing {
                                                theme::accent()
                                            } else {
                                                theme::text()
                                            })
                                            .child(title),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(theme::Text::Small.size())
                                            .text_color(theme::text_muted())
                                            .child(artist),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(44.0))
                                    .text_right()
                                    .text_size(theme::Text::Small.size())
                                    .text_color(theme::text_faint())
                                    .child(duration),
                            )
                            .child(
                                div()
                                    .id(("queue-like", slot_index))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(theme::size::CONTROL_SM))
                                    .rounded(px(theme::radius::PILL))
                                    .cursor_pointer()
                                    .hover(|style| style.bg(theme::surface_hover()))
                                    .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                                        cx.stop_propagation();
                                    })
                                    .on_click({
                                        let entity = entity.clone();
                                        let like_track = track.clone();
                                        move |_event: &ClickEvent, _window, cx: &mut gpui::App| {
                                            entity.update(cx, |root, cx| {
                                                root.toggle_like(like_track.clone(), cx);
                                            });
                                        }
                                    })
                                    .child(
                                        svg()
                                            .path(icons::path(if liked_ids.contains(&track.id) {
                                                "heart-filled"
                                            } else {
                                                "heart"
                                            }))
                                            .size(px(theme::ICON_SM))
                                            .text_color(if liked_ids.contains(&track.id) {
                                                theme::accent()
                                            } else {
                                                theme::text_faint()
                                            }),
                                    ),
                            )
                            .into_any_element()
                    }
                }
            })
            .collect::<Vec<_>>()
    })
    .flex_1()
    .min_h(px(0.0))
    .track_scroll(&root.queue_scroll);

    let menu_tracks = root.queue_cache.1.clone();
    let menu = root.queue_menu.and_then(|(index, position)| {
        let track = menu_tracks.get(index)?.clone();
        Some((index, position, track))
    });

    div()
        .relative()
        .flex()
        .flex_col()
        .h_full()
        .when(embedded, |this| {
            this.flex_1()
                .w_full()
                .min_w(px(0.0))
                .bg(gpui::transparent_black())
        })
        .when(!embedded, |this| {
            this.w(px(320.0))
                .flex_none()
                .bg(theme::bg())
                .border_l_1()
                .border_color(theme::border())
                // 独立队列面板也铺到顶部，列表头避开自绘标题栏的窗口按钮。
                .when(own_titlebar, |this| {
                    this.pt(px(super::titlebar::STRIP_HEIGHT))
                })
        })
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(px(theme::space::SM))
                .h(px(theme::TOOLBAR_H))
                .flex_none()
                .px(px(theme::space::LG))
                .when(!embedded, |this| {
                    this.border_b_1().border_color(theme::border())
                })
                .child(
                    div()
                        .text_size(theme::Text::Tiny.size())
                        .text_color(theme::text_faint())
                        .child(language.textf("播放队列 · {} 首", &[total.to_string()])),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(theme::space::XS))
                        .child(
                            icon_button("drawer-close", "x", theme::ICON_SM, theme::text_muted())
                                .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                                    root.queue_open = false;
                                    cx.notify();
                                })),
                        ),
                ),
        )
        .child(list)
        .when_some(menu, |this, (index, position, track)| {
            this.child(
                gpui::deferred(
                    gpui::anchored()
                        .snap_to_window()
                        .position(gpui::point(position.x, position.y))
                        .child(
                            div()
                                .id("queue-menu")
                                .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                                    // 菜单内的点击不要穿透到下层行（否则会触发切歌）
                                    cx.stop_propagation();
                                })
                                .flex()
                                .flex_col()
                                .w(px(180.0))
                                .p(px(theme::space::XS))
                                .rounded(px(theme::radius::CARD))
                                .bg(theme::surface_elevated())
                                .border_1()
                                .border_color(theme::border())
                                .shadow_lg()
                                .child(
                                    div()
                                        .px(px(theme::space::SM))
                                        .py(px(theme::space::XS))
                                        .truncate()
                                        .text_size(theme::Text::Tiny.size())
                                        .text_color(theme::text_faint())
                                        .child(track.title.clone()),
                                )
                                .child(
                                    div()
                                        .id("queue-remove")
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap(px(theme::space::SM))
                                        .px(px(theme::space::SM))
                                        .py(px(theme::space::XS))
                                        .rounded(px(theme::radius::ROW))
                                        .cursor_pointer()
                                        .hover(|style| style.bg(theme::surface_hover()))
                                        .text_size(theme::Text::Small.size())
                                        .text_color(theme::text())
                                        .on_click(cx.listener(
                                            move |root, _event: &ClickEvent, _window, cx| {
                                                root.remove_from_queue(index, cx);
                                            },
                                        ))
                                        .child(
                                            svg()
                                                .path(icons::path("x"))
                                                .size(px(theme::ICON_SM))
                                                .text_color(theme::text_muted()),
                                        )
                                        .child(root.tr("从队列移除")),
                                ),
                        ),
                )
                .with_priority(2),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::localized_quality;
    use crate::ui::i18n::Language;

    /// 徽章文本是“档位名 + 码率”的拼接，翻译必须只换档位名、保留码率。
    #[test]
    fn localized_quality_translates_every_tier_and_keeps_bitrate() {
        let cases = [
            ("无损 871k", "Lossless 871k"),
            ("Hi-Res 无损 871k", "Hi-Res Lossless 871k"),
            ("Hi-Res 320k", "Hi-Res 320k"),
            ("杜比全景声 768k", "Dolby Atmos 768k"),
            ("极高 320k", "High 320k"),
            ("较高 256k", "Medium 256k"),
            ("标准 128k", "Low 128k"),
            ("省流 96k", "Data saver 96k"),
            ("未知", "Unknown"),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                localized_quality(raw.to_string(), Language::English),
                expected,
                "{raw}"
            );
        }
    }

    #[test]
    fn localized_quality_passes_chinese_through_unchanged() {
        for raw in [
            "无损 871k",
            "Hi-Res 无损 871k",
            "Hi-Res 320k",
            "杜比全景声 768k",
            "省流 96k",
            "未知",
        ] {
            assert_eq!(
                localized_quality(raw.to_string(), Language::Chinese),
                raw,
                "{raw}"
            );
        }
    }
}

/// 非播放页的左侧信息块：封面 + 歌名/歌手 + 收藏/评论/添加/少推荐/更多。
fn track_info_left(
    root: &Root,
    track: &Option<sodam_core::models::TrackItem>,
    cx: &mut Context<Root>,
) -> impl IntoElement {
    let Some(track) = track.clone() else {
        return div().into_any_element();
    };
    let cover_path = root.cover_of(&track.cover);
    let liked = root.liked_ids.contains(&track.id);
    let count = root.comment_cache.get(&track.id).copied().unwrap_or(0);
    let open = {
        let track = track.clone();
        move |root: &mut Root, _e: &ClickEvent, _w: &mut gpui::Window, cx: &mut Context<Root>| {
            let _ = &track;
            root.open_playback(cx);
        }
    };

    div()
        .absolute()
        .left(px(20.0))
        .top(px(0.0))
        .bottom(px(0.0))
        // 右边界止于中间传输控件左侧，绝不盖住「上一首」。
        .right(gpui::relative(0.5))
        .mr(px(150.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .child(
            div()
                .id("bar-track-cover")
                .flex_none()
                .cursor_pointer()
                .on_click(cx.listener(open))
                .child(crate::ui::artwork::cover(cover_path, 56.0, 8.0)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .min_w(px(0.0))
                .child({
                    // 官方：歌名+歌手超长时向左循环滚动，尾首相接、间隔 24px。
                    // 用 gpui 的 with_animation（帧时钟驱动），比定时器 notify 更稳。
                    let title_len = track.title.chars().count() as f32;
                    let artist_len = track.artist.chars().count() as f32;
                    let estimated = title_len * 15.0 + artist_len * 12.0 + 28.0;
                    let long = estimated > 300.0;
                    let title = track.title.clone();
                    let artist = track.artist.clone();
                    let copy = || {
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .whitespace_nowrap()
                            .child(
                                div()
                                    .text_size(px(15.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme::text())
                                    .child(title.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(15.0))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(gpui::Rgba {
                                        a: 0.45,
                                        ..theme::text()
                                    })
                                    .child(format!("– {artist}")),
                            )
                    };
                    if long {
                        // 用实测文本宽度算周期，否则两份文本在循环接缝处会错开、
                        // 看起来就是一顿一顿的跳。
                        // 宽度取整：亚像素抖动会让 duration 每帧变化，动画看起来发抖。
                        let measured = root.marquee_width.get().round();
                        let width = if measured > 1.0 { measured } else { estimated };
                        let span = width + 24.0;
                        let seconds = (width * 0.03).max(3.0);
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .overflow_hidden()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .whitespace_nowrap()
                                    .child({
                                        let slot = root.marquee_width.clone();
                                        div()
                                            .relative()
                                            .child(copy())
                                            .child(
                                                canvas(
                                                    move |bounds: gpui::Bounds<gpui::Pixels>, _w, _cx| {
                                                        slot.set(f32::from(bounds.size.width));
                                                    },
                                                    |_b, _s, _w, _c| {},
                                                )
                                                .absolute()
                                                .size_full(),
                                            )
                                    })
                                    .child(div().ml(px(24.0)).child(copy()))
                                    .with_animation(
                                        "bar-track-marquee",
                                        Animation::new(std::time::Duration::from_secs_f32(seconds))
                                            .repeat(),
                                        move |element, delta| {
                                            // 对应官方 translateX(0 → -(width+24)) 的关键帧。
                                            element.ml(px(-(delta as f32) * span))
                                        },
                                    ),
                            )
                    } else {
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .overflow_hidden()
                            .child(copy())
                    }
                })
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(16.0))
                        .child({
                            let t = track.clone();
                            icon_button(
                                "bar-track-like",
                                if liked { "heart-filled" } else { "heart" },
                                theme::ICON,
                                if liked {
                                    theme::accent()
                                } else {
                                    theme::text_muted()
                                },
                            )
                            .on_click(cx.listener(move |root, _e: &ClickEvent, _w, cx| {
                                root.toggle_like(t.clone(), cx);
                            }))
                        })
                        .child({
                            let t = track.clone();
                            div()
                                .id("bar-track-comment")
                                .relative()
                                .cursor_pointer()
                                .on_click(cx.listener(move |root, _e: &ClickEvent, _w, cx| {
                                    root.open_comments(t.clone(), cx);
                                }))
                                .child(
                                    svg()
                                        .path(icons::path("comment-bubble"))
                                        .size(px(theme::ICON))
                                        .text_color(theme::text_muted()),
                                )
                                .when(count > 0, |this| {
                                    this.child(
                                        div()
                                            .absolute()
                                            .top(px(-7.0))
                                            .right(px(-14.0))
                                            .w(px(22.0))
                                            .h(px(12.0))
                                            .flex()
                                            .justify_end()
                                            .child(
                                                div()
                                                    .px(px(2.0))
                                                    .h(px(12.0))
                                                    .rounded(px(6.0))
                                                    .bg(theme::surface_hover())
                                                    .flex()
                                                    .items_center()
                                                    .text_size(px(9.0))
                                                    .font_weight(gpui::FontWeight::MEDIUM)
                                                    .text_color(theme::text_muted())
                                                    .child(count.to_string()),
                                            ),
                                    )
                                })
                        })
                        .child({
                            let t = track.clone();
                            icon_button("bar-track-add", "note-plus", theme::ICON, theme::text_muted())
                                .on_click(cx.listener(move |root, event: &ClickEvent, _w, cx| {
                                    root.load_playlists(cx);
                                    root.add_menu = Some((t.clone(), event.position()));
                                    cx.notify();
                                }))
                        })
                        .child({
                            let t = track.clone();
                            icon_button("bar-track-dislike", "thumb-down", theme::ICON, theme::text_muted())
                                .on_click(cx.listener(move |root, _e: &ClickEvent, _w, cx| {
                                    root.dislike_track(t.clone(), cx);
                                }))
                        })
                        .child({
                            let t = track.clone();
                            icon_button(
                                "bar-track-more",
                                "three-points-vertical",
                                theme::ICON,
                                theme::text_muted(),
                            )
                            .on_click(cx.listener(move |root, event: &ClickEvent, _w, cx| {
                                root.more_menu = Some((t.clone(), event.position()));
                                cx.notify();
                            }))
                        }),
                ),
        )
        .into_any_element()
}

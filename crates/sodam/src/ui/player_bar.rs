//! 底部播放条 + 右侧队列抽屉。
//!
//! 布局（参考官方客户端）：左「封面 + 标题/歌手」、中「上一首 / 播放 / 下一首 + 进度」、
//! 右「音量图标 + 内联音量滑条 + 队列开关」。音质不在这里选（见设置页）。

use gpui::prelude::*;
use gpui::{
    canvas, div, px, svg, uniform_list, ClickEvent, Context, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent,
};

use crate::app::Root;
use crate::ui::artwork::{cover, icon_button};
use crate::ui::{icons, theme};

/// 内联音量条宽度与滑块直径。
const VOLUME_TRACK_W: f32 = 96.0;
const VOLUME_THUMB: f32 = 11.0;

fn transport_button(
    id: &'static str,
    icon: &'static str,
    size: f32,
    cx: &mut Context<Root>,
    action: fn(&mut Root, &mut Context<Root>),
) -> impl IntoElement {
    icon_button(id, icon, size, theme::text_muted()).on_click(cx.listener(
        move |root, _event: &ClickEvent, _window, cx| {
            action(root, cx);
            cx.notify();
        },
    ))
}

/// 内联音量滑条：细轨道 + 强调色已播段 + 圆形滑块，点或拖都能调。
fn volume_slider(root: &Root, cx: &mut Context<Root>) -> impl IntoElement {
    let volume = root.engine.snapshot().volume.clamp(0.0, 1.0);
    let filled = (VOLUME_TRACK_W - VOLUME_THUMB) * volume;
    let bounds_slot = root.volume_track_bounds.clone();
    let apply = move |x: f32, root: &mut Root, cx: &mut Context<Root>| {
        let left = bounds_slot
            .lock()
            .ok()
            .and_then(|value| value.map(|rect| f32::from(rect.left())))
            .unwrap_or(0.0);
        let ratio = ((x - left) / VOLUME_TRACK_W).clamp(0.0, 1.0);
        root.set_volume(ratio);
        cx.notify();
    };
    let apply_down = apply.clone();
    let apply_move = apply.clone();
    let slot_for_canvas = root.volume_track_bounds.clone();

    div()
        .relative()
        .flex()
        .items_center()
        .h(px(16.0))
        .w(px(VOLUME_TRACK_W))
        .flex_none()
        .child(
            div()
                .absolute()
                .top(px(6.5))
                .left(px(0.0))
                .w(px(VOLUME_TRACK_W))
                .h(px(3.0))
                .rounded(px(2.0))
                .bg(theme::surface_hover()),
        )
        .child(
            div()
                .absolute()
                .top(px(6.5))
                .left(px(0.0))
                .w(px(filled + VOLUME_THUMB / 2.0))
                .h(px(3.0))
                .rounded(px(2.0))
                .bg(theme::accent()),
        )
        .child(
            div()
                .absolute()
                .top(px((16.0 - VOLUME_THUMB) / 2.0))
                .left(px(filled))
                .size(px(VOLUME_THUMB))
                .rounded_full()
                .bg(theme::text()),
        )
        .child(
            // 用 canvas 记录轨道在窗口里的位置，鼠标 x 才能换算成比例
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
                .id("volume-track")
                .absolute()
                .size_full()
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |root, event: &MouseDownEvent, _window, cx| {
                        apply_down(f32::from(event.position.x), root, cx);
                    }),
                )
                .on_mouse_move(
                    cx.listener(move |root, event: &MouseMoveEvent, _window, cx| {
                        if event.pressed_button == Some(MouseButton::Left) {
                            apply_move(f32::from(event.position.x), root, cx);
                        }
                    }),
                ),
        )
}

/// 进度条：拖动过程中只更新预览（不 seek），**松手才真正跳转**。
///
/// 之前是「每移动一次就 try_seek 一次」，解码器被反复打断，
/// 听起来像变速/鬼畜；sonora 的做法也是拖动预览、松手提交。
fn progress_bar(
    root: &Root,
    progress: f32,
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

/// 引擎里的音质标签由核心层生成；这里按当前语言显示。
fn localized_quality(raw: String, language: crate::ui::i18n::Language) -> String {
    if language.resolved().is_zh() {
        return raw;
    }
    raw.replace("无损", "Lossless")
        .replace("极高", "High")
        .replace("较高", "Medium")
        .replace("标准", "Standard")
        .replace("未知", "Unknown")
        .replace(" · 试听", " · Preview")
}

/// 渲染播放条。
pub fn render(root: &Root, cx: &mut Context<Root>) -> impl IntoElement {
    let language = root.language;
    let snapshot = root.engine.snapshot();
    let loading = root.pending_track.is_some();
    // 加载中显示「正在准备的这首」，而不是还在响的上一首 —— 否则会出现
    // 「界面显示 A、耳朵听到 B」的错位。
    let track = root
        .pending_track
        .clone()
        .or_else(|| root.queue.current().cloned());
    let title = track
        .as_ref()
        .map(|track| track.title.clone())
        .unwrap_or_else(|| language.text("未在播放").to_string());
    let artist = track
        .as_ref()
        .map(|track| track.artist.clone())
        .unwrap_or_else(|| language.text("选择一首歌开始播放吧").to_string());
    let detail = artist.clone();
    // 引擎错误（解码/设备）与「拉流失败」都显示在这一行
    let error = if !snapshot.error.is_empty() {
        snapshot.error.clone()
    } else {
        root.playback_error.clone().unwrap_or_default()
    };
    let progress = snapshot.progress_fraction();
    // 实际拉到的流音质（不是设置里的偏好档位）
    let quality = localized_quality(snapshot.quality.clone(), language);
    let title_link_track = track.as_ref().cloned();
    let artist_link_track = track.as_ref().cloned();
    let volume = snapshot.volume;
    let liked = track
        .as_ref()
        .is_some_and(|track| root.liked_ids.contains(&track.id));
    let cover_path = track.as_ref().and_then(|track| root.cover_of(&track.cover));

    div()
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
        .border_t_1()
        .border_color(theme::border())
        // 左：封面 + 标题/歌手/进度文案
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(theme::space::MD))
                .w(px(280.0))
                .min_w(px(200.0))
                .flex_shrink(1.0)
                .child(
                    div()
                        .id("now-playing-cover")
                        .flex_none()
                        .cursor_pointer()
                        .hover(|style| style.opacity(0.82))
                        .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                            root.open_playback(cx);
                        }))
                        .child(cover(
                            cover_path,
                            theme::size::PLAYER_THUMB,
                            theme::radius::ART,
                        )),
                )
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
                                .child(
                                    div()
                                        .id("player-title-link")
                                        .min_w(px(0.0))
                                        .truncate()
                                        .cursor_pointer()
                                        .text_size(theme::Text::Body.size())
                                        .text_color(theme::text())
                                        .hover(|style| style.text_color(theme::accent()))
                                        .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                                            cx.stop_propagation();
                                        })
                                        .on_click(cx.listener({
                                            move |root, _event: &ClickEvent, _window, cx| {
                                                if let Some(track) = title_link_track.as_ref() {
                                                    root.open_track_album(track.clone(), cx);
                                                }
                                            }
                                        }))
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .id("player-like")
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .self_center()
                                        .size(px(theme::size::CONTROL_SM))
                                        .rounded(px(theme::radius::PILL))
                                        .cursor_pointer()
                                        .hover(|style| style.bg(theme::surface_hover()))
                                        .on_click(cx.listener({
                                            let like_track = track.clone();
                                            move |root, _event: &ClickEvent, _window, cx| {
                                                if let Some(track) = like_track.clone() {
                                                    root.toggle_like(track, cx);
                                                }
                                            }
                                        }))
                                        .child(
                                            svg()
                                                .path(icons::path(if liked {
                                                    "heart-filled"
                                                } else {
                                                    "heart"
                                                }))
                                                .size(px(theme::ICON_SM))
                                                .text_color(if liked {
                                                    theme::accent()
                                                } else {
                                                    theme::text_muted()
                                                }),
                                        ),
                                ),
                        )
                        .child(
                            // 左侧曲目信息：歌手 · 时间，后面接**实际拉到的流音质**
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(theme::space::SM))
                                .min_w(px(0.0))
                                .child(
                                    div()
                                        .id("player-artist-link")
                                        .min_w(px(0.0))
                                        .truncate()
                                        .cursor_pointer()
                                        .text_size(theme::Text::Small.size())
                                        .text_color(theme::text_muted())
                                        .hover(|style| style.text_color(theme::accent()))
                                        .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                                            cx.stop_propagation();
                                        })
                                        .on_click(cx.listener({
                                            move |root, _event: &ClickEvent, _window, cx| {
                                                if let Some(track) = artist_link_track.as_ref() {
                                                    root.open_track_artist(track.clone(), cx);
                                                }
                                            }
                                        }))
                                        .child(detail),
                                )
                                .when(!quality.is_empty(), |this| {
                                    this.child(
                                        div()
                                            .flex_none()
                                            .px(px(theme::space::SM))
                                            .py(px(1.0))
                                            .rounded(px(theme::radius::PILL))
                                            .bg(theme::surface_elevated())
                                            .text_size(theme::Text::Tiny.size())
                                            .text_color(theme::accent())
                                            .child(quality.clone()),
                                    )
                                }),
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
        // 中：传输控制 + 进度
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(theme::space::XS))
                .flex_1()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(theme::space::LG))
                        .child(transport_button(
                            "prev",
                            "skip-back",
                            theme::ICON_LG,
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
                                .size(px(40.0))
                                .rounded_full()
                                .bg(theme::accent())
                                .cursor_pointer()
                                .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
                                    root.toggle_play(cx);
                                    cx.notify();
                                }))
                                .child(if loading {
                                    crate::ui::spinner::spinner(
                                        theme::ICON_LG,
                                        theme::accent_foreground(),
                                    )
                                    .into_any_element()
                                } else {
                                    svg()
                                        .path(icons::path(if snapshot.playing {
                                            "pause"
                                        } else {
                                            "play"
                                        }))
                                        .size(px(theme::ICON_LG))
                                        .text_color(theme::accent_foreground())
                                        .into_any_element()
                                }),
                        )
                        .child(transport_button(
                            "next",
                            "skip-forward",
                            theme::ICON_LG,
                            cx,
                            |root, cx| {
                                root.next_track(cx);
                            },
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(theme::space::SM))
                        .w(px(420.0))
                        .max_w_full()
                        .child(
                            div()
                                .text_size(theme::Text::Tiny.size())
                                .text_color(theme::text_faint())
                                .child(snapshot.position_label()),
                        )
                        .child(progress_bar(root, progress, loading, cx))
                        .child(
                            div()
                                .text_size(theme::Text::Tiny.size())
                                .text_color(theme::text_faint())
                                .child(snapshot.duration_label()),
                        ),
                ),
        )
        // 右：音量（内联滑条）+ 队列抽屉开关
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_end()
                .gap(px(theme::space::SM))
                .w(px(240.0))
                .min_w(px(180.0))
                .flex_shrink(1.0)
                .child(
                    icon_button(
                        "volume",
                        if volume <= 0.01 {
                            "volume-x"
                        } else {
                            "volume-2"
                        },
                        theme::ICON,
                        theme::text_muted(),
                    )
                    .on_click(cx.listener(
                        |root, _event: &ClickEvent, _window, cx| {
                            root.toggle_mute();
                            cx.notify();
                        },
                    )),
                )
                .child(volume_slider(root, cx))
                .child(
                    icon_button("queue", "list-end", theme::ICON, theme::text_muted()).on_click(
                        cx.listener(|root, _event: &ClickEvent, _window, cx| {
                            root.queue_open = !root.queue_open;
                            if root.queue_open {
                                // 打开时把滚动位置锚到「正在播放」
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
    if current + 1 < tracks.len() {
        slots.push(QueueSlot::Upcoming(tracks.len() - current - 1));
        for index in current + 1..tracks.len() {
            slots.push(QueueSlot::Track {
                index,
                playing: false,
            });
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
pub fn queue_drawer(root: &Root, cx: &mut Context<Root>) -> impl IntoElement {
    queue_drawer_inner(root, cx, false)
}

/// 歌词页内嵌队列：无侧边分割线，背景融入歌词页。
pub fn queue_panel(root: &Root, cx: &mut Context<Root>) -> impl IntoElement {
    queue_drawer_inner(root, cx, true)
}

/// 队列内容：主界面为 320px 抽屉；歌词页为无边界内嵌面板。
fn queue_drawer_inner(root: &Root, cx: &mut Context<Root>, embedded: bool) -> impl IntoElement {
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
                                move |event: &MouseDownEvent, _window, cx: &mut gpui::App| {
                                    let position = event.position;
                                    entity.update(cx, |root, cx| {
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

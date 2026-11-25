//! 听歌模式页。

use super::*;
use crate::app::SceneModesState;
use crate::ui::middle_drag::DragTarget;
/// 听歌模式页：常用模式 + 探索歌单，使用变高虚拟列表只渲染可视行。
pub(crate) fn scenes_view(root: &Root, cx: &mut Context<Root>) -> AnyElement {
    let scenes = root.scenes.clone();
    let language = root.language;
    let scene_texts: std::sync::Arc<Vec<String>> = std::sync::Arc::new(
        scenes
            .iter()
            .map(|scene| root.scene_display_text(&scene.sub_queue_type, &scene.text))
            .collect(),
    );
    let cards = root.scene_cards.clone();
    let covers = root.cover_store.paths_handle();
    let cover_mailbox = root.cover_mailbox.clone();
    let selected_scene = root
        .recommendation
        .as_ref()
        .and_then(|source| source.scene.as_ref())
        .map(|scene| scene.sub_queue_type.clone());
    let modes_state = root.scene_modes_state;
    let loading_cards = root.loading_scene_cards;
    let card_count = cards.len();
    let entity = cx.entity();
    let common_columns = root.scenes_common_columns.max(1);
    let card_columns = root.scenes_card_columns.max(1);
    let common_rows = scenes.len().div_ceil(common_columns).max(1);
    let card_rows = cards.len().div_ceil(card_columns).max(1);
    let width_slot = root.scenes_width.clone();

    // 整页加载只在常用模式正在读、探索卡也还空着时出现：探索卡单独在飞
    // 就留在列表里，让卡区自己显示加载态，别把整页伪装成「正在读取听歌模式」（#213）。
    if loading_cards && cards.is_empty() && modes_state == SceneModesState::Loading {
        return div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(loading_state(language.text("正在读取听歌模式…")))
            .into_any_element();
    }

    let list = gpui::list(root.scenes_list.clone(), move |index, _window, _cx| {
        // Header: 常用模式
        if index == 0 {
            div()
                .w_full()
                .h(px(44.0))
                .flex()
                .items_end()
                .pb(px(theme::space::MD))
                .child(section_title(language.text("常用模式")))
                .into_any_element()
        }
        // 常用模式虚拟网格行
        else if index <= common_rows {
            let row = index - 1;
            if scenes.is_empty() {
                // 三态分开：加载中转圈、失败给「不可用 + 重试」、加载完还空着就说实话。
                // 拿转圈顶替失败会让页面永远像在加载，用户没有可点的恢复动作（#213）。
                let state = match modes_state {
                    SceneModesState::Loading => loading_state(language.text("正在读取常用模式…")),
                    SceneModesState::Failed => div()
                        .flex()
                        .items_center()
                        .gap(px(theme::space::MD))
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(language.text("常用模式暂不可用").to_string())
                        .child(
                            compact_retry_pill(
                                "retry-scene-modes",
                                language.text("重试加载").to_string(),
                            )
                            .on_click({
                                let entity = entity.clone();
                                move |_event: &ClickEvent, _window, cx: &mut gpui::App| {
                                    entity.update(cx, |root, cx| {
                                        root.retry_scene_modes(cx);
                                    });
                                }
                            }),
                        )
                        .into_any_element(),
                    SceneModesState::Ready | SceneModesState::Idle => div()
                        .text_size(theme::Text::Small.size())
                        .text_color(theme::text_muted())
                        .child(language.text("暂无常用模式").to_string())
                        .into_any_element(),
                };
                return div()
                    .id("scene-modes-loading")
                    .w_full()
                    .h(px(theme::size::CONTROL + 20.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(state)
                    .into_any_element();
            }
            let mut row_element = div()
                .id(("scene-mode-row", row))
                .w_full()
                .flex()
                .flex_row()
                .gap(px(theme::space::MD))
                .h(px(theme::size::CONTROL + 20.0));

            for column in 0..common_columns {
                let Some(scene) = scenes.get(row * common_columns + column) else {
                    row_element = row_element.child(div().flex_1().min_w(px(0.0)));
                    continue;
                };
                let selected = selected_scene
                    .as_ref()
                    .is_some_and(|selected| selected == &scene.sub_queue_type);
                let click_scene = scene.clone();
                row_element = row_element.child(
                    div()
                        .id(gpui::ElementId::Name(
                            format!("scene-{}", scene.sub_queue_type).into(),
                        ))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(theme::space::SM))
                        .flex_1()
                        .min_w(px(0.0))
                        .h(px(theme::size::CONTROL + 12.0))
                        .px(px(theme::space::MD))
                        .rounded(px(theme::radius::CARD))
                        .bg(theme::surface_elevated())
                        .border_1()
                        .border_color(if selected {
                            theme::accent()
                        } else {
                            theme::border()
                        })
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::surface_hover()))
                        .on_click({
                            let entity = entity.clone();
                            move |_event: &ClickEvent, _window, cx: &mut gpui::App| {
                                entity.update(cx, |root, cx| {
                                    root.start_scene_queue(click_scene.clone(), cx);
                                });
                            }
                        })
                        .child(scene_image(&scene.sub_queue_type, 24.0))
                        .child(
                            div()
                                .min_w(px(0.0))
                                .truncate()
                                .text_size(theme::Text::Body.size())
                                .text_color(theme::text())
                                .child(
                                    scene_texts
                                        .get(row * common_columns + column)
                                        .cloned()
                                        .unwrap_or_else(|| scene.text.clone()),
                                ),
                        ),
                );
            }
            row_element.into_any_element()
        } else {
            let mut index = index - common_rows - 1;

            // Header: 探索更多新模式
            if index == 0 {
                return div()
                    .w_full()
                    .h(px(44.0))
                    .flex()
                    .items_end()
                    .pb(px(theme::space::MD))
                    .child(section_title(language.text("探索更多新模式")))
                    .into_any_element();
            }
            index -= 1;

            // 探索卡虚拟网格行
            if index < card_rows {
                let row = index;
                let mut row_element = div()
                    .id(("scene-card-row", row))
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(theme::space::LG))
                    .h(px(232.0));

                for column in 0..card_columns {
                    let Some(card) = cards.get(row * card_columns + column) else {
                        row_element = row_element.child(div().flex_1().min_w(px(0.0)));
                        continue;
                    };
                    let url = &card.playlist.cover;
                    let lookup = covers.lookup(url);
                    if lookup.should_request {
                        cover_mailbox.request(url);
                    }

                    let subtitle = if card.kind == sodam_core::library::SceneCardKind::Radio {
                        language.text("电台").to_string()
                    } else {
                        language.textf("{} 首", &[card.playlist.track_count.to_string()])
                    };
                    row_element = row_element.child(
                        div()
                            .id(gpui::ElementId::Name(
                                format!("scene-card-{}", card.inner_block_id).into(),
                            ))
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(theme::space::SM))
                            .flex_1()
                            .min_w(px(0.0))
                            .p(px(theme::space::SM))
                            .rounded(px(theme::radius::CARD))
                            .bg(theme::surface_elevated())
                            .border_1()
                            .border_color(theme::border())
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::surface_hover()))
                            .on_click({
                                let entity = entity.clone();
                                let card = card.clone();
                                move |_event: &ClickEvent, _window, cx: &mut gpui::App| {
                                    entity.update(cx, |root, cx| {
                                        root.start_scene_card(card.clone(), cx);
                                    });
                                }
                            })
                            .child(cover(lookup.path, 148.0, theme::radius::CARD))
                            .child(
                                div()
                                    .w_full()
                                    .truncate()
                                    .text_size(theme::Text::Body.size())
                                    .text_color(theme::text())
                                    .child(card.playlist.title.clone()),
                            )
                            .child(
                                div()
                                    .text_size(theme::Text::Small.size())
                                    .text_color(theme::text_muted())
                                    .child(subtitle),
                            ),
                    );
                }
                if cards.is_empty() {
                    let state = if loading_cards {
                        loading_state(language.text("正在读取探索歌单…"))
                    } else {
                        empty_state("list-music", language.text("暂无探索歌单"))
                    };
                    row_element = row_element.child(
                        div()
                            .flex_1()
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(state),
                    );
                }
                row_element.into_any_element()
            } else {
                // Load more row
                div()
                    .id("load-more-scene-cards-row")
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(theme::size::CONTROL + theme::space::MD))
                    .child(
                        div()
                            .id("load-more-scene-cards")
                            .flex()
                            .items_center()
                            .justify_center()
                            .h(px(theme::size::CONTROL))
                            .px(px(theme::space::XL))
                            .rounded(px(theme::radius::PILL))
                            .bg(theme::surface_elevated())
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::surface_hover()))
                            .on_click({
                                let entity = entity.clone();
                                move |_event: &ClickEvent, _window, cx: &mut gpui::App| {
                                    entity.update(cx, |root, cx| {
                                        root.load_scenes(cx);
                                    });
                                }
                            })
                            .text_size(theme::Text::Small.size())
                            .text_color(theme::text_muted())
                            .child(if loading_cards {
                                language.text("正在加载…").to_string()
                            } else {
                                language.textf(
                                    "加载更多探索歌单（当前 {} 张）",
                                    &[card_count.to_string()],
                                )
                            }),
                    )
                    .into_any_element()
            }
        }
    })
    .w_full()
    .flex_1()
    .min_h(px(0.0));

    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        // 按下时上报自己是这次拖动的目标；移动、松开由根元素统一收。
        .on_mouse_down(
            MouseButton::Middle,
            cx.listener(|root, event: &MouseDownEvent, _window, _cx| {
                root.begin_middle_drag_for(DragTarget::Scenes, f32::from(event.position.y));
            }),
        )
        .child(
            canvas(
                move |bounds: gpui::Bounds<gpui::Pixels>, _window, _cx| {
                    if let Ok(mut width) = width_slot.lock() {
                        *width = f32::from(bounds.size.width);
                    }
                },
                |_bounds, _state, _window, _cx| {},
            )
            .absolute()
            .size_full(),
        )
        .child(list)
        .into_any_element()
}

pub(crate) fn retry_button(
    language: crate::ui::i18n::Language,
    cx: &mut Context<Root>,
) -> AnyElement {
    compact_retry_pill(
        "retry-playback-assets",
        language.text("重试加载").to_string(),
    )
    .on_click(cx.listener(|root, _event: &ClickEvent, _window, cx| {
        if let Some(track) = root
            .pending_track
            .clone()
            .or_else(|| root.queue.current().cloned())
        {
            root.retry_playback_assets(track, cx);
        }
    }))
    .into_any_element()
}

fn section_title(text: &str) -> AnyElement {
    div()
        .text_size(theme::Text::Large.size())
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::text())
        .child(text.to_string())
        .into_any_element()
}

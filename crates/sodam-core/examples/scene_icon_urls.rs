//! 导出听歌模式内置图标下载清单。

use sodam_core::{Session, Settings};

fn main() -> anyhow::Result<()> {
    let session = Session::new(Settings::load().merged_with_env());
    let mode = session.soda().feed_mode()?;
    for block in mode.feed_mode_block {
        for item in block.feed_mode {
            let image = &item.url_info;
            let prefix = image.urls.first().cloned().unwrap_or_default();
            let uri = image.uri.trim();
            let template = image.template_prefix.trim();
            let url = if uri.is_empty() || template.is_empty() {
                format!("{prefix}{uri}")
            } else {
                format!("https://p3-luna.douyinpic.com/img/{uri}~{template}-resize:128:128.png")
            };
            println!("{}\t{url}", item.entity.feed_scene_mode.sub_queue_type);
        }
    }
    Ok(())
}

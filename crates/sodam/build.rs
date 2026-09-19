//! Windows：把应用图标嵌入 sodam.exe（快捷方式 / 任务栏 / 资源管理器显示）。
//! 非 Windows 目标直接跳过。

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/brand/sodam.ico");
    res.compile().expect("嵌入 Windows 图标资源失败");
}

use anyhow::{Context as _, Result, ensure};
use gpui::{HeadlessAppContext, Keystroke, PlatformHeadlessRenderer};
use gpui_wgpu::CosmicTextSystem;
use std::{path::Path, sync::Arc};

#[cfg(target_os = "macos")]
fn renderer() -> Result<Option<Box<dyn PlatformHeadlessRenderer>>> {
    Ok(Some(Box::new(
        gpui_macos::metal_renderer::MetalHeadlessRenderer::new(),
    )))
}

#[cfg(not(target_os = "macos"))]
fn renderer() -> Result<Option<Box<dyn PlatformHeadlessRenderer>>> {
    let renderer = gpui_wgpu::WgpuHeadlessRenderer::new()
        .context("initialize GPUI's offscreen renderer with a Vulkan or OpenGL adapter")?;
    Ok(Some(Box::new(renderer)))
}

#[test]
#[ignore = "requires an offscreen GPU renderer; run with just screenshot"]
fn capture() -> Result<()> {
    let scenario = std::env::var("M4CODE_SCREENSHOT_SCENARIO").unwrap_or_else(|_| "home".into());
    let shortcut = match scenario.as_str() {
        "home" => None,
        "settings" => Some(Keystroke::parse("secondary-,").expect("Settings shortcut")),
        _ => anyhow::bail!("unknown screenshot scenario {scenario:?}; use home or settings"),
    };

    let fixture = tempfile::tempdir()?;
    // The recipe selects only this test. Set paths before initializing GPUI or starting app tasks.
    unsafe {
        std::env::set_var("M4CODE_DATA_DIR", fixture.path().join("m4code"));
        std::env::set_var("CLAUDE_CONFIG_DIR", fixture.path().join("claude"));
        std::env::set_var("CODEX_HOME", fixture.path().join("codex"));
    }
    let path = crate::store::Store::default_path();
    let mut store = crate::store::Store::load(path.clone());
    let settings = &mut store.data.settings;
    // Keep the fixture offline and independent of installed CLIs, usage counters, and live model catalogs.
    settings.update_url = Some(String::new());
    settings.enabled_harnesses = Some(Vec::new());
    settings.custom_subagents = crate::harness::subagents::CustomSubagents(Default::default());
    crate::datadir::write_atomic(&path, &serde_json::to_string(&store.data)?)?;

    let font = if cfg!(windows) {
        "Segoe UI"
    } else if cfg!(target_os = "macos") {
        "Helvetica"
    } else {
        "sans-serif"
    };
    let mut cx = HeadlessAppContext::with_platform(
        Arc::new(CosmicTextSystem::new(font)),
        Arc::new(crate::Assets),
        renderer,
    );
    let (window, _) = cx.update(|app| {
        crate::init_ui(app);
        crate::open_main_window(Vec::new(), app)
    });
    cx.run_until_parked();
    if let Some(shortcut) = shortcut {
        let handled = cx.update_window(window.into(), |_, window, app| {
            window.dispatch_keystroke(shortcut, app)
        })?;
        ensure!(handled, "the Settings shortcut was not handled");
        cx.run_until_parked();
    }
    cx.update_window(window.into(), |_, window, app| window.draw(app).clear(app))?;
    let frame = cx.capture_screenshot(window.into())?;
    ensure!(
        frame.pixels().any(|pixel| pixel != frame.get_pixel(0, 0)),
        "GPUI rendered a blank frame"
    );

    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/screenshots");
    std::fs::create_dir_all(&directory)?;
    let output = directory.join(format!("{scenario}.png"));
    let mut png = tempfile::NamedTempFile::new_in(directory)?;
    frame.write_to(&mut png, image::ImageFormat::Png)?;
    png.persist(&output)?;
    println!(
        "{} ({}×{})",
        output.display(),
        frame.width(),
        frame.height()
    );
    Ok(())
}

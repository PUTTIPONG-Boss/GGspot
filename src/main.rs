#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;
mod model;
mod ui;

use eframe::egui;

fn main() -> eframe::Result {
    // Cached library is read before the window opens, so the first frame already has music.
    let lib = model::Library::load();
    let cfg = model::Config::load();
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("GGspot")
            .with_inner_size([880.0, 580.0])
            .with_min_inner_size([520.0, 320.0])
            .with_icon(egui::IconData {
                rgba: include_bytes!("../assets/icon64.rgba").to_vec(),
                width: 64,
                height: 64,
            }),
        ..Default::default()
    };
    eframe::run_native(
        "GGspot",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_theme(egui::Theme::Dark);
            ui::install_style(&cc.egui_ctx);
            backend::spawn(cmd_rx, ev_tx, cc.egui_ctx.clone(), lib.clone());
            if let Some(v) = cfg.last_view.clone().filter(|v| v != model::LIKED) {
                let _ = cmd_tx.send(backend::Cmd::Open(v));
            }
            Ok(Box::new(ui::App::new(lib, &cfg, cmd_tx, ev_rx)))
        }),
    )
}

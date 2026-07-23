//! 最小限のGUI(`egui`/`eframe`製、Tauriには依存しない——このエコ
//! システム全体の既存方針「Tauriパッケージには直接依存しない」に
//! 準拠)。
//!
//! **同意の設計方針(ユーザー指示)**: 「速度測定」ボタンを押す行為
//! そのものが同意であり、別途の確認ダイアログは出さない。ボタンを
//! 押さなければ測定は一切実行されない(「速度測定しないで良い人は、
//! そのボタンを押さなければ良い」)。ボタン近くに静的な注記
//! (M-Labへ接続しIPが共有される旨)だけを表示する。
//!
//! 「自動測定」チェックボックスをONにすると、確認なしで一定間隔ごとに
//! 測定・記録を繰り返す(ユーザー指示「自動測定・自動記録(対話確認
//! なしで定期的に実行・記録)」)。

use crate::speedtest::{self, NetworkEnvironment, SpeedTestRecord};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

const AUTO_INTERVAL: Duration = Duration::from_secs(60 * 60);

enum GuiEvent {
    MeasurementStarted,
    MeasurementFinished(anyhow::Result<SpeedTestRecord>),
}

struct RsLinkFusionApp {
    runtime: tokio::runtime::Runtime,
    history_path: PathBuf,
    history: Vec<SpeedTestRecord>,
    environment: Option<NetworkEnvironment>,
    measuring: bool,
    last_error: Option<String>,
    auto_enabled: bool,
    event_tx: mpsc::Sender<GuiEvent>,
    event_rx: mpsc::Receiver<GuiEvent>,
    auto_stop_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl RsLinkFusionApp {
    fn new(history_path: PathBuf) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("failed to start tokio runtime for GUI");
        let history = speedtest::load_history(&history_path).unwrap_or_default();
        let environment = speedtest::detect_environment().ok();
        let (event_tx, event_rx) = mpsc::channel();
        Self {
            runtime,
            history_path,
            history,
            environment,
            measuring: false,
            last_error: None,
            auto_enabled: false,
            event_tx,
            event_rx,
            auto_stop_tx: None,
        }
    }

    fn start_measurement(&mut self) {
        if self.measuring {
            return;
        }
        self.measuring = true;
        self.last_error = None;
        let _ = self.event_tx.send(GuiEvent::MeasurementStarted);
        let tx = self.event_tx.clone();
        let history_path = self.history_path.clone();
        self.runtime.spawn(async move {
            let result = speedtest::run("gui".to_string(), &history_path, true).await;
            let _ = tx.send(GuiEvent::MeasurementFinished(result));
        });
    }

    fn set_auto(&mut self, enabled: bool) {
        self.auto_enabled = enabled;
        if enabled {
            let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel();
            self.auto_stop_tx = Some(stop_tx);
            let tx = self.event_tx.clone();
            let history_path = self.history_path.clone();
            self.runtime.spawn(async move {
                let mut ticker = tokio::time::interval(AUTO_INTERVAL);
                loop {
                    tokio::select! {
                        _ = &mut stop_rx => break,
                        _ = ticker.tick() => {
                            let _ = tx.send(GuiEvent::MeasurementStarted);
                            let result = speedtest::run("auto".to_string(), &history_path, true).await;
                            let _ = tx.send(GuiEvent::MeasurementFinished(result));
                        }
                    }
                }
            });
        } else if let Some(stop_tx) = self.auto_stop_tx.take() {
            let _ = stop_tx.send(());
        }
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                GuiEvent::MeasurementStarted => {
                    self.measuring = true;
                }
                GuiEvent::MeasurementFinished(result) => {
                    self.measuring = false;
                    match result {
                        Ok(record) => {
                            self.history.push(record);
                        }
                        Err(e) => {
                            self.last_error = Some(e.to_string());
                        }
                    }
                }
            }
        }
    }
}

impl eframe::App for RsLinkFusionApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        ctx.request_repaint_after(Duration::from_millis(500));

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("RS-LinkFusion");
            ui.separator();

            if let Some(env) = &self.environment {
                ui.label(format!(
                    "検出されたネットワークインターフェース: 合計{}個(有線{}・無線{}・その他{})",
                    env.interface_count, env.ethernet_count, env.wifi_count, env.other_count
                ));
                for name in &env.interface_names {
                    ui.label(format!("  - {name}"));
                }
            }

            ui.add_space(8.0);
            ui.label("速度測定はM-Lab(Measurement Lab)の公開インフラへ接続し、IPアドレスが共有されます。");
            ui.label("測定したくない場合は、下のボタンを押さないでください。");

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let button = egui::Button::new("速度測定を実行");
                if ui.add_enabled(!self.measuring, button).clicked() {
                    self.start_measurement();
                }
                if self.measuring {
                    ui.spinner();
                    ui.label("測定中...");
                }
            });

            let mut auto = self.auto_enabled;
            if ui.checkbox(&mut auto, "自動測定(1時間ごと、確認なしで測定・記録)").changed() {
                self.set_auto(auto);
            }

            if let Some(err) = &self.last_error {
                ui.colored_label(egui::Color32::RED, format!("測定エラー: {err}"));
            }

            ui.separator();
            ui.label("参考: 以下は非公式サイトのため自動測定は行いません。手動で開いて読み取った値を記録できます。");
            for (name, url) in speedtest::MANUAL_REFERENCE_SITES {
                ui.hyperlink_to(format!("{name}: {url}"), *url);
            }

            ui.separator();
            ui.label(format!("測定履歴({}件、直近5件を表示):", self.history.len()));
            for record in self.history.iter().rev().take(5) {
                ui.label(format!(
                    "[{}] {} / {}: down {:.1} Mbps, up {:.1} Mbps",
                    record.recorded_at.format("%Y-%m-%d %H:%M:%S"),
                    record.source,
                    record.label,
                    record.download_mbps,
                    record.upload_mbps
                ));
            }
        });
    }
}

/// GUIウィンドウを起動する(呼び出しはブロッキング、ウィンドウが
/// 閉じられるまで戻らない)。
pub fn run(history_path: PathBuf) -> anyhow::Result<()> {
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "RS-LinkFusion",
        options,
        Box::new(move |_cc| Ok(Box::new(RsLinkFusionApp::new(history_path)))),
    )
    .map_err(|e| anyhow::anyhow!("GUI起動に失敗しました: {e}"))
}

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::env;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use bkr1x_hal::{
    InstallOptions, InstallSummary, TrayIconStatus, install_with_options,
    is_user_application_installed, render_mouse_icon_rgba, uninstall_user_application_full,
    user_install_dir,
};
use eframe::egui::{self, Color32, RichText, Stroke};

const WIZARD_TITLE: &str = "Fire Phoenix BK-R1X — Setup Wizard";

const BG_DARK: Color32 = Color32::from_rgb(22, 22, 30);
const PANEL_BG: Color32 = Color32::from_rgb(30, 32, 46);
const CARD_BORDER: Color32 = Color32::from_rgb(56, 62, 86);
const ACCENT_BLUE: Color32 = Color32::from_rgb(122, 162, 247);
const ACCENT_CYAN: Color32 = Color32::from_rgb(42, 195, 222);
const ACCENT_GREEN: Color32 = Color32::from_rgb(158, 206, 106);
const ACCENT_RED: Color32 = Color32::from_rgb(247, 118, 142);
const TEXT_MUTED: Color32 = Color32::from_rgb(150, 158, 188);

const DEFAULT_WIZARD_ICON_STATUS: TrayIconStatus = TrayIconStatus {
    connected: true,
    online: true,
    battery_percent: 100,
    charging: true,
};

#[derive(Debug)]
enum WizardStage {
    Ready,
    Working { label: &'static str },
    Installed(InstallSummary),
    Uninstalled,
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WizardAction {
    None,
    StartInstall,
    StartUninstall,
    ResetReady,
    CloseWindow,
}

enum TaskOutcome {
    Installed(Result<InstallSummary, String>),
    Uninstalled(Result<(), String>),
}

struct SetupWizardApp {
    uninstall_mode: bool,
    already_installed: bool,
    opts: InstallOptions,
    launch_after_install: bool,
    stage: WizardStage,
    logo_texture: Option<egui::TextureHandle>,
    result_rx: Receiver<TaskOutcome>,
    result_tx: Sender<TaskOutcome>,
}

impl SetupWizardApp {
    fn new(cc: &eframe::CreationContext<'_>, uninstall_mode: bool) -> Self {
        configure_wizard_theme(&cc.egui_ctx);

        let logo_rgba = render_mouse_icon_rgba(64, DEFAULT_WIZARD_ICON_STATUS);
        let color_image = egui::ColorImage::from_rgba_unmultiplied([64, 64], &logo_rgba);
        let logo_texture = Some(cc.egui_ctx.load_texture(
            "bkr1x-setup-logo",
            color_image,
            egui::TextureOptions::LINEAR,
        ));

        let (result_tx, result_rx) = mpsc::channel();

        Self {
            uninstall_mode,
            already_installed: is_user_application_installed(),
            opts: InstallOptions::default(),
            launch_after_install: true,
            stage: WizardStage::Ready,
            logo_texture,
            result_rx,
            result_tx,
        }
    }

    fn start_install(&mut self, ctx: &egui::Context) {
        let opts = self.opts;
        let launch_after = self.launch_after_install;
        let tx = self.result_tx.clone();
        let egui_ctx = ctx.clone();

        self.stage = WizardStage::Working {
            label: "Extracting native binaries & registering Windows shortcuts...",
        };

        if let Err(err) = thread::Builder::new()
            .name("bkr1x-setup-worker".to_owned())
            .spawn(move || {
                let res = install_with_options(&opts);
                if let Ok(ref summary) = res
                    && launch_after
                {
                    let _ = spawn_detached(&summary.daemon_exe, &["--silent-if-running"]);
                    let _ = spawn_detached(&summary.gui_exe, &[]);
                }
                let _ = tx.send(TaskOutcome::Installed(res));
                egui_ctx.request_repaint();
            })
        {
            self.stage = WizardStage::Failed(format!("failed spawning setup worker: {err}"));
        }
    }

    fn start_uninstall(&mut self, ctx: &egui::Context) {
        let tx = self.result_tx.clone();
        let egui_ctx = ctx.clone();

        self.stage = WizardStage::Working {
            label: "Stopping System Tray agent & removing shortcuts...",
        };

        if let Err(err) = thread::Builder::new()
            .name("bkr1x-uninst-worker".to_owned())
            .spawn(move || {
                let res = uninstall_user_application_full(true);
                let _ = tx.send(TaskOutcome::Uninstalled(res));
                egui_ctx.request_repaint();
            })
        {
            self.stage = WizardStage::Failed(format!("failed spawning uninstall worker: {err}"));
        }
    }

    fn poll_worker(&mut self) {
        while let Ok(outcome) = self.result_rx.try_recv() {
            match outcome {
                TaskOutcome::Installed(Ok(summary)) => {
                    self.already_installed = true;
                    self.stage = WizardStage::Installed(summary);
                }
                TaskOutcome::Installed(Err(err)) => {
                    self.stage = WizardStage::Failed(err);
                }
                TaskOutcome::Uninstalled(Ok(())) => {
                    self.already_installed = false;
                    self.stage = WizardStage::Uninstalled;
                }
                TaskOutcome::Uninstalled(Err(err)) => {
                    self.stage = WizardStage::Failed(err);
                }
            }
        }
    }

    fn render_header(&self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(PANEL_BG)
            .stroke(Stroke::new(1.0_f32, CARD_BORDER))
            .corner_radius(10.0)
            .inner_margin(14.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some(ref tex) = self.logo_texture {
                        ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(48.0, 48.0)));
                        ui.add_space(10.0);
                    }
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("Fire Phoenix BK-R1X")
                                .size(19.0)
                                .strong()
                                .color(Color32::WHITE),
                        );
                        ui.label(
                            RichText::new(format!(
                                "v{} · Native PAW3311 Driver, System Tray & GUI (< 2 MB RAM)",
                                env!("CARGO_PKG_VERSION")
                            ))
                            .size(12.0)
                            .color(ACCENT_CYAN),
                        );
                    });
                });
            });
    }

    fn render_install_options(&mut self, ui: &mut egui::Ui) -> WizardAction {
        let dest = user_install_dir();
        egui::Frame::new()
            .fill(PANEL_BG)
            .stroke(Stroke::new(1.0_f32, CARD_BORDER))
            .corner_radius(10.0)
            .inner_margin(14.0)
            .show(ui, |ui| {
                ui.label(
                    RichText::new(format!("Destination: {}", dest.display()))
                        .size(12.0)
                        .color(TEXT_MUTED),
                );
                ui.label(
                    RichText::new(
                        "Per-user installation — no Administrator / UAC prompt required.",
                    )
                    .size(11.5)
                    .color(TEXT_MUTED),
                );
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(8.0);

                ui.checkbox(
                    &mut self.opts.enable_autostart,
                    "Launch System Tray icon automatically when Windows starts",
                );
                ui.add_space(4.0);
                ui.checkbox(
                    &mut self.opts.create_start_menu_shortcut,
                    "Create Windows Start Menu shortcut",
                );
                ui.add_space(4.0);
                ui.checkbox(
                    &mut self.opts.create_desktop_shortcut,
                    "Create Desktop shortcut",
                );
                ui.add_space(4.0);
                ui.checkbox(
                    &mut self.launch_after_install,
                    "Start System Tray & open BK-R1X Configurator now",
                );
            });

        ui.add_space(16.0);
        let mut action = WizardAction::None;
        ui.horizontal(|ui| {
            let primary_label = if self.already_installed {
                "Update / Reinstall BK-R1X"
            } else {
                "Install Fire Phoenix BK-R1X"
            };

            if ui
                .add_sized(
                    [250.0, 38.0],
                    egui::Button::new(
                        RichText::new(primary_label)
                            .size(14.0)
                            .strong()
                            .color(Color32::from_rgb(16, 18, 26)),
                    )
                    .fill(ACCENT_BLUE),
                )
                .clicked()
            {
                action = WizardAction::StartInstall;
            }

            if self.already_installed
                && ui
                    .add_sized(
                        [110.0, 38.0],
                        egui::Button::new(RichText::new("Uninstall").color(ACCENT_RED)),
                    )
                    .clicked()
            {
                action = WizardAction::StartUninstall;
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_sized([90.0, 38.0], egui::Button::new("Cancel"))
                    .clicked()
                {
                    action = WizardAction::CloseWindow;
                }
            });
        });
        action
    }

    fn render_uninstall_confirm(&self, ui: &mut egui::Ui) -> WizardAction {
        egui::Frame::new()
            .fill(PANEL_BG)
            .stroke(Stroke::new(1.0_f32, CARD_BORDER))
            .corner_radius(10.0)
            .inner_margin(16.0)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("Remove Fire Phoenix BK-R1X from your computer?")
                        .size(15.0)
                        .strong()
                        .color(Color32::WHITE),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "This stops the background System Tray agent, removes the Windows Startup \
                         and Installed Apps entries, and deletes installed binaries.\n\
                         Your saved mouse profiles in %APPDATA%\\bkr1x\\profiles are preserved.",
                    )
                    .color(TEXT_MUTED),
                );
            });

        ui.add_space(18.0);
        let mut action = WizardAction::None;
        ui.horizontal(|ui| {
            if ui
                .add_sized(
                    [220.0, 38.0],
                    egui::Button::new(
                        RichText::new("Uninstall BK-R1X")
                            .size(14.0)
                            .strong()
                            .color(Color32::from_rgb(20, 14, 18)),
                    )
                    .fill(ACCENT_RED),
                )
                .clicked()
            {
                action = WizardAction::StartUninstall;
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_sized([100.0, 38.0], egui::Button::new("Cancel"))
                    .clicked()
                {
                    action = WizardAction::CloseWindow;
                }
            });
        });
        action
    }
}

impl eframe::App for SetupWizardApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_worker();

        let mut action = WizardAction::None;

        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(&ctx.style()).inner_margin(20.0))
            .show(ctx, |ui| {
                self.render_header(ui);
                ui.add_space(14.0);

                match &self.stage {
                    WizardStage::Ready => {
                        action = if self.uninstall_mode {
                            self.render_uninstall_confirm(ui)
                        } else {
                            self.render_install_options(ui)
                        };
                    }
                    WizardStage::Working { label } => {
                        let label = *label;
                        egui::Frame::new()
                            .fill(PANEL_BG)
                            .stroke(Stroke::new(1.0_f32, CARD_BORDER))
                            .corner_radius(10.0)
                            .inner_margin(22.0)
                            .show(ui, |ui| {
                                ui.vertical_centered(|ui| {
                                    ui.add_space(12.0);
                                    ui.spinner();
                                    ui.add_space(12.0);
                                    ui.label(
                                        RichText::new(label)
                                            .size(14.0)
                                            .strong()
                                            .color(Color32::WHITE),
                                    );
                                    ui.add_space(12.0);
                                });
                            });
                    }
                    WizardStage::Installed(summary) => {
                        egui::Frame::new()
                            .fill(PANEL_BG)
                            .stroke(Stroke::new(1.0_f32, ACCENT_GREEN))
                            .corner_radius(10.0)
                            .inner_margin(16.0)
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new("✔ Installation Complete!")
                                        .size(16.0)
                                        .strong()
                                        .color(ACCENT_GREEN),
                                );
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new(format!(
                                        "Installed to: {}",
                                        summary.install_dir.display()
                                    ))
                                    .size(12.0)
                                    .color(TEXT_MUTED),
                                );
                                ui.add_space(8.0);
                                ui.label(
                                    "• Your BK-R1X mouse icon is now running in the bottom-right taskbar tray (^).\n\
                                     • Left-click the tray icon anytime to open the Configurator.\n\
                                     • Right-click the tray icon for 1-click DPI, Polling Rate, and Profile switching.",
                                );
                            });

                        ui.add_space(16.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_sized(
                                    [140.0, 36.0],
                                    egui::Button::new(
                                        RichText::new("Finish")
                                            .strong()
                                            .color(Color32::from_rgb(16, 18, 26)),
                                    )
                                    .fill(ACCENT_GREEN),
                                )
                                .clicked()
                            {
                                action = WizardAction::CloseWindow;
                            }
                        });
                    }
                    WizardStage::Uninstalled => {
                        egui::Frame::new()
                            .fill(PANEL_BG)
                            .stroke(Stroke::new(1.0_f32, ACCENT_CYAN))
                            .corner_radius(10.0)
                            .inner_margin(18.0)
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new("✔ Fire Phoenix BK-R1X Uninstalled")
                                        .size(16.0)
                                        .strong()
                                        .color(ACCENT_CYAN),
                                );
                                ui.add_space(8.0);
                                ui.label(
                                    "Removed the background System Tray daemon, Windows Startup registration, \
                                     Start Menu / Desktop shortcuts, and program files.",
                                );
                            });

                        ui.add_space(18.0);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_sized([130.0, 36.0], egui::Button::new("Close"))
                                .clicked()
                            {
                                action = WizardAction::CloseWindow;
                            }
                        });
                    }
                    WizardStage::Failed(err) => {
                        egui::Frame::new()
                            .fill(PANEL_BG)
                            .stroke(Stroke::new(1.0_f32, ACCENT_RED))
                            .corner_radius(10.0)
                            .inner_margin(16.0)
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new("Setup Encountered an Error")
                                        .size(15.0)
                                        .strong()
                                        .color(ACCENT_RED),
                                );
                                ui.add_space(6.0);
                                ui.label(err);
                            });

                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            if ui.button("Back").clicked() {
                                action = WizardAction::ResetReady;
                            }
                            if ui.button("Close").clicked() {
                                action = WizardAction::CloseWindow;
                            }
                        });
                    }
                }
            });

        match action {
            WizardAction::None => {}
            WizardAction::StartInstall => self.start_install(ctx),
            WizardAction::StartUninstall => self.start_uninstall(ctx),
            WizardAction::ResetReady => self.stage = WizardStage::Ready,
            WizardAction::CloseWindow => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }
}

fn configure_wizard_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG_DARK;
    visuals.window_fill = BG_DARK;
    visuals.widgets.noninteractive.bg_fill = PANEL_BG;
    visuals.selection.bg_fill = ACCENT_BLUE.gamma_multiply(0.35);
    visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT_BLUE);
    ctx.set_visuals(visuals);
}

fn spawn_detached(exe: &Path, args: &[&str]) -> Result<(), String> {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    if let Some(parent) = exe.parent() {
        cmd.current_dir(parent);
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }

    cmd.spawn()
        .map(|_| ())
        .map_err(|err| format!("failed launching {}: {err}", exe.display()))
}

fn main() -> ExitCode {
    let mut silent = false;
    let mut uninstall_mode = false;
    for arg in env::args().skip(1) {
        match arg.as_str() {
            "--silent" | "/S" | "/quiet" => silent = true,
            "--uninstall" | "/uninstall" | "/U" => uninstall_mode = true,
            _ => {}
        }
    }

    if silent {
        let res = if uninstall_mode {
            uninstall_user_application_full(true)
        } else {
            install_with_options(&InstallOptions::default()).map(|summary| {
                let _ = spawn_detached(&summary.daemon_exe, &["--silent-if-running"]);
            })
        };
        return match res {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        };
    }

    let icon_rgba = render_mouse_icon_rgba(64, DEFAULT_WIZARD_ICON_STATUS);
    let app_icon = egui::IconData {
        rgba: icon_rgba,
        width: 64,
        height: 64,
    };

    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        vsync: true,
        viewport: egui::ViewportBuilder::default()
            .with_title(WIZARD_TITLE)
            .with_icon(app_icon)
            .with_inner_size([520.0, 395.0])
            .with_min_inner_size([520.0, 395.0])
            .with_max_inner_size([520.0, 420.0])
            .with_resizable(false)
            .with_maximize_button(false),
        ..Default::default()
    };

    match eframe::run_native(
        "bkr1x-installer",
        native_options,
        Box::new(move |cc| Ok(Box::new(SetupWizardApp::new(cc, uninstall_mode)))),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

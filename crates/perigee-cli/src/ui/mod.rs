pub mod common;
pub mod home;
pub mod tui;

use anyhow::Result;
use crossterm::event::{self, Event, KeyEventKind};
use ratatui::DefaultTerminal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppScreen {
    Home,
    SriovProfiles,
    SriovStatus(usize),
    SriovEditor(usize),
    SriovNewEditor,
}

pub struct AppState {
    pub screen: AppScreen,
    pub should_quit: bool,
    pub daemon_online: bool,
    pub sriov_state: perigee_sriov::ui::SriovState,
    pub host_info: perigee_core::sysinfo::HostInfo,
    host_info_rx: Option<tokio::sync::oneshot::Receiver<perigee_core::sysinfo::HostInfo>>,
}

impl AppState {
    pub fn new() -> Self {
        let daemon_online = perigee_core::client::IpcClient::is_daemon_running();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(perigee_core::sysinfo::HostInfo::gather());
        });
        Self {
            screen: AppScreen::Home,
            should_quit: false,
            daemon_online,
            sriov_state: perigee_sriov::ui::SriovState::new(),
            host_info: perigee_core::sysinfo::HostInfo::default(),
            host_info_rx: Some(rx),
        }
    }

    pub fn poll_host_info(&mut self) {
        if let Some(mut rx) = self.host_info_rx.take() {
            match rx.try_recv() {
                Ok(info) => self.host_info = info,
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    self.host_info_rx = Some(rx);
                }
                Err(_) => {}
            }
        }
    }
}

/// Main TUI entry point (from `perigee` with no args).
pub async fn run_app() -> Result<()> {
    let mut terminal = tui::init()?;
    let mut state = AppState::new();
    let result = main_loop(&mut terminal, &mut state).await;
    tui::restore()?;
    result
}

/// SR-IOV TUI entry (from `perigee sriov`).
pub async fn run_sriov_tui() -> Result<()> {
    let mut terminal = tui::init()?;
    let mut state = AppState::new();
    state.screen = AppScreen::SriovProfiles;
    state.sriov_state.load_profiles();
    state.sriov_state.fetch_profile_statuses().await;
    let result = main_loop(&mut terminal, &mut state).await;
    tui::restore()?;
    result
}

async fn main_loop(terminal: &mut DefaultTerminal, state: &mut AppState) -> Result<()> {
    while !state.should_quit {
        state.poll_host_info();
        terminal.draw(|frame| {
            match state.screen {
                AppScreen::Home => home::render(frame, state),
                AppScreen::SriovProfiles => {
                    perigee_sriov::ui::render_profiles(frame, state.daemon_online, &state.sriov_state)
                }
                AppScreen::SriovStatus(idx) => {
                    perigee_sriov::ui::render_status(frame, state.daemon_online, &state.sriov_state, idx)
                }
                AppScreen::SriovEditor(idx) => {
                    perigee_sriov::ui::render_editor(frame, state.daemon_online, &state.sriov_state, idx)
                }
                AppScreen::SriovNewEditor => {
                    perigee_sriov::ui::render_editor(frame, state.daemon_online, &state.sriov_state, usize::MAX)
                }
            }
        })?;

        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match state.screen {
                    AppScreen::Home => home::handle_input(state, key).await,
                    AppScreen::SriovProfiles => {
                        let action = perigee_sriov::ui::handle_profiles_input(
                            &mut state.sriov_state, key,
                        ).await;
                        apply_sriov_action(state, action);
                    }
                    AppScreen::SriovStatus(idx) => {
                        let action = perigee_sriov::ui::handle_status_input(
                            &mut state.sriov_state, key, idx,
                        ).await;
                        apply_sriov_action(state, action);
                    }
                    AppScreen::SriovEditor(idx) => {
                        let action = perigee_sriov::ui::handle_editor_input(
                            &mut state.sriov_state, key, Some(idx),
                        ).await;
                        apply_sriov_action(state, action);
                    }
                    AppScreen::SriovNewEditor => {
                        let action = perigee_sriov::ui::handle_editor_input(
                            &mut state.sriov_state, key, None,
                        ).await;
                        apply_sriov_action(state, action);
                    }
                }
            }
        }
    }
    Ok(())
}

fn apply_sriov_action(state: &mut AppState, action: perigee_sriov::ui::SriovUiAction) {
    use perigee_sriov::ui::{SriovUiAction, SriovScreen};
    match action {
        SriovUiAction::None => {}
        SriovUiAction::GoBack => {
            state.screen = AppScreen::Home;
        }
        SriovUiAction::NavigateTo(screen) => {
            state.screen = match screen {
                SriovScreen::Profiles => AppScreen::SriovProfiles,
                SriovScreen::Status(i) => AppScreen::SriovStatus(i),
                SriovScreen::Editor(i) => AppScreen::SriovEditor(i),
                SriovScreen::NewEditor => AppScreen::SriovNewEditor,
            };
        }
    }
}

pub mod common;
pub mod home;
pub mod sriov;
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
    pub sriov_state: sriov::SriovState,
    pub host_info: perigee_core::sysinfo::HostInfo,
    host_info_rx: Option<tokio::sync::oneshot::Receiver<perigee_core::sysinfo::HostInfo>>,
}

impl AppState {
    pub fn new() -> Self {
        let daemon_online = crate::client::IpcClient::is_daemon_running();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(perigee_core::sysinfo::HostInfo::gather());
        });
        Self {
            screen: AppScreen::Home,
            should_quit: false,
            daemon_online,
            sriov_state: sriov::SriovState::new(),
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
                AppScreen::SriovProfiles => sriov::render_profiles(frame, state),
                AppScreen::SriovStatus(idx) => sriov::render_status(frame, state, idx),
                AppScreen::SriovEditor(idx) => sriov::render_editor(frame, state, idx),
                AppScreen::SriovNewEditor => sriov::render_editor(frame, state, usize::MAX),
            }
        })?;

        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match state.screen {
                    AppScreen::Home => home::handle_input(state, key).await,
                    AppScreen::SriovProfiles => sriov::handle_profiles_input(state, key).await,
                    AppScreen::SriovStatus(idx) => {
                        sriov::handle_status_input(state, key, idx).await
                    }
                    AppScreen::SriovEditor(idx) => {
                        sriov::handle_editor_input(state, key, Some(idx)).await
                    }
                    AppScreen::SriovNewEditor => {
                        sriov::handle_editor_input(state, key, None).await
                    }
                }
            }
        }
    }
    Ok(())
}

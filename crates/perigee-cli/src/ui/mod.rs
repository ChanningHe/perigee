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
    AffinityTopology,
    AffinityStrategy,
    AffinityApply,
    AffinityAutoApply,
}

pub struct AppState {
    pub screen: AppScreen,
    pub should_quit: bool,
    pub daemon_online: bool,
    pub home_cursor: usize,
    pub daemon_message: Option<String>,
    pub sriov_state: perigee_sriov::ui::SriovState,
    pub affinity_state: perigee_affinity::ui::AffinityState,
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
        let mut affinity_state = perigee_affinity::ui::AffinityState::new();
        affinity_state.preload();

        Self {
            screen: AppScreen::Home,
            should_quit: false,
            daemon_online,
            home_cursor: 0,
            daemon_message: None,
            sriov_state: perigee_sriov::ui::SriovState::new(),
            affinity_state,
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
    let (mut terminal, _guard) = tui::init()?;
    let mut state = AppState::new();
    main_loop(&mut terminal, &mut state).await
}

/// SR-IOV TUI entry (from `perigee sriov`).
pub async fn run_sriov_tui() -> Result<()> {
    let (mut terminal, _guard) = tui::init()?;
    let mut state = AppState::new();
    state.screen = AppScreen::SriovProfiles;
    state.sriov_state.load_profiles();
    state.sriov_state.fetch_profile_statuses().await;
    main_loop(&mut terminal, &mut state).await
}

/// CPU Affinity TUI entry (from `perigee affinity`).
pub async fn run_affinity_tui() -> Result<()> {
    let (mut terminal, _guard) = tui::init()?;
    let mut state = AppState::new();
    state.screen = AppScreen::AffinityTopology;
    // preload() already called in AppState::new(); if data arrived, great;
    // otherwise topology_view will show "Loading..." until poll completes.
    main_loop(&mut terminal, &mut state).await
}

async fn main_loop(terminal: &mut DefaultTerminal, state: &mut AppState) -> Result<()> {
    while !state.should_quit {
        state.poll_host_info();
        state.affinity_state.poll_preload();
        terminal.draw(|frame| match state.screen {
            AppScreen::Home => home::render(frame, state),
            AppScreen::SriovProfiles => perigee_sriov::ui::render_profiles(
                frame,
                state.daemon_online,
                &mut state.sriov_state,
            ),
            AppScreen::SriovStatus(idx) => perigee_sriov::ui::render_status(
                frame,
                state.daemon_online,
                &state.sriov_state,
                idx,
            ),
            AppScreen::SriovEditor(idx) => perigee_sriov::ui::render_editor(
                frame,
                state.daemon_online,
                &state.sriov_state,
                idx,
            ),
            AppScreen::SriovNewEditor => perigee_sriov::ui::render_editor(
                frame,
                state.daemon_online,
                &state.sriov_state,
                usize::MAX,
            ),
            AppScreen::AffinityTopology => perigee_affinity::ui::render_topology(
                frame,
                state.daemon_online,
                &mut state.affinity_state,
            ),
            AppScreen::AffinityStrategy => perigee_affinity::ui::render_strategy(
                frame,
                state.daemon_online,
                &state.affinity_state,
            ),
            AppScreen::AffinityApply => perigee_affinity::ui::render_apply(
                frame,
                state.daemon_online,
                &mut state.affinity_state,
            ),
            AppScreen::AffinityAutoApply => perigee_affinity::ui::render_auto_apply(
                frame,
                state.daemon_online,
                &state.affinity_state,
            ),
        })?;

        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match state.screen {
                    AppScreen::Home => home::handle_input(state, key).await,
                    AppScreen::SriovProfiles => {
                        let action =
                            perigee_sriov::ui::handle_profiles_input(&mut state.sriov_state, key)
                                .await;
                        apply_sriov_action(state, action);
                    }
                    AppScreen::SriovStatus(idx) => {
                        let action = perigee_sriov::ui::handle_status_input(
                            &mut state.sriov_state,
                            key,
                            idx,
                        )
                        .await;
                        apply_sriov_action(state, action);
                    }
                    AppScreen::SriovEditor(idx) => {
                        let action = perigee_sriov::ui::handle_editor_input(
                            &mut state.sriov_state,
                            key,
                            Some(idx),
                        )
                        .await;
                        apply_sriov_action(state, action);
                    }
                    AppScreen::SriovNewEditor => {
                        let action = perigee_sriov::ui::handle_editor_input(
                            &mut state.sriov_state,
                            key,
                            None,
                        )
                        .await;
                        apply_sriov_action(state, action);
                    }
                    AppScreen::AffinityTopology => {
                        let action = perigee_affinity::ui::handle_topology_input(
                            &mut state.affinity_state,
                            key,
                        );
                        apply_affinity_action(state, action);
                    }
                    AppScreen::AffinityStrategy => {
                        let action = perigee_affinity::ui::handle_strategy_input(
                            &mut state.affinity_state,
                            key,
                        );
                        apply_affinity_action(state, action);
                    }
                    AppScreen::AffinityApply => {
                        let action = perigee_affinity::ui::handle_apply_input(
                            &mut state.affinity_state,
                            key,
                        );
                        apply_affinity_action(state, action);
                    }
                    AppScreen::AffinityAutoApply => {
                        let action = perigee_affinity::ui::handle_auto_apply_input(
                            &mut state.affinity_state,
                            key,
                        );
                        apply_affinity_action(state, action);
                    }
                }
            }
        }
    }
    Ok(())
}

fn apply_affinity_action(state: &mut AppState, action: perigee_affinity::ui::AffinityUiAction) {
    use perigee_affinity::ui::{AffinityScreen, AffinityUiAction};
    match action {
        AffinityUiAction::None => {}
        AffinityUiAction::GoBack => {
            state.screen = AppScreen::Home;
        }
        AffinityUiAction::NavigateTo(screen) => {
            state.screen = match screen {
                AffinityScreen::Topology => AppScreen::AffinityTopology,
                AffinityScreen::Strategy => AppScreen::AffinityStrategy,
                AffinityScreen::Apply => AppScreen::AffinityApply,
                AffinityScreen::AutoApply => AppScreen::AffinityAutoApply,
            };
        }
    }
}

fn apply_sriov_action(state: &mut AppState, action: perigee_sriov::ui::SriovUiAction) {
    use perigee_sriov::ui::{SriovScreen, SriovUiAction};
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

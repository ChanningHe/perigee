pub mod fdb_config;
pub mod pf_select;
pub mod result;
pub mod review;
pub mod vf_config;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use perigee_core::ipc::{ProfileDetailStatus, Request, Response};
use perigee_sriov::config::{SriovFileConfig, SriovProfileConfig};
use perigee_sriov::detect::PhysicalFunction;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Tabs},
    Frame,
};
use std::collections::BTreeMap;

use super::{common, AppScreen, AppState};

// ── SR-IOV state ──

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorTab {
    Pf,
    General,
    VfTable,
    Fdb,
    Review,
}

impl EditorTab {
    pub const ALL: [Self; 5] = [Self::Pf, Self::General, Self::VfTable, Self::Fdb, Self::Review];

    pub fn title(&self) -> &str {
        match self {
            Self::Pf => "PF",
            Self::General => "General",
            Self::VfTable => "VF Table",
            Self::Fdb => "FDB",
            Self::Review => "Review",
        }
    }

    pub fn index(&self) -> usize {
        Self::ALL.iter().position(|t| t == self).unwrap_or(0)
    }

    pub fn from_index(i: usize) -> Self {
        Self::ALL.get(i).copied().unwrap_or(Self::Pf)
    }
}

/// Which field currently has text-editing focus.
/// When Some(_), global keys (q, Esc, Left/Right) are suppressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditFocus {
    ProfileName,
    GeneralVfCount,
    VfVlanId,
}

pub struct SriovState {
    pub profiles: Vec<(String, SriovProfileConfig)>,
    pub profile_list_state: ListState,
    pub active_tab: EditorTab,
    pub editing_profile: Option<SriovProfileConfig>,
    pub editing_name: String,
    pub message: Option<String>,

    // PF tab
    pub detected_pfs: Vec<PhysicalFunction>,
    pub pf_scan_error: Option<String>,
    pub selected_pf: usize,

    // General tab
    pub general_cursor: usize,
    pub vf_count_buf: String,

    // VF Table tab
    pub vf_table_cursor: usize,
    pub vf_table_scroll: usize,
    pub vlan_id_buf: String,

    // FDB tab
    pub fdb_cursor: usize,

    // Focus mode
    pub edit_focus: Option<EditFocus>,

    // Status view cache
    pub status_detail: Option<ProfileDetailStatus>,
    pub status_error: Option<String>,
}

impl SriovState {
    pub fn new() -> Self {
        Self {
            profiles: Vec::new(),
            profile_list_state: ListState::default(),
            active_tab: EditorTab::Pf,
            editing_profile: None,
            editing_name: String::new(),
            message: None,
            detected_pfs: Vec::new(),
            pf_scan_error: None,
            selected_pf: 0,
            general_cursor: 0,
            vf_count_buf: String::new(),
            vf_table_cursor: 0,
            vf_table_scroll: 0,
            vlan_id_buf: String::new(),
            fdb_cursor: 0,
            edit_focus: None,
            status_detail: None,
            status_error: None,
        }
    }

    pub fn load_profiles(&mut self) {
        let path = perigee_daemon::config::sriov_config_path();
        if path.exists() {
            if let Ok(config) = SriovFileConfig::load(&path) {
                self.profiles = config.sriov.into_iter().collect();
            }
        }
        if !self.profiles.is_empty() && self.profile_list_state.selected().is_none() {
            self.profile_list_state.select(Some(0));
        }
    }

    pub fn scan_pfs(&mut self) {
        match perigee_sriov::detect::scan_physical_functions() {
            Ok(pfs) => {
                self.detected_pfs = pfs;
                self.pf_scan_error = None;
                self.selected_pf = 0;
            }
            Err(e) => {
                self.detected_pfs.clear();
                self.pf_scan_error = Some(e.to_string());
            }
        }
    }

    pub fn reset_editor_cursors(&mut self) {
        self.general_cursor = 0;
        self.vf_count_buf.clear();
        self.vf_table_cursor = 0;
        self.vf_table_scroll = 0;
        self.vlan_id_buf.clear();
        self.fdb_cursor = 0;
        self.edit_focus = None;
    }

    /// Sync vf_count_buf from editing_profile.
    pub fn sync_vf_count_buf(&mut self) {
        self.vf_count_buf = self
            .editing_profile
            .as_ref()
            .map(|p| p.num_vfs.to_string())
            .unwrap_or_default();
    }

    /// Save the current editing_profile into the config file.
    pub fn save_config(&mut self) -> Result<(), String> {
        let profile = self
            .editing_profile
            .as_ref()
            .ok_or("No profile to save")?
            .clone();
        let name = if self.editing_name.trim().is_empty() {
            return Err("Profile name cannot be empty".to_string());
        } else {
            self.editing_name.trim().to_string()
        };

        let path = perigee_daemon::config::sriov_config_path();
        let mut file_config = if path.exists() {
            SriovFileConfig::load(&path).unwrap_or(SriovFileConfig {
                sriov: BTreeMap::new(),
            })
        } else {
            SriovFileConfig {
                sriov: BTreeMap::new(),
            }
        };

        file_config.sriov.insert(name.clone(), profile);
        file_config
            .save(&path)
            .map_err(|e| format!("Failed to save: {}", e))?;

        self.profiles = file_config.sriov.into_iter().collect();
        Ok(())
    }
}

// ── Profile list ──

pub fn render_profiles(frame: &mut Frame, state: &AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(frame.area());

    common::header_bar(frame, chunks[0], "SR-IOV Profiles", state.daemon_online);

    if state.sriov_state.profiles.is_empty() {
        let empty = Paragraph::new(Line::from(vec![
            Span::raw("  No profiles configured. Press "),
            Span::styled(
                "n",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" to create one."),
        ]))
        .block(Block::default().borders(Borders::ALL).title(" Profiles "));
        frame.render_widget(empty, chunks[1]);
    } else {
        let header = Line::from(vec![Span::styled(
            format!(
                "  {:<20} {:<20} {:>4}  {:<10}",
                "Profile", "PF MAC", "VFs", "Status"
            ),
            Style::default().fg(Color::DarkGray),
        )]);

        let items: Vec<ListItem> = state
            .sriov_state
            .profiles
            .iter()
            .enumerate()
            .map(|(i, (name, profile))| {
                let selected = state.sriov_state.profile_list_state.selected() == Some(i);
                let prefix = if selected { "▸ " } else { "  " };
                let style = if selected {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                };
                ListItem::new(Line::from(Span::styled(
                    format!(
                        "{}{:<20} {:<20} {:>4}  {:<10}",
                        prefix, name, profile.mac, profile.num_vfs, "—"
                    ),
                    style,
                )))
            })
            .collect();

        let list = List::new(items).block(
            Block::default()
                .title(" Profiles ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );

        let inner_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(chunks[1]);

        frame.render_widget(Paragraph::new(header), inner_chunks[0]);
        frame.render_widget(list, inner_chunks[1]);
    }

    common::footer_bar(
        frame,
        chunks[2],
        &[
            ("s/Enter", "Status"),
            ("e", "Edit"),
            ("n", "New"),
            ("d", "Delete"),
            ("r", "Reload"),
            ("q", "Quit"),
        ],
    );
}

pub async fn handle_profiles_input(state: &mut AppState, key: KeyEvent) {
    let len = state.sriov_state.profiles.len();
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => {
            state.screen = AppScreen::Home;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if len > 0 {
                let i = state
                    .sriov_state
                    .profile_list_state
                    .selected()
                    .unwrap_or(0);
                let new = if i == 0 { len - 1 } else { i - 1 };
                state.sriov_state.profile_list_state.select(Some(new));
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if len > 0 {
                let i = state
                    .sriov_state
                    .profile_list_state
                    .selected()
                    .unwrap_or(0);
                let new = if i >= len - 1 { 0 } else { i + 1 };
                state.sriov_state.profile_list_state.select(Some(new));
            }
        }
        KeyCode::Enter | KeyCode::Char('s') => {
            if let Some(idx) = state.sriov_state.profile_list_state.selected() {
                state.sriov_state.status_detail = None;
                state.sriov_state.status_error = None;
                state.sriov_state.message = None;
                state.screen = AppScreen::SriovStatus(idx);
                fetch_profile_status(state, idx).await;
            }
        }
        KeyCode::Char('e') => {
            if let Some(idx) = state.sriov_state.profile_list_state.selected() {
                let (name, profile) = &state.sriov_state.profiles[idx];
                state.sriov_state.editing_name = name.clone();
                state.sriov_state.editing_profile = Some(profile.clone());
                state.sriov_state.active_tab = EditorTab::Pf;
                state.sriov_state.reset_editor_cursors();
                state.sriov_state.sync_vf_count_buf();
                state.sriov_state.scan_pfs();
                state.screen = AppScreen::SriovEditor(idx);
            }
        }
        KeyCode::Char('n') => {
            state.sriov_state.editing_name.clear();
            state.sriov_state.editing_profile = None;
            state.sriov_state.active_tab = EditorTab::Pf;
            state.sriov_state.reset_editor_cursors();
            state.sriov_state.scan_pfs();
            state.screen = AppScreen::SriovNewEditor;
        }
        KeyCode::Char('r') => {
            if crate::client::IpcClient::is_daemon_running() {
                let _ = crate::client::IpcClient::send(&Request::Reload).await;
                state.sriov_state.message = Some("Reload sent to daemon".to_string());
            }
        }
        _ => {}
    }
}

// ── Status view ──

pub fn render_status(frame: &mut Frame, state: &AppState, profile_idx: usize) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(frame.area());

    let (profile_name, profile) = match state.sriov_state.profiles.get(profile_idx) {
        Some((n, p)) => (n.clone(), p.clone()),
        None => {
            common::header_bar(frame, chunks[0], "SR-IOV > Status", state.daemon_online);
            let para = Paragraph::new("  Profile not found").block(
                Block::default().borders(Borders::ALL),
            );
            frame.render_widget(para, chunks[1]);
            common::footer_bar(frame, chunks[2], &[("Esc", "Back")]);
            return;
        }
    };

    common::header_bar(
        frame,
        chunks[0],
        &format!("SR-IOV > {} > Status", profile_name),
        state.daemon_online,
    );

    let mut lines: Vec<Line> = Vec::new();

    // Config info (always available)
    lines.push(Line::from(Span::styled(
        "  ── Configuration ──",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(vec![
        Span::styled("  PF MAC:       ", Style::default().fg(Color::DarkGray)),
        Span::styled(profile.mac.to_string(), Style::default().fg(Color::White)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  VF Count:     ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            profile.num_vfs.to_string(),
            Style::default().fg(Color::White),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  MAC Strategy: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{:?}", profile.mac_strategy),
            Style::default().fg(Color::White),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  FDB Mode:     ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{:?}", profile.fdb.mode),
            Style::default().fg(Color::White),
        ),
    ]));
    lines.push(Line::from(""));

    // Runtime status from daemon
    if let Some(detail) = &state.sriov_state.status_detail {
        let state_color = match detail.state {
            perigee_core::ipc::ProfileState::Active => Color::Green,
            perigee_core::ipc::ProfileState::Degraded => Color::Yellow,
            perigee_core::ipc::ProfileState::Pending => Color::Cyan,
            _ => Color::Red,
        };
        lines.push(Line::from(Span::styled(
            "  ── Runtime ──",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(vec![
            Span::styled("  State:        ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{}", detail.state), Style::default().fg(state_color)),
        ]));
        if let Some(ref iface) = detail.pf_iface {
            lines.push(Line::from(vec![
                Span::styled("  PF Iface:     ", Style::default().fg(Color::DarkGray)),
                Span::styled(iface.as_str(), Style::default().fg(Color::White)),
            ]));
        }
        if let Some(ts) = &detail.last_applied {
            lines.push(Line::from(vec![
                Span::styled("  Last Applied: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    ts.format("%Y-%m-%d %H:%M:%S").to_string(),
                    Style::default().fg(Color::White),
                ),
            ]));
        }

        // FDB status
        lines.push(Line::from(vec![
            Span::styled("  FDB Entries:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                detail.fdb.managed_entries.to_string(),
                Style::default().fg(Color::White),
            ),
        ]));

        // VF summary
        if !detail.vfs.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  ── VF Status ──",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                format!(
                    "  {:>4}  {:<18} {:<6} {:<8} {:<6} {}",
                    "VF#", "MAC", "Trust", "SpoofChk", "VLAN", "Status"
                ),
                Style::default().fg(Color::DarkGray),
            )));

            let max_show = 20;
            for vf in detail.vfs.iter().take(max_show) {
                let status_str = if vf.matches { "OK" } else { "MISMATCH" };
                let status_color = if vf.matches {
                    Color::Green
                } else {
                    Color::Red
                };
                let vlan_str = vf
                    .configured
                    .vlan_id
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string());

                lines.push(Line::from(vec![
                    Span::styled(
                        format!(
                            "  {:>4}  {:<18} {:<6} {:<8} {:<6} ",
                            vf.index,
                            &vf.configured.mac,
                            if vf.configured.trust { "✓" } else { "✗" },
                            if vf.configured.spoofchk { "✓" } else { "✗" },
                            vlan_str,
                        ),
                        Style::default().fg(Color::Gray),
                    ),
                    Span::styled(status_str, Style::default().fg(status_color)),
                ]));
            }
            if detail.vfs.len() > max_show {
                lines.push(Line::from(Span::styled(
                    format!("  ... and {} more VFs", detail.vfs.len() - max_show),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
    } else if let Some(err) = &state.sriov_state.status_error {
        lines.push(Line::from(Span::styled(
            "  ── Runtime ──",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!("  {}", err),
            Style::default().fg(Color::Yellow),
        )));
    } else if !state.daemon_online {
        lines.push(Line::from(Span::styled(
            "  Daemon offline — no runtime status available.",
            Style::default().fg(Color::Yellow),
        )));
    }

    // Message
    if let Some(msg) = &state.sriov_state.message {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  {}", msg),
            Style::default().fg(Color::Yellow),
        )));
    }

    let para = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray)),
    );
    frame.render_widget(para, chunks[1]);

    common::footer_bar(
        frame,
        chunks[2],
        &[
            ("e", "Edit"),
            ("R", "Refresh"),
            ("a", "Apply"),
            ("Esc", "Back"),
        ],
    );
}

pub async fn handle_status_input(state: &mut AppState, key: KeyEvent, profile_idx: usize) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            state.sriov_state.status_detail = None;
            state.sriov_state.status_error = None;
            state.screen = AppScreen::SriovProfiles;
        }
        KeyCode::Char('e') => {
            if let Some((name, profile)) = state.sriov_state.profiles.get(profile_idx) {
                state.sriov_state.editing_name = name.clone();
                state.sriov_state.editing_profile = Some(profile.clone());
                state.sriov_state.active_tab = EditorTab::Pf;
                state.sriov_state.reset_editor_cursors();
                state.sriov_state.sync_vf_count_buf();
                state.screen = AppScreen::SriovEditor(profile_idx);
            }
        }
        KeyCode::Char('R') | KeyCode::Char('r') => {
            fetch_profile_status(state, profile_idx).await;
        }
        KeyCode::Char('a') => {
            if let Some((name, _)) = state.sriov_state.profiles.get(profile_idx) {
                let profile_name = name.clone();
                if crate::client::IpcClient::is_daemon_running() {
                    match crate::client::IpcClient::send(&Request::Apply {
                        profile: profile_name.clone(),
                    })
                    .await
                    {
                        Ok(Response::Ok) => {
                            state.sriov_state.message =
                                Some(format!("Apply triggered for '{}'", profile_name));
                            fetch_profile_status(state, profile_idx).await;
                        }
                        Ok(Response::Error { message }) => {
                            state.sriov_state.message = Some(format!("Apply error: {}", message));
                        }
                        _ => {
                            state.sriov_state.message =
                                Some("Unexpected daemon response".to_string());
                        }
                    }
                } else {
                    state.sriov_state.message = Some("Daemon is not running".to_string());
                }
            }
        }
        _ => {}
    }
}

async fn fetch_profile_status(state: &mut AppState, profile_idx: usize) {
    if let Some((name, _)) = state.sriov_state.profiles.get(profile_idx) {
        let profile_name = name.clone();
        if crate::client::IpcClient::is_daemon_running() {
            match crate::client::IpcClient::send(&Request::ProfileStatus {
                profile: profile_name,
            })
            .await
            {
                Ok(Response::ProfileDetail(detail)) => {
                    state.sriov_state.status_detail = Some(detail);
                    state.sriov_state.status_error = None;
                }
                Ok(Response::Error { message }) => {
                    state.sriov_state.status_detail = None;
                    state.sriov_state.status_error = Some(message);
                }
                Err(e) => {
                    state.sriov_state.status_detail = None;
                    state.sriov_state.status_error = Some(format!("IPC error: {}", e));
                }
                _ => {
                    state.sriov_state.status_detail = None;
                    state.sriov_state.status_error = Some("Unexpected response".to_string());
                }
            }
        } else {
            state.sriov_state.status_error = Some("Daemon is not running".to_string());
        }
    }
}

// ── Tab editor ──

pub fn render_editor(frame: &mut Frame, state: &AppState, profile_idx: usize) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(frame.area());

    let title = if profile_idx == usize::MAX {
        "SR-IOV > New Profile".to_string()
    } else if let Some((name, _)) = state.sriov_state.profiles.get(profile_idx) {
        format!("SR-IOV > {}", name)
    } else {
        "SR-IOV > Editor".to_string()
    };

    common::header_bar(frame, chunks[0], &title, state.daemon_online);

    // Tab bar
    let tab_titles: Vec<Line> = EditorTab::ALL
        .iter()
        .map(|t| {
            let style = if *t == state.sriov_state.active_tab {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            Line::from(Span::styled(t.title(), style))
        })
        .collect();

    let tabs = Tabs::new(tab_titles)
        .select(state.sriov_state.active_tab.index())
        .divider(Span::raw(" │ "));
    frame.render_widget(tabs, chunks[1]);

    // Tab content
    match state.sriov_state.active_tab {
        EditorTab::Pf => pf_select::render(frame, state, chunks[2]),
        EditorTab::General => vf_config::render_general(frame, state, chunks[2]),
        EditorTab::VfTable => vf_config::render_vf_table(frame, state, chunks[2]),
        EditorTab::Fdb => fdb_config::render(frame, state, chunks[2]),
        EditorTab::Review => review::render(frame, state, chunks[2]),
    }

    // Show message if present (on non-Review tabs, since Review shows it inline)
    if state.sriov_state.active_tab != EditorTab::Review {
        if let Some(msg) = &state.sriov_state.message {
            let msg_area = ratatui::layout::Rect {
                x: chunks[2].x,
                y: chunks[2].y + chunks[2].height.saturating_sub(1),
                width: chunks[2].width,
                height: 1,
            };
            let msg_para = Paragraph::new(Line::from(Span::styled(
                format!("  {}", msg),
                Style::default().fg(Color::Yellow),
            )));
            frame.render_widget(msg_para, msg_area);
        }
    }

    // Dynamic footer hints based on focus state
    let hints: Vec<(&str, &str)> = if state.sriov_state.edit_focus.is_some() {
        vec![("Enter", "Confirm"), ("Esc", "Cancel")]
    } else {
        vec![
            ("Tab/◀▶", "Switch Tab"),
            ("↑↓", "Navigate"),
            ("Enter", "Edit/Select"),
            ("Ctrl+S", "Save"),
            ("Esc", "Back"),
        ]
    };
    common::footer_bar(frame, chunks[3], &hints);
}

pub async fn handle_editor_input(
    state: &mut AppState,
    key: KeyEvent,
    _profile_idx: Option<usize>,
) {
    // When a field has text-editing focus, route everything to the tab handler
    // except Ctrl+S which is always the save shortcut.
    if state.sriov_state.edit_focus.is_some() {
        if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
            do_save(state).await;
            return;
        }
        // Esc exits focus mode, handled by tab
        match state.sriov_state.active_tab {
            EditorTab::Pf => pf_select::handle_input(state, key),
            EditorTab::General => vf_config::handle_general_input(state, key),
            EditorTab::VfTable => vf_config::handle_vf_table_input(state, key),
            EditorTab::Fdb => fdb_config::handle_input(state, key),
            EditorTab::Review => review::handle_input(state, key).await,
        }
        return;
    }

    // Global editor keys (only active when no field focus)
    match key.code {
        KeyCode::Esc => {
            state.screen = AppScreen::SriovProfiles;
            return;
        }
        KeyCode::Char('q') if key.modifiers.is_empty() => {
            state.screen = AppScreen::SriovProfiles;
            return;
        }
        KeyCode::Tab | KeyCode::Right => {
            let next = (state.sriov_state.active_tab.index() + 1) % EditorTab::ALL.len();
            state.sriov_state.active_tab = EditorTab::from_index(next);
            return;
        }
        KeyCode::BackTab | KeyCode::Left => {
            let cur = state.sriov_state.active_tab.index();
            let prev = if cur == 0 {
                EditorTab::ALL.len() - 1
            } else {
                cur - 1
            };
            state.sriov_state.active_tab = EditorTab::from_index(prev);
            return;
        }
        KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            do_save(state).await;
            return;
        }
        _ => {}
    }

    // Per-tab input handling (navigation mode)
    match state.sriov_state.active_tab {
        EditorTab::Pf => pf_select::handle_input(state, key),
        EditorTab::General => vf_config::handle_general_input(state, key),
        EditorTab::VfTable => vf_config::handle_vf_table_input(state, key),
        EditorTab::Fdb => fdb_config::handle_input(state, key),
        EditorTab::Review => review::handle_input(state, key).await,
    }
}

async fn do_save(state: &mut AppState) {
    state.sriov_state.edit_focus = None;
    match state.sriov_state.save_config() {
        Ok(()) => {
            let mut msg = "✓ Config saved to /etc/perigee/sriov.toml".to_string();
            if crate::client::IpcClient::is_daemon_running() {
                match crate::client::IpcClient::send(&Request::Reload).await {
                    Ok(Response::Ok) => {
                        msg.push_str(" — daemon reloaded.");
                    }
                    Ok(Response::Error { message }) => {
                        msg.push_str(&format!(" — daemon reload error: {}", message));
                    }
                    _ => {
                        msg.push_str(" — daemon reload: unexpected response.");
                    }
                }
            }
            state.sriov_state.message = Some(msg);
            // Switch to Review tab to show the feedback
            state.sriov_state.active_tab = EditorTab::Review;
        }
        Err(e) => {
            state.sriov_state.message = Some(format!("✗ Save failed: {}", e));
            state.sriov_state.active_tab = EditorTab::Review;
        }
    }
}

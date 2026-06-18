pub mod fdb_config;
pub mod pf_select;
pub mod result;
pub mod review;
pub mod vf_config;

use crate::config::{sriov_config_path, SriovFileConfig, SriovProfileConfig};
use crate::detect::PhysicalFunction;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use perigee_core::ipc::{
    FdbEntryInfo, ProfileDetailStatus, ProfileState, Request, Response, VfUser,
};
use perigee_tui as common;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Tabs},
    Frame,
};
use std::collections::{BTreeMap, HashMap};

// ── Navigation types (consumed by binary crate) ──

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SriovScreen {
    Profiles,
    Status(usize),
    FdbDetail(usize),
    Editor(usize),
    NewEditor,
}

#[derive(Debug)]
pub enum SriovUiAction {
    None,
    NavigateTo(SriovScreen),
    GoBack,
}

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
    pub const ALL: [Self; 5] = [
        Self::Pf,
        Self::General,
        Self::VfTable,
        Self::Fdb,
        Self::Review,
    ];

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

    pub detected_pfs: Vec<PhysicalFunction>,
    pub pf_scan_error: Option<String>,
    pub selected_pf: usize,

    pub general_cursor: usize,
    pub vf_count_buf: String,

    pub vf_table_cursor: usize,
    pub vf_table_scroll: usize,
    pub vlan_id_buf: String,

    pub fdb_cursor: usize,

    pub edit_focus: Option<EditFocus>,

    pub profile_statuses: HashMap<String, ProfileState>,

    pub status_detail: Option<ProfileDetailStatus>,
    pub status_error: Option<String>,
    /// Vertical scroll offset (in lines) for the status view, which can exceed
    /// the viewport on PFs with many VFs.
    pub status_scroll: u16,
    /// Scroll offset for the editor's TOML preview, long with many VF overrides.
    pub review_scroll: u16,
    /// FDB entries fetched for the FDB detail sub-page, with its scroll offset.
    pub fdb_entries: Vec<FdbEntryInfo>,
    pub fdb_scroll: u16,
    /// VF→VM usage map and resolved PF iface, cached on editor open so the VF
    /// Table renderer never scans /etc/pve or all of /sys/class/net per frame.
    pub vf_users: HashMap<String, VfUser>,
    pub editor_pf_iface: Option<String>,
}

impl Default for SriovState {
    fn default() -> Self {
        Self::new()
    }
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
            profile_statuses: HashMap::new(),
            status_detail: None,
            status_error: None,
            status_scroll: 0,
            review_scroll: 0,
            fdb_entries: Vec::new(),
            fdb_scroll: 0,
            vf_users: HashMap::new(),
            editor_pf_iface: None,
        }
    }

    pub fn load_profiles(&mut self) {
        let path = sriov_config_path();
        if path.exists() {
            if let Ok(config) = SriovFileConfig::load(&path) {
                self.profiles = config.sriov.into_iter().collect();
            }
        }
        if !self.profiles.is_empty() && self.profile_list_state.selected().is_none() {
            self.profile_list_state.select(Some(0));
        }
    }

    pub async fn fetch_profile_statuses(&mut self) {
        if !perigee_core::client::IpcClient::is_daemon_running() {
            return;
        }
        if let Ok(Response::Status(status)) =
            perigee_core::client::IpcClient::send(&Request::Status).await
        {
            for module in &status.modules {
                if module.name == "sriov" {
                    for ps in &module.profiles {
                        self.profile_statuses.insert(ps.name.clone(), ps.state);
                    }
                }
            }
        }
    }

    pub fn scan_pfs(&mut self) {
        match crate::detect::scan_physical_functions() {
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
        self.review_scroll = 0;
        self.refresh_vf_usage();
    }

    /// Resolve the PF interface and scan VM passthrough usage once, on editor
    /// open. Keeps the VF Table render path free of per-frame filesystem I/O.
    pub fn refresh_vf_usage(&mut self) {
        self.editor_pf_iface = self
            .editing_profile
            .as_ref()
            .and_then(|p| perigee_core::sysfs::find_iface_by_mac(&p.mac.to_string()).ok());
        self.vf_users = crate::vm_usage::scan_vf_users();
    }

    pub fn sync_vf_count_buf(&mut self) {
        self.vf_count_buf = self
            .editing_profile
            .as_ref()
            .map(|p| p.num_vfs.to_string())
            .unwrap_or_default();
    }

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

        let path = sriov_config_path();
        let mut file_config = if path.exists() {
            // Never clobber an existing config we cannot parse — that would drop
            // every other profile in the file. Abort and let the user fix it.
            SriovFileConfig::load(&path).map_err(|e| {
                format!("Refusing to overwrite unreadable {}: {}", path.display(), e)
            })?
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

pub fn render_profiles(frame: &mut Frame, daemon_online: bool, sriov: &mut SriovState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(frame.area());

    common::header_bar(frame, chunks[0], "SR-IOV Profiles", daemon_online);

    if sriov.profiles.is_empty() {
        let empty = Paragraph::new(Line::from(vec![
            Span::styled("  No profiles configured. Press ", common::style_muted()),
            Span::styled(
                "n",
                Style::default()
                    .fg(common::BRAND)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" to create one.", common::style_muted()),
        ]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(common::BORDER))
                .title(Span::styled(
                    " Profiles ",
                    Style::default().fg(common::BRAND_DIM),
                )),
        );
        frame.render_widget(empty, chunks[1]);
    } else {
        let header = Line::from(Span::styled(
            format!(
                "  {:<20} {:<20} {:>4}  {:<10}",
                "Profile", "PF MAC", "VFs", "Status"
            ),
            common::style_muted(),
        ));

        let items: Vec<ListItem> = sriov
            .profiles
            .iter()
            .enumerate()
            .map(|(i, (name, profile))| {
                let selected = sriov.profile_list_state.selected() == Some(i);
                let prefix = if selected { " ▸ " } else { "   " };

                let status = sriov.profile_statuses.get(name).copied();
                let status_str = status.as_ref().map(|s| s.to_string()).unwrap_or_else(|| {
                    if daemon_online {
                        "—".to_string()
                    } else {
                        "offline".to_string()
                    }
                });
                let status_color = status
                    .as_ref()
                    .map(common::state_color)
                    .unwrap_or(common::TEXT_MUTED);

                let name_style = if selected {
                    common::style_selected()
                } else {
                    Style::default().fg(common::TEXT_DIM)
                };

                ListItem::new(Line::from(vec![
                    Span::styled(prefix, name_style),
                    Span::styled(format!("{:<20}", name), name_style),
                    Span::styled(
                        format!("{:<20}", profile.mac),
                        if selected {
                            Style::default().fg(common::TEXT)
                        } else {
                            common::style_muted()
                        },
                    ),
                    Span::styled(
                        format!("{:>4}  ", profile.num_vfs),
                        if selected {
                            Style::default().fg(common::TEXT)
                        } else {
                            common::style_muted()
                        },
                    ),
                    Span::styled(
                        format!("{:<10}", status_str),
                        Style::default().fg(status_color).add_modifier(if selected {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                    ),
                ]))
            })
            .collect();

        let list = List::new(items).block(
            Block::default()
                .title(Span::styled(
                    " Profiles ",
                    Style::default().fg(common::BRAND_DIM),
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(common::BORDER)),
        );

        let inner_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(chunks[1]);

        frame.render_widget(Paragraph::new(header), inner_chunks[0]);
        // Stateful render so the selection scrolls into view once the profile
        // count exceeds the visible rows; a plain render_widget ignores the
        // ListState and the cursor would vanish off the bottom.
        frame.render_stateful_widget(list, inner_chunks[1], &mut sriov.profile_list_state);
    }

    if let Some(msg) = &sriov.message {
        let msg_area = ratatui::layout::Rect {
            x: chunks[1].x + 1,
            y: chunks[1].y + chunks[1].height.saturating_sub(2),
            width: chunks[1].width.saturating_sub(2),
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(Span::styled(format!("  {}", msg), common::style_warn())),
            msg_area,
        );
    }

    common::footer_bar(
        frame,
        chunks[2],
        &[
            ("Enter", "Status"),
            ("e", "Edit"),
            ("n", "New"),
            ("d", "Delete"),
            ("r", "Reload"),
            ("q", "Back"),
        ],
    );
}

pub async fn handle_profiles_input(sriov: &mut SriovState, key: KeyEvent) -> SriovUiAction {
    let len = sriov.profiles.len();
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => {
            return SriovUiAction::GoBack;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if len > 0 {
                let i = sriov.profile_list_state.selected().unwrap_or(0);
                let new = if i == 0 { len - 1 } else { i - 1 };
                sriov.profile_list_state.select(Some(new));
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if len > 0 {
                let i = sriov.profile_list_state.selected().unwrap_or(0);
                let new = if i >= len - 1 { 0 } else { i + 1 };
                sriov.profile_list_state.select(Some(new));
            }
        }
        KeyCode::Enter | KeyCode::Char('s') => {
            if let Some(idx) = sriov.profile_list_state.selected() {
                sriov.status_detail = None;
                sriov.status_error = None;
                sriov.message = None;
                fetch_profile_status(sriov, idx).await;
                return SriovUiAction::NavigateTo(SriovScreen::Status(idx));
            }
        }
        KeyCode::Char('e') => {
            if let Some(idx) = sriov.profile_list_state.selected() {
                let (name, profile) = &sriov.profiles[idx];
                sriov.editing_name = name.clone();
                sriov.editing_profile = Some(profile.clone());
                sriov.active_tab = EditorTab::Pf;
                sriov.reset_editor_cursors();
                sriov.sync_vf_count_buf();
                sriov.scan_pfs();
                return SriovUiAction::NavigateTo(SriovScreen::Editor(idx));
            }
        }
        KeyCode::Char('n') => {
            sriov.editing_name.clear();
            sriov.editing_profile = None;
            sriov.active_tab = EditorTab::Pf;
            sriov.reset_editor_cursors();
            sriov.scan_pfs();
            return SriovUiAction::NavigateTo(SriovScreen::NewEditor);
        }
        KeyCode::Char('r') => {
            if perigee_core::client::IpcClient::is_daemon_running() {
                let _ = perigee_core::client::IpcClient::send(&Request::Reload).await;
                sriov.message = Some("Reload sent to daemon".to_string());
            }
            sriov.load_profiles();
            sriov.fetch_profile_statuses().await;
        }
        _ => {}
    }
    SriovUiAction::None
}

// ── Status view ──

pub fn render_status(
    frame: &mut Frame,
    daemon_online: bool,
    sriov: &mut SriovState,
    profile_idx: usize,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(frame.area());

    let (profile_name, profile) = match sriov.profiles.get(profile_idx) {
        Some((n, p)) => (n.clone(), p.clone()),
        None => {
            common::header_bar(frame, chunks[0], "SR-IOV > Status", daemon_online);
            let para =
                Paragraph::new("  Profile not found").block(Block::default().borders(Borders::ALL));
            frame.render_widget(para, chunks[1]);
            common::footer_bar(frame, chunks[2], &[("Esc", "Back")]);
            return;
        }
    };

    common::header_bar(
        frame,
        chunks[0],
        &format!("SR-IOV > {} > Status", profile_name),
        daemon_online,
    );

    let mut lines: Vec<Line> = Vec::new();

    let section_hdr = |text: &str| -> Line<'static> {
        Line::from(Span::styled(
            format!("  {}", text),
            Style::default()
                .fg(common::TEXT)
                .add_modifier(Modifier::BOLD),
        ))
    };
    let kv = |label: &str, value: String, vc: ratatui::style::Color| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!("  {:<16}", label), common::style_label()),
            Span::styled(value, Style::default().fg(vc)),
        ])
    };

    lines.push(section_hdr("── Configuration ──"));
    lines.push(kv("PF MAC:", profile.mac.to_string(), common::TEXT));
    lines.push(kv("VF Count:", profile.num_vfs.to_string(), common::TEXT));
    lines.push(kv(
        "MAC Strategy:",
        format!("{:?}", profile.mac_strategy),
        common::TEXT,
    ));
    lines.push(kv(
        "FDB Mode:",
        format!("{:?}", profile.fdb.mode),
        common::TEXT,
    ));
    lines.push(Line::from(""));

    if let Some(detail) = &sriov.status_detail {
        let sc = common::state_color(&detail.state);
        lines.push(section_hdr("── Runtime ──"));
        lines.push(kv("State:", format!("{}", detail.state), sc));
        if detail.config_dirty {
            lines.push(Line::from(Span::styled(
                "  ⚠ Config modified since last apply — press 'a' to apply.",
                Style::default().fg(common::WARN),
            )));
        }
        if let Some(ref iface) = detail.pf_iface {
            lines.push(kv("PF Iface:", iface.clone(), common::TEXT));
        }
        if let Some(ts) = &detail.last_applied {
            lines.push(kv(
                "Last Applied:",
                ts.format("%Y-%m-%d %H:%M:%S").to_string(),
                common::TEXT,
            ));
        }
        lines.push(kv(
            "FDB Entries:",
            detail.fdb.managed_entries.to_string(),
            common::TEXT,
        ));

        if !detail.vfs.is_empty() {
            lines.push(Line::from(""));
            lines.push(section_hdr("── VF Status ──"));
            lines.push(Line::from(Span::styled(
                format!(
                    "  {:>4}  {:<14} {:<26} {:<6} {:<8} {:<8} {:<10} {}",
                    "VF#", "PCI Addr", "MAC", "Trust", "Spoof", "VLAN", "Status", "Used By"
                ),
                common::style_muted(),
            )));

            for vf in detail.vfs.iter() {
                let ok = vf.matches;
                let vlan_str = vf
                    .configured
                    .vlan_id
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string());

                // PCI address PVE uses to pass the VF through; VF# alone doesn't
                // map to what shows up in the PVE hardware list.
                let pci_str = vf.pci_addr.as_deref().unwrap_or("-");

                // For auto-assigned MACs the configured value is the "(auto)"
                // sentinel; show the live MAC read back from the VF so the
                // operator can see what was actually assigned.
                let mac_display = if vf.configured.mac == "(auto)" {
                    match vf.actual.as_ref() {
                        Some(a) if !a.mac.is_empty() => format!("{} (auto)", a.mac),
                        _ => "(auto)".to_string(),
                    }
                } else {
                    vf.configured.mac.clone()
                };

                // "Used By" is a colored span: green when the referencing VM is
                // running, muted when stopped, and "-" when no VM uses the VF.
                let (used_text, used_color) = match &vf.used_by {
                    Some(u) if u.running => (format!("VM {}", u.vmid), common::SUCCESS),
                    Some(u) => (format!("VM {}", u.vmid), common::TEXT_MUTED),
                    None => ("-".to_string(), common::TEXT_MUTED),
                };

                lines.push(Line::from(vec![
                    Span::styled(
                        format!(
                            "  {:>4}  {:<14} {:<26} {:<6} {:<8} {:<8} ",
                            vf.index,
                            pci_str,
                            mac_display,
                            if vf.configured.trust { "✓" } else { "✗" },
                            if vf.configured.spoofchk { "✓" } else { "✗" },
                            vlan_str,
                        ),
                        Style::default().fg(common::TEXT_DIM),
                    ),
                    Span::styled(
                        format!("{:<10}", if ok { "OK" } else { "MISMATCH" }),
                        Style::default().fg(if ok { common::SUCCESS } else { common::ERROR }),
                    ),
                    Span::styled(used_text, Style::default().fg(used_color)),
                ]));
            }
        }
    } else if let Some(err) = &sriov.status_error {
        lines.push(section_hdr("── Runtime ──"));
        lines.push(Line::from(Span::styled(
            format!("  {}", err),
            common::style_warn(),
        )));
    } else if !daemon_online {
        lines.push(Line::from(Span::styled(
            "  Daemon offline — no runtime status available.",
            common::style_warn(),
        )));
    }

    if let Some(msg) = &sriov.message {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  {}", msg),
            common::style_warn(),
        )));
    }

    // Clamp the scroll offset to the content height for the current viewport
    // (borders consume 2 rows), so the view can't scroll past the last line.
    let viewport = chunks[1].height.saturating_sub(2);
    let max_scroll = (lines.len() as u16).saturating_sub(viewport);
    sriov.status_scroll = sriov.status_scroll.min(max_scroll);

    let para = Paragraph::new(lines)
        .scroll((sriov.status_scroll, 0))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(common::BORDER)),
        );
    frame.render_widget(para, chunks[1]);

    common::footer_bar(
        frame,
        chunks[2],
        &[
            ("↑↓", "Scroll"),
            ("e", "Edit"),
            ("f", "FDB"),
            ("R", "Refresh"),
            ("a", "Apply"),
            ("Esc", "Back"),
        ],
    );
}

pub async fn handle_status_input(
    sriov: &mut SriovState,
    key: KeyEvent,
    profile_idx: usize,
) -> SriovUiAction {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            sriov.status_detail = None;
            sriov.status_error = None;
            sriov.status_scroll = 0;
            return SriovUiAction::NavigateTo(SriovScreen::Profiles);
        }
        KeyCode::Char('f') => {
            fetch_fdb_entries(sriov, profile_idx).await;
            return SriovUiAction::NavigateTo(SriovScreen::FdbDetail(profile_idx));
        }
        KeyCode::Up => {
            sriov.status_scroll = sriov.status_scroll.saturating_sub(1);
        }
        KeyCode::Down => {
            // Upper bound is clamped in render against the viewport height.
            sriov.status_scroll = sriov.status_scroll.saturating_add(1);
        }
        KeyCode::Char('e') => {
            if let Some((name, profile)) = sriov.profiles.get(profile_idx) {
                sriov.editing_name = name.clone();
                sriov.editing_profile = Some(profile.clone());
                sriov.active_tab = EditorTab::Pf;
                sriov.reset_editor_cursors();
                sriov.sync_vf_count_buf();
                sriov.scan_pfs();
                return SriovUiAction::NavigateTo(SriovScreen::Editor(profile_idx));
            }
        }
        KeyCode::Char('R') | KeyCode::Char('r') => {
            fetch_profile_status(sriov, profile_idx).await;
        }
        KeyCode::Char('a') => {
            if let Some((name, _)) = sriov.profiles.get(profile_idx) {
                let profile_name = name.clone();
                if perigee_core::client::IpcClient::is_daemon_running() {
                    match perigee_core::client::IpcClient::send(&Request::Apply {
                        profile: profile_name.clone(),
                    })
                    .await
                    {
                        Ok(Response::Ok) => {
                            sriov.message = Some(format!("Apply triggered for '{}'", profile_name));
                            fetch_profile_status(sriov, profile_idx).await;
                        }
                        Ok(Response::Error { message }) => {
                            sriov.message = Some(format!("Apply error: {}", message));
                        }
                        _ => {
                            sriov.message = Some("Unexpected daemon response".to_string());
                        }
                    }
                } else {
                    sriov.message = Some("Daemon is not running".to_string());
                }
            }
        }
        _ => {}
    }
    SriovUiAction::None
}

async fn fetch_profile_status(sriov: &mut SriovState, profile_idx: usize) {
    if let Some((name, _)) = sriov.profiles.get(profile_idx) {
        let profile_name = name.clone();
        if perigee_core::client::IpcClient::is_daemon_running() {
            match perigee_core::client::IpcClient::send(&Request::ProfileStatus {
                profile: profile_name,
            })
            .await
            {
                Ok(Response::ProfileDetail(detail)) => {
                    sriov.status_detail = Some(detail);
                    sriov.status_error = None;
                    sriov.status_scroll = 0;
                }
                Ok(Response::Error { message }) => {
                    sriov.status_detail = None;
                    sriov.status_error = Some(message);
                }
                Err(e) => {
                    sriov.status_detail = None;
                    sriov.status_error = Some(format!("IPC error: {}", e));
                }
                _ => {
                    sriov.status_detail = None;
                    sriov.status_error = Some("Unexpected response".to_string());
                }
            }
        } else {
            sriov.status_error = Some("Daemon is not running".to_string());
        }
    }
}

// ── FDB detail sub-page ──

async fn fetch_fdb_entries(sriov: &mut SriovState, profile_idx: usize) {
    sriov.fdb_entries.clear();
    sriov.fdb_scroll = 0;
    let Some((name, _)) = sriov.profiles.get(profile_idx) else {
        return;
    };
    let profile_name = name.clone();
    if !perigee_core::client::IpcClient::is_daemon_running() {
        return;
    }
    if let Ok(Response::FdbEntries(entries)) =
        perigee_core::client::IpcClient::send(&Request::FdbEntries {
            profile: profile_name,
        })
        .await
    {
        sriov.fdb_entries = entries;
    }
}

pub fn render_fdb_detail(
    frame: &mut Frame,
    daemon_online: bool,
    sriov: &mut SriovState,
    profile_idx: usize,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(frame.area());

    let profile_name = sriov
        .profiles
        .get(profile_idx)
        .map(|(n, _)| n.clone())
        .unwrap_or_else(|| "?".to_string());

    common::header_bar(
        frame,
        chunks[0],
        &format!("SR-IOV > {} > FDB", profile_name),
        daemon_online,
    );

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!("  {} managed FDB entries", sriov.fdb_entries.len()),
        common::style_muted(),
    )));
    lines.push(Line::from(""));

    if sriov.fdb_entries.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No entries (daemon offline, FDB disabled, or no VMs on the watched bridge).",
            common::style_muted(),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            format!("  {:<8}  {:<20}  {}", "VM", "MAC", "Bridge"),
            common::style_muted(),
        )));
        lines.push(Line::from(Span::styled(
            format!("  {}", "─".repeat(46)),
            Style::default().fg(common::BORDER),
        )));
        for e in &sriov.fdb_entries {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<8}  ", e.vmid), common::style_value()),
                Span::styled(
                    format!("{:<20}  ", e.mac),
                    Style::default().fg(common::TEXT),
                ),
                Span::styled(e.bridge.clone(), common::style_muted()),
            ]));
        }
    }

    // Clamp scroll to content (borders take 2 rows).
    let viewport = chunks[1].height.saturating_sub(2);
    let max_scroll = (lines.len() as u16).saturating_sub(viewport);
    sriov.fdb_scroll = sriov.fdb_scroll.min(max_scroll);

    let para = Paragraph::new(lines).scroll((sriov.fdb_scroll, 0)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(common::BORDER)),
    );
    frame.render_widget(para, chunks[1]);

    common::footer_bar(frame, chunks[2], &[("↑↓", "Scroll"), ("Esc", "Back")]);
}

pub fn handle_fdb_detail_input(
    sriov: &mut SriovState,
    key: KeyEvent,
    profile_idx: usize,
) -> SriovUiAction {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('f') => {
            sriov.fdb_scroll = 0;
            SriovUiAction::NavigateTo(SriovScreen::Status(profile_idx))
        }
        KeyCode::Up => {
            sriov.fdb_scroll = sriov.fdb_scroll.saturating_sub(1);
            SriovUiAction::None
        }
        KeyCode::Down => {
            sriov.fdb_scroll = sriov.fdb_scroll.saturating_add(1);
            SriovUiAction::None
        }
        _ => SriovUiAction::None,
    }
}

// ── Tab editor ──

pub fn render_editor(
    frame: &mut Frame,
    daemon_online: bool,
    sriov: &mut SriovState,
    profile_idx: usize,
) {
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
    } else if let Some((name, _)) = sriov.profiles.get(profile_idx) {
        format!("SR-IOV > {}", name)
    } else {
        "SR-IOV > Editor".to_string()
    };

    common::header_bar(frame, chunks[0], &title, daemon_online);

    let tab_titles: Vec<Line> = EditorTab::ALL
        .iter()
        .map(|t| {
            let style = if *t == sriov.active_tab {
                Style::default()
                    .fg(common::BRAND)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                Style::default().fg(common::TEXT_MUTED)
            };
            Line::from(Span::styled(t.title(), style))
        })
        .collect();

    let tabs = Tabs::new(tab_titles)
        .select(sriov.active_tab.index())
        .divider(Span::styled(" │ ", Style::default().fg(common::BORDER)));
    frame.render_widget(tabs, chunks[1]);

    match sriov.active_tab {
        EditorTab::Pf => pf_select::render(frame, sriov, chunks[2]),
        EditorTab::General => vf_config::render_general(frame, sriov, chunks[2]),
        EditorTab::VfTable => vf_config::render_vf_table(frame, sriov, chunks[2]),
        EditorTab::Fdb => fdb_config::render(frame, sriov, chunks[2]),
        EditorTab::Review => review::render(frame, sriov, chunks[2]),
    }
    // (other tab renderers take &SriovState and reborrow from the &mut)

    if sriov.active_tab != EditorTab::Review {
        if let Some(msg) = &sriov.message {
            let msg_area = ratatui::layout::Rect {
                x: chunks[2].x,
                y: chunks[2].y + chunks[2].height.saturating_sub(1),
                width: chunks[2].width,
                height: 1,
            };
            let msg_para = Paragraph::new(Line::from(Span::styled(
                format!("  {}", msg),
                common::style_warn(),
            )));
            frame.render_widget(msg_para, msg_area);
        }
    }

    let hints: Vec<(&str, &str)> = if sriov.edit_focus.is_some() {
        vec![("Enter", "Confirm"), ("Esc", "Cancel")]
    } else if sriov.active_tab == EditorTab::Review {
        vec![
            ("Tab/◀▶", "Switch Tab"),
            ("↑↓", "Scroll"),
            ("Ctrl+S", "Save Only"),
            ("Enter", "Save & Apply"),
            ("Esc", "Back"),
        ]
    } else {
        vec![
            ("Tab/◀▶", "Switch Tab"),
            ("↑↓", "Navigate"),
            ("Enter", "Edit/Select"),
            ("Ctrl+S", "Save Only"),
            ("Esc", "Back"),
        ]
    };
    common::footer_bar(frame, chunks[3], &hints);
}

pub async fn handle_editor_input(
    sriov: &mut SriovState,
    key: KeyEvent,
    _profile_idx: Option<usize>,
) -> SriovUiAction {
    if sriov.edit_focus.is_some() {
        if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
            do_save(sriov).await;
            return SriovUiAction::None;
        }
        match sriov.active_tab {
            EditorTab::Pf => pf_select::handle_input(sriov, key),
            EditorTab::General => vf_config::handle_general_input(sriov, key),
            EditorTab::VfTable => vf_config::handle_vf_table_input(sriov, key),
            EditorTab::Fdb => fdb_config::handle_input(sriov, key),
            EditorTab::Review => review::handle_input(sriov, key),
        }
        return SriovUiAction::None;
    }

    match key.code {
        KeyCode::Esc => {
            return SriovUiAction::NavigateTo(SriovScreen::Profiles);
        }
        KeyCode::Char('q') if key.modifiers.is_empty() => {
            return SriovUiAction::NavigateTo(SriovScreen::Profiles);
        }
        KeyCode::Tab | KeyCode::Right => {
            let next = (sriov.active_tab.index() + 1) % EditorTab::ALL.len();
            sriov.active_tab = EditorTab::from_index(next);
            return SriovUiAction::None;
        }
        KeyCode::BackTab | KeyCode::Left => {
            let cur = sriov.active_tab.index();
            let prev = if cur == 0 {
                EditorTab::ALL.len() - 1
            } else {
                cur - 1
            };
            sriov.active_tab = EditorTab::from_index(prev);
            return SriovUiAction::None;
        }
        KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            do_save(sriov).await;
            return SriovUiAction::None;
        }
        KeyCode::Enter if sriov.active_tab == EditorTab::Review => {
            do_save_and_apply(sriov).await;
            return SriovUiAction::None;
        }
        _ => {}
    }

    match sriov.active_tab {
        EditorTab::Pf => pf_select::handle_input(sriov, key),
        EditorTab::General => vf_config::handle_general_input(sriov, key),
        EditorTab::VfTable => vf_config::handle_vf_table_input(sriov, key),
        EditorTab::Fdb => fdb_config::handle_input(sriov, key),
        EditorTab::Review => review::handle_input(sriov, key),
    }
    SriovUiAction::None
}

async fn do_save(sriov: &mut SriovState) {
    sriov.edit_focus = None;
    match sriov.save_config() {
        Ok(()) => {
            let mut msg = format!("✓ Config saved to {}", sriov_config_path().display());
            if perigee_core::client::IpcClient::is_daemon_running() {
                match perigee_core::client::IpcClient::send(&Request::Reload).await {
                    Ok(Response::Ok) => {
                        msg.push_str(" — daemon config reloaded (not yet applied).");
                    }
                    Ok(Response::Error { message }) => {
                        msg.push_str(&format!(" — reload error: {}", message));
                    }
                    _ => {}
                }
            }
            sriov.message = Some(msg);
            sriov.active_tab = EditorTab::Review;
        }
        Err(e) => {
            sriov.message = Some(format!("✗ Save failed: {}", e));
            sriov.active_tab = EditorTab::Review;
        }
    }
}

async fn do_save_and_apply(sriov: &mut SriovState) {
    sriov.edit_focus = None;
    let profile_name = sriov.editing_name.trim().to_string();
    match sriov.save_config() {
        Ok(()) => {
            let mut msg = "✓ Config saved".to_string();
            if perigee_core::client::IpcClient::is_daemon_running() {
                match perigee_core::client::IpcClient::send(&Request::Reload).await {
                    Ok(Response::Ok) => {}
                    Ok(Response::Error { message }) => {
                        msg.push_str(&format!(", reload error: {}", message));
                    }
                    _ => {}
                }
                if !profile_name.is_empty() {
                    match perigee_core::client::IpcClient::send(&Request::Apply {
                        profile: profile_name,
                    })
                    .await
                    {
                        Ok(Response::Ok) => {
                            msg.push_str(" and applied to system.");
                        }
                        Ok(Response::Error { message }) => {
                            msg.push_str(&format!(", apply error: {}", message));
                        }
                        _ => {
                            msg.push_str(", apply: unexpected response.");
                        }
                    }
                }
            }
            sriov.message = Some(msg);
            sriov.active_tab = EditorTab::Review;
        }
        Err(e) => {
            sriov.message = Some(format!("✗ Save failed: {}", e));
            sriov.active_tab = EditorTab::Review;
        }
    }
}

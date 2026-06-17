use crossterm::event::{KeyCode, KeyEvent};
use perigee_tui as common;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use super::{AffinityScreen, AffinityState, AffinityUiAction};
use crate::affinity::{cpus_to_ccd_names, parse_affinity_str};
use crate::config::{affinity_config_path, AffinityFileConfig};
use crate::topology::Architecture;

pub fn render(frame: &mut Frame, daemon_online: bool, state: &mut AffinityState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(frame.area());

    common::header_bar(frame, chunks[0], "CPU Affinity", daemon_online);

    let mut lines: Vec<Line> = Vec::new();

    if let Some(ref err) = state.topo_error {
        lines.push(Line::from(Span::styled(
            format!("  Topology detection failed: {}", err),
            common::style_error(),
        )));
    } else if let Some(ref topo) = state.topology {
        let section = |text: &str| -> Line<'static> {
            Line::from(Span::styled(
                format!("  {}", text),
                Style::default()
                    .fg(common::TEXT)
                    .add_modifier(Modifier::BOLD),
            ))
        };
        let kv = |label: &str, value: String| -> Line<'static> {
            Line::from(vec![
                Span::styled(format!("  {:<16}", label), common::style_label()),
                Span::styled(value, common::style_value()),
            ])
        };

        lines.push(section("── CPU Info ──"));
        lines.push(kv("Architecture:", topo.architecture.to_string()));
        lines.push(kv(
            "Total:",
            format!(
                "{} logical / {} physical / SMT {}",
                topo.total_cpus,
                topo.total_cores,
                if topo.has_smt { "✓" } else { "✗" }
            ),
        ));

        let auto_enabled = AffinityFileConfig::load(&affinity_config_path())
            .map(|c| c.affinity.auto_apply.enabled)
            .unwrap_or(false);
        lines.push(Line::from(vec![
            Span::styled(
                "  Auto Binding:  ",
                common::style_label().add_modifier(Modifier::BOLD),
            ),
            if auto_enabled {
                Span::styled(
                    "Enabled",
                    Style::default()
                        .fg(common::SUCCESS)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(
                    "Disabled",
                    Style::default()
                        .fg(common::ERROR)
                        .add_modifier(Modifier::BOLD),
                )
            },
            Span::styled(
                "  (press a to configure)",
                Style::default().fg(common::TEXT_DIM),
            ),
        ]));

        lines.push(Line::from(""));

        let bindings = state.existing_bindings();
        let ccd_stats = compute_ccd_stats(&bindings, &topo.core_groups);

        let bar_width = 8usize;

        match topo.architecture {
            Architecture::IntelHybrid => {
                lines.push(section("── Core Groups ──"));
                for (idx, cg) in topo.core_groups.iter().enumerate() {
                    let cores = cg.physical_cpus.len();
                    let threads = cg.all_cpus.len();
                    let st = &ccd_stats[idx];
                    lines.push(build_ccd_line(
                        &format!("  {:<10}", cg.name),
                        None,
                        cores,
                        threads,
                        st,
                        bar_width,
                    ));
                }
            }
            _ => {
                for pkg in &topo.packages {
                    lines.push(section(&format!("── Package {} ──", pkg.id)));
                    for cg in &pkg.core_groups {
                        let global_idx = topo
                            .core_groups
                            .iter()
                            .position(|g| g.name == cg.name && g.package_id == cg.package_id)
                            .unwrap_or(0);
                        let cores = cg.physical_cpus.len();
                        let threads = cg.all_cpus.len();
                        let l3 = if cg.l3_cache_id >= 0 {
                            Some(format!("L3#{:<3}", cg.l3_cache_id))
                        } else {
                            None
                        };
                        let st = &ccd_stats[global_idx];
                        lines.push(build_ccd_line(
                            &format!("  {:<7}", cg.name),
                            l3.as_deref(),
                            cores,
                            threads,
                            st,
                            bar_width,
                        ));
                    }
                    lines.push(Line::from(""));
                }
            }
        }

        if !state.vms.is_empty() {
            lines.push(section("── VM Bindings ──"));
            lines.push(Line::from(Span::styled(
                format!(
                    "    {:<6} {:<12} {:>3}  {:<26} {}",
                    "VMID", "Name", "vC", "Affinity", "CCD"
                ),
                common::style_muted(),
            )));

            let mut bound = 0usize;
            for vm in &state.vms {
                let cfg = state.vm_configs.get(&vm.vmid);
                let cores = cfg.map(|c| c.cores).unwrap_or(0);
                let (aff_str, ccd_str) = if let Some(aff) = cfg.and_then(|c| c.affinity.as_ref()) {
                    bound += 1;
                    let cpus = parse_affinity_str(aff);
                    let ccds = cpus_to_ccd_names(&cpus, &topo.core_groups);
                    let ccd_ids: Vec<String> = ccds.iter().map(|n| extract_ccd_id(n)).collect();
                    (aff.to_string(), format!("[{}]", ccd_ids.join(",")))
                } else {
                    ("—".to_string(), "—".to_string())
                };

                let status_dot = if vm.status == "running" {
                    Span::styled("● ", Style::default().fg(common::SUCCESS))
                } else {
                    Span::styled("● ", Style::default().fg(common::TEXT_MUTED))
                };

                let name_display = if vm.name.len() > 10 {
                    format!("{:.10}…", vm.name)
                } else {
                    vm.name.clone()
                };

                let aff_display = if aff_str.len() > 24 {
                    format!("{:.24}…", aff_str)
                } else {
                    aff_str
                };

                lines.push(Line::from(vec![
                    Span::raw("  "),
                    status_dot,
                    Span::styled(format!("{:<6}", vm.vmid), common::style_value()),
                    Span::styled(
                        format!("{:<12}", name_display),
                        Style::default().fg(common::TEXT_DIM),
                    ),
                    Span::styled(
                        format!("{:>3}  ", cores),
                        Style::default().fg(common::TEXT_DIM),
                    ),
                    Span::styled(
                        format!("{:<26} ", aff_display),
                        Style::default().fg(common::TEXT_DIM),
                    ),
                    Span::styled(ccd_str, Style::default().fg(common::BRAND_DIM)),
                ]));
            }

            let unbound = state.vms.len() - bound;
            lines.push(Line::from(Span::styled(
                format!(
                    "  {} VMs, {} bound, {} unbound",
                    state.vms.len(),
                    bound,
                    unbound
                ),
                common::style_muted(),
            )));
        }
    } else {
        lines.push(Line::from(Span::styled(
            "  Loading topology...",
            common::style_muted(),
        )));
    }

    let visible_height = chunks[1].height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(visible_height);
    state.topo_max_scroll = max_scroll;
    if state.topo_scroll > max_scroll {
        state.topo_scroll = max_scroll;
    }

    let para = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(common::BORDER)),
        )
        .scroll((state.topo_scroll as u16, 0));
    frame.render_widget(para, chunks[1]);

    common::footer_bar(
        frame,
        chunks[2],
        &[
            ("Enter", "Configure"),
            ("a", "Auto Apply"),
            ("r", "Refresh"),
            ("q", "Back"),
        ],
    );
}

struct CcdStats {
    unique_threads: usize,
    total_assigned: usize,
}

fn compute_ccd_stats(
    bindings: &[crate::affinity::VmBinding],
    core_groups: &[crate::topology::CoreGroup],
) -> Vec<CcdStats> {
    let mut unique_sets: Vec<std::collections::HashSet<usize>> =
        vec![std::collections::HashSet::new(); core_groups.len()];
    let mut total_assigned: Vec<usize> = vec![0; core_groups.len()];

    for binding in bindings {
        for &cpu in &binding.cpus {
            for (idx, cg) in core_groups.iter().enumerate() {
                if cg.all_cpus.contains(&cpu) {
                    unique_sets[idx].insert(cpu);
                    total_assigned[idx] += 1;
                }
            }
        }
    }

    unique_sets
        .into_iter()
        .zip(total_assigned)
        .map(|(uniq, total)| CcdStats {
            unique_threads: uniq.len(),
            total_assigned: total,
        })
        .collect()
}

fn build_ccd_line<'a>(
    name_label: &str,
    l3_label: Option<&str>,
    cores: usize,
    threads: usize,
    stats: &CcdStats,
    bar_width: usize,
) -> Line<'a> {
    let unique = stats.unique_threads;
    let filled = (unique * bar_width).checked_div(threads).unwrap_or(0);
    let empty = bar_width - filled;

    let bar_color = if unique == 0 {
        common::SUCCESS
    } else if unique >= threads {
        common::ERROR
    } else {
        common::BRAND
    };

    let label = if unique == 0 {
        "idle".to_string()
    } else {
        let oversub = if stats.total_assigned > unique {
            let ratio = stats.total_assigned as f64 / unique as f64;
            format!("  {:.1}x ⚠", ratio)
        } else {
            String::new()
        };
        format!("{}/{} used{}", unique, threads, oversub)
    };

    let mut spans = vec![Span::styled(
        name_label.to_string(),
        common::style_selected(),
    )];

    if let Some(l3) = l3_label {
        spans.push(Span::styled(
            format!("{} ", l3),
            Style::default().fg(common::TEXT_MUTED),
        ));
    }

    spans.extend([
        Span::styled(format!("{}C/{}T  ", cores, threads), common::style_value()),
        Span::styled("█".repeat(filled), Style::default().fg(bar_color)),
        Span::styled("░".repeat(empty), Style::default().fg(common::TEXT_MUTED)),
        Span::styled(
            format!("  {}", label),
            Style::default().fg(common::TEXT_DIM),
        ),
    ]);

    Line::from(spans)
}

fn extract_ccd_id(name: &str) -> String {
    name.strip_prefix("CCD ")
        .or_else(|| name.strip_prefix("P-Core "))
        .or_else(|| name.strip_prefix("E-Core "))
        .unwrap_or(name)
        .to_string()
}

pub fn handle_input(state: &mut AffinityState, key: KeyEvent) -> AffinityUiAction {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => AffinityUiAction::GoBack,
        KeyCode::Enter => {
            state.editing_cores = true;
            state.cores_input = state.cores_needed.to_string();
            state.regenerate_strategies();
            AffinityUiAction::NavigateTo(AffinityScreen::Strategy)
        }
        KeyCode::Char('a') => {
            state.generate_auto_plan();
            AffinityUiAction::NavigateTo(AffinityScreen::AutoApply)
        }
        KeyCode::Char('r') => {
            state.detect_topology();
            state.refresh_vms();
            state.message = Some("Refreshed".to_string());
            AffinityUiAction::None
        }
        KeyCode::Char('t') => {
            if let Some(ref topo) = state.topology {
                if let Ok(json) = serde_json::to_string_pretty(topo) {
                    state.message = Some(format!("Topology JSON:\n{}", json));
                }
            }
            AffinityUiAction::None
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if state.topo_scroll > 0 {
                state.topo_scroll -= 1;
            }
            AffinityUiAction::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if state.topo_scroll < state.topo_max_scroll {
                state.topo_scroll += 1;
            }
            AffinityUiAction::None
        }
        _ => AffinityUiAction::None,
    }
}

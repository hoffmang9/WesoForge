use std::collections::VecDeque;
use std::io::Stdout;
use std::time::Instant;

use bbr_client_engine::JobSummary;
use ratatui::Terminal;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::prelude::{Color, CrosstermBackend, Line, Modifier, Span, Style};
use ratatui::widgets::{List, ListItem, Paragraph};

use crate::format::{field_vdf_label, format_number};

const PROGRESS_BAR_WIDTH: usize = 20;
const MAX_LOG_LINES: usize = 200;
const MAX_RENDERED_LOG_LINES: usize = 4;

struct WorkerUiState {
    total_iters: u64,
    reported_iters_done: u64,
    display_iters_done: u64,
    iters_per_sec: u64,
    last_reported_at: Instant,
    label: String,
}

pub(crate) struct Ui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    worker_states: Vec<WorkerUiState>,
    worker_prefix_width: usize,
    global_message: String,
    stop_message: String,
    logs: VecDeque<String>,
}

impl Ui {
    pub(crate) fn new(worker_count: usize) -> anyhow::Result<Self> {
        let backend = CrosstermBackend::new(std::io::stdout());
        let terminal = Terminal::new(backend)?;
        let mut worker_states = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            worker_states.push(WorkerUiState {
                total_iters: 0,
                reported_iters_done: 0,
                display_iters_done: 0,
                iters_per_sec: 0,
                last_reported_at: Instant::now(),
                label: "Idle".to_string(),
            });
        }

        let mut ui = Self {
            terminal,
            worker_states,
            worker_prefix_width: worker_count.max(1).ilog10() as usize + 1,
            global_message: "Global: 0 it/s".to_string(),
            stop_message: " ".to_string(),
            logs: VecDeque::new(),
        };
        ui.redraw();
        Ok(ui)
    }

    pub(crate) fn println(&mut self, msg: &str) {
        if self.logs.len() >= MAX_LOG_LINES {
            self.logs.pop_front();
        }
        self.logs.push_back(msg.to_string());
        self.redraw();
    }

    fn set_worker_work(&mut self, worker_idx: usize, msg: String, total_iters: u64) {
        let Some(state) = self.worker_states.get_mut(worker_idx) else {
            return;
        };
        state.total_iters = total_iters.max(1);
        state.reported_iters_done = 0;
        state.display_iters_done = 0;
        state.iters_per_sec = 0;
        state.last_reported_at = Instant::now();
        state.label = msg;
        self.redraw();
    }

    pub(crate) fn set_worker_job(&mut self, worker_idx: usize, job: &JobSummary) {
        if let Some(count) = job.group_proofs.filter(|count| *count > 1) {
            self.set_worker_work(
                worker_idx,
                format!("Group {count} proofs"),
                job.number_of_iterations,
            );
            return;
        }

        let field = field_vdf_label(job.field_vdf);
        self.set_worker_work(
            worker_idx,
            format!("Block {} ({field})", job.height),
            job.number_of_iterations,
        );
    }

    pub(crate) fn set_worker_progress(
        &mut self,
        worker_idx: usize,
        iters_done: u64,
        iters_per_sec: u64,
    ) {
        let Some(state) = self.worker_states.get_mut(worker_idx) else {
            return;
        };
        if state.total_iters == 0 {
            return;
        }
        state.reported_iters_done = iters_done.min(state.total_iters);
        state.display_iters_done = state.display_iters_done.max(state.reported_iters_done);
        state.iters_per_sec = iters_per_sec;
        state.last_reported_at = Instant::now();
        self.redraw();
    }

    pub(crate) fn set_worker_idle(&mut self, worker_idx: usize) {
        let Some(state) = self.worker_states.get_mut(worker_idx) else {
            return;
        };
        state.total_iters = 0;
        state.reported_iters_done = 0;
        state.display_iters_done = 0;
        state.iters_per_sec = 0;
        state.last_reported_at = Instant::now();
        state.label = "Idle".to_string();
        self.redraw();
    }

    pub(crate) fn set_stop_message(&mut self, msg: &str) {
        self.stop_message = msg.to_string();
        self.redraw();
    }

    pub(crate) fn tick_global(&mut self, speed: u64, busy: usize, total: usize) {
        self.global_message = format!(
            "Global: {} it/s (running {busy}/{total})",
            format_number(speed)
        );
        let now = Instant::now();
        for state in &mut self.worker_states {
            if state.total_iters == 0 || state.iters_per_sec == 0 {
                continue;
            }
            let elapsed = now.saturating_duration_since(state.last_reported_at);
            if elapsed.is_zero() {
                continue;
            }
            let predicted_iters = state
                .reported_iters_done
                .saturating_add((elapsed.as_secs_f64() * state.iters_per_sec as f64) as u64)
                .min(state.total_iters);
            state.display_iters_done = state.display_iters_done.max(predicted_iters);
        }
        self.redraw();
    }

    pub(crate) fn freeze(&mut self) {
        self.redraw();
    }

    fn redraw(&mut self) {
        let global_message = self.global_message.clone();
        let stop_message = self.stop_message.clone();
        let worker_lines: Vec<Line<'static>> = self
            .worker_states
            .iter()
            .enumerate()
            .map(|(idx, state)| self.worker_line(idx, state))
            .collect();
        let rendered_logs: Vec<String> = self
            .logs
            .iter()
            .rev()
            .take(MAX_RENDERED_LOG_LINES)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();

        let _ = self.terminal.draw(|frame| {
            let areas = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),
                    Constraint::Min(1),
                    Constraint::Length(1),
                ])
                .split(frame.area());

            let global = Paragraph::new(global_message.as_str())
                .style(Style::default().add_modifier(Modifier::BOLD));
            frame.render_widget(global, areas[0]);

            let middle = if rendered_logs.is_empty() {
                None
            } else {
                let logs_height =
                    (rendered_logs.len() as u16).min(areas[1].height.saturating_sub(1));
                if logs_height == 0 {
                    None
                } else {
                    let split = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([Constraint::Min(1), Constraint::Length(logs_height)])
                        .split(areas[1]);
                    let logs = Paragraph::new(rendered_logs.join("\n"));
                    frame.render_widget(logs, split[1]);
                    Some(split[0])
                }
            };

            let workers_area = middle.unwrap_or(areas[1]);
            let workers: Vec<ListItem> = worker_lines.into_iter().map(ListItem::new).collect();
            frame.render_widget(List::new(workers), workers_area);

            let stop = Paragraph::new(stop_message.as_str());
            frame.render_widget(stop, areas[2]);
        });
    }

    fn worker_line(&self, worker_idx: usize, state: &WorkerUiState) -> Line<'static> {
        let prefix = format!(
            "W{:0width$}",
            worker_idx + 1,
            width = self.worker_prefix_width
        );
        if state.total_iters == 0 {
            return Line::from(format!("{prefix}  Idle"));
        }

        let total_iters = state.total_iters.max(1);
        let iters_done = state.display_iters_done.min(total_iters);
        let percent = (iters_done.saturating_mul(100) / total_iters).min(100);
        let filled = (iters_done.saturating_mul(PROGRESS_BAR_WIDTH as u64) / total_iters)
            .min(PROGRESS_BAR_WIDTH as u64) as usize;
        let eta = if state.iters_per_sec == 0 || iters_done >= total_iters {
            "--:--:--".to_string()
        } else {
            format_eta((total_iters - iters_done) / state.iters_per_sec)
        };

        let before_bar = format!("{prefix}  {percent:>3}%[");
        let bar_filled = "#".repeat(filled);
        let bar_remaining = "-".repeat(PROGRESS_BAR_WIDTH.saturating_sub(filled));
        let after_bar = format!(
            "] {eta} ETA  {:>12} it/s  {:>11} it   {}",
            format_number(state.iters_per_sec),
            format_number(iters_done),
            state.label
        );

        Line::from(vec![
            Span::raw(before_bar),
            Span::styled(bar_filled, Style::default().fg(Color::Cyan)),
            Span::styled(bar_remaining, Style::default().fg(Color::Blue)),
            Span::raw(after_bar),
        ])
    }
}

fn format_eta(total_seconds: u64) -> String {
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

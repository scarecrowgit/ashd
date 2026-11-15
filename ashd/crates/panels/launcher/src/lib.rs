//! Launcher (det-04): one input, modes by prefix. Apps and open windows by default;
//! `=` calc, `/` files, `>` run, `:` emoji, `!` web (`ashd launcher --mode …` opens with the
//! prefix selected). Enter acts on the selected row (spawns go through niri), Shift+Enter is
//! the alternate action, Escape / outside click closes.
//!
//! Commands (the `>` mode, and a "Run …" row for a query that matches no application):
//! Enter runs the line here and shows its stdout / stderr under the input (`runner.rs`,
//! `sh -lc`, killed when the launcher closes); Ctrl+Enter runs it in the configured
//! terminal, which stays open (`kitty --hold sh -lc …`); Shift+Enter runs it detached and
//! silent through niri.
//!
//! Owner: launcher. Layout: `view.rs`; result rows per mode: `rows.rs`; pure helpers:
//! `model.rs`, `calc.rs`, `files.rs`, `emoji.rs`, `history.rs`, `runner.rs`.

mod calc;
mod emoji;
mod files;
mod history;
mod model;
mod rows;
mod runner;
mod view;

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use gpui::{
    AppContext as _, ClipboardItem, Context, Entity, Focusable as _, KeyBinding, Task, Window, actions,
};
use nucleo_matcher::Matcher;
use ashd_ipc::{LauncherMode, PanelOp, Request, Response};
use ashd_niri::niri_ipc::Action;
use ashd_shell_core::{ModalStyle, Panel, PanelContext, PanelId, Placement};
use ashd_ui::{TextInput, TextInputEvent};

use calc::{Calc, CalcOut};
use files::FileResults;
use model::{Act, MAX_VISIBLE, Row, mode_for_prefix, placeholder, plan_run, terminal_run_argv};
use runner::{Exit, Msg, OutputBuf};

actions!(launcher, [AltSubmit, CtrlSubmit, ExitMode, SelectNext, SelectPrev, CopyOutput]);

const FILE_DEBOUNCE: Duration = Duration::from_millis(140);

/// Which of the three submit keys was pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Submit {
    /// Enter.
    Primary,
    /// Shift+Enter.
    Alt,
    /// Ctrl+Enter.
    Ctrl,
}

/// How the command being shown ended.
pub(crate) enum RunStatus {
    Running,
    Done { exit: Exit, took: Duration },
    /// The shell could not be started.
    Failed(String),
}

/// A command run inside the launcher and what it printed.
pub(crate) struct RunView {
    pub command: String,
    pub buf: OutputBuf,
    pub status: RunStatus,
    pub started: Instant,
    /// First visible output line; `follow` keeps it at the end while the output grows.
    pub scroll: usize,
    pub follow: bool,
    handle: runner::Handle,
    _reader: Task<()>,
    _tick: Task<()>,
}

impl RunView {
    pub fn max_scroll(&self) -> usize {
        self.buf.visible_len().saturating_sub(view::OUT_LINES)
    }
}

#[derive(Default)]
pub(crate) struct FilesState {
    /// Bumped per query; stale background results are dropped.
    pub seq: u64,
    pub results: Option<FileResults>,
    pub searching: bool,
}

pub struct LauncherPanel {
    pub(crate) ctx: PanelContext,
    pub(crate) input: Entity<TextInput>,
    /// `None` = apps and windows.
    pub(crate) mode: Option<LauncherMode>,
    pub(crate) rows: Vec<Row>,
    pub(crate) selected: usize,
    /// First visible row (the list shows [`MAX_VISIBLE`] at a time).
    pub(crate) scroll: usize,
    pub(crate) open: bool,
    /// Restarts the enter animation on every open.
    pub(crate) open_seq: u64,
    pub(crate) calc: Calc,
    pub(crate) calc_out: CalcOut,
    pub(crate) files: FilesState,
    /// The command whose output is on screen.
    pub(crate) run: Option<RunView>,
    /// `>` history, newest first (loaded on open).
    pub(crate) run_history: Vec<(String, u64)>,
    pub(crate) recent_emoji: Vec<String>,
    terminal: Vec<String>,
    plocate: Option<PathBuf>,
    matcher: Matcher,
}

impl Panel for LauncherPanel {
    const ID: PanelId = PanelId("launcher");
    // The mockup shows no dim behind the launcher.
    const PLACEMENT: Placement = Placement::Modal(ModalStyle { scrim: false });

    fn new(ctx: PanelContext, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new(placeholder(None), cx));
        cx.subscribe(&input, |this: &mut Self, _, ev: &TextInputEvent, cx| this.on_input_event(ev, cx)).detach();
        cx.observe(&ctx.services.apps.state, |this, _, cx| {
            if this.open {
                this.recompute(cx);
            }
        })
        .detach();
        cx.observe(&ctx.niri, |this, _, cx| {
            if this.open && this.mode.is_none() {
                this.recompute(cx);
            }
        })
        .detach();
        cx.bind_keys([
            KeyBinding::new("shift-enter", AltSubmit, Some("Launcher")),
            KeyBinding::new("ctrl-enter", CtrlSubmit, Some("Launcher")),
            // over the input's own copy: the output is copied unless the input has a selection
            KeyBinding::new("ctrl-c", CopyOutput, Some("LauncherOutput > TextInput")),
            KeyBinding::new("backspace", ExitMode, Some("LauncherEmpty > TextInput")),
            KeyBinding::new("ctrl-n", SelectNext, Some("Launcher")),
            KeyBinding::new("ctrl-j", SelectNext, Some("Launcher")),
            KeyBinding::new("ctrl-p", SelectPrev, Some("Launcher")),
            KeyBinding::new("ctrl-k", SelectPrev, Some("Launcher")),
            KeyBinding::new("shift-tab", SelectPrev, Some("Launcher")),
        ]);
        // Terminal prefix and plocate are looked up off the main thread.
        let probe = cx.background_spawn(async { (ashd_svc_apps::terminal_command(), files::find_plocate()) });
        cx.spawn(async move |this, cx| {
            let (terminal, plocate) = probe.await;
            this.update(cx, |this, _| {
                this.terminal = terminal;
                this.plocate = plocate;
            })
            .ok();
        })
        .detach();
        Self {
            ctx,
            input,
            mode: None,
            rows: Vec::new(),
            selected: 0,
            scroll: 0,
            open: false,
            open_seq: 0,
            calc: Calc::new(),
            calc_out: CalcOut::Empty,
            files: FilesState::default(),
            run: None,
            run_history: Vec::new(),
            recent_emoji: Vec::new(),
            terminal: vec!["xterm".into(), "-e".into()],
            plocate: None,
            matcher: emoji::matcher(),
        }
    }

    fn on_request(&mut self, request: &Request, cx: &mut Context<Self>) -> Option<Response> {
        let Request::Launcher { op, mode } = request else { return None };
        let open = self.ctx.shell.modal(cx).is_some_and(|m| m.panel == Self::ID);
        match (op, open) {
            // `ashd launcher --mode calc` while open in another mode switches mode.
            (PanelOp::Toggle, true) if mode.is_some() && *mode != self.mode => self.set_mode(*mode, String::new(), cx),
            (PanelOp::Open, true) if mode.is_some() => self.set_mode(*mode, String::new(), cx),
            (PanelOp::Open, true) => {}
            (PanelOp::Open, false) | (PanelOp::Toggle, false) => {
                self.open = true;
                self.set_mode(*mode, String::new(), cx);
                self.ctx.shell.open_modal(Self::ID, None, cx);
            }
            (PanelOp::Close, _) | (PanelOp::Toggle, true) => self.ctx.shell.close_modal(cx),
        }
        Some(Response::Ok)
    }

    fn on_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = true;
        self.open_seq += 1;
        self.input.focus_handle(cx).focus(window, cx);
        self.recompute(cx);
        let task = cx.background_spawn(async { history::load() });
        cx.spawn(async move |this, cx| {
            let loaded = task.await;
            this.update(cx, |this, cx| {
                this.run_history = loaded;
                if this.open && matches!(this.mode, Some(LauncherMode::Run) | None) {
                    this.recompute(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn on_close(&mut self, cx: &mut Context<Self>) {
        self.stop_run();
        self.open = false;
        self.mode = None;
        self.input.update(cx, |i, cx| {
            i.set_text("", cx);
            i.set_placeholder(placeholder(None), cx);
        });
        self.rows.clear();
        self.selected = 0;
        self.scroll = 0;
        self.files.seq += 1;
        self.files.results = None;
        self.files.searching = false;
    }
}

impl LauncherPanel {
    pub(crate) fn query(&self, cx: &gpui::App) -> String {
        self.input.read(cx).text().to_string()
    }

    fn on_input_event(&mut self, ev: &TextInputEvent, cx: &mut Context<Self>) {
        match ev {
            TextInputEvent::Changed => self.on_changed(cx),
            TextInputEvent::Submit => self.activate(Submit::Primary, cx),
            TextInputEvent::Cancel => self.ctx.shell.close_modal(cx),
            TextInputEvent::Up if self.run.is_some() => self.scroll_output(-3, cx),
            TextInputEvent::Down | TextInputEvent::Tab if self.run.is_some() => self.scroll_output(3, cx),
            TextInputEvent::Up => self.move_selection(-1, cx),
            TextInputEvent::Down | TextInputEvent::Tab => self.move_selection(1, cx),
            TextInputEvent::Blur => {}
        }
    }

    fn on_changed(&mut self, cx: &mut Context<Self>) {
        // editing the line leaves the output (and ends a command still running)
        self.stop_run();
        let text = self.query(cx);
        if self.mode.is_none()
            && let Some(first) = text.chars().next()
            && let Some(mode) = mode_for_prefix(first)
        {
            // typing (or pasting) a prefix as the first character selects the mode
            self.set_mode(Some(mode), text[first.len_utf8()..].to_string(), cx);
            return;
        }
        self.selected = 0;
        self.scroll = 0;
        self.recompute(cx);
    }

    pub(crate) fn set_mode(&mut self, mode: Option<LauncherMode>, text: String, cx: &mut Context<Self>) {
        self.stop_run();
        self.mode = mode;
        self.input.update(cx, |i, cx| {
            i.set_text(text, cx);
            i.set_placeholder(placeholder(mode), cx);
        });
        self.selected = 0;
        self.scroll = 0;
        self.files.results = None;
        self.recompute(cx);
    }

    pub(crate) fn exit_mode(&mut self, cx: &mut Context<Self>) {
        if self.mode.is_some() {
            self.set_mode(None, String::new(), cx);
        }
    }

    pub(crate) fn recompute(&mut self, cx: &mut Context<Self>) {
        let q = self.query(cx);
        self.rows = match self.mode {
            None => self.app_rows(&q, cx),
            Some(LauncherMode::Calc) => {
                self.calc_out = self.calc.eval(&q);
                Vec::new()
            }
            Some(LauncherMode::Files) => {
                self.start_file_search(&q, cx);
                self.file_rows(cx)
            }
            Some(LauncherMode::Run) => self.run_rows(&q),
            Some(LauncherMode::Emoji) => self.emoji_rows(&q),
            Some(LauncherMode::Web) => self.web_rows(&q),
        };
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        self.ensure_visible();
        cx.notify();
    }

    fn start_file_search(&mut self, q: &str, cx: &mut Context<Self>) {
        self.files.seq += 1;
        let seq = self.files.seq;
        if q.trim().is_empty() {
            self.files.results = None;
            self.files.searching = false;
            return;
        }
        self.files.searching = true;
        let (q, plocate) = (q.to_string(), self.plocate.clone());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FILE_DEBOUNCE).await;
            if !this.update(cx, |this, _| this.files.seq == seq).unwrap_or(false) {
                return;
            }
            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
            let results = cx.background_executor().spawn(async move { files::search(&q, &home, plocate.as_deref()) }).await;
            this.update(cx, |this, cx| {
                if this.files.seq == seq {
                    this.files.results = Some(results);
                    this.files.searching = false;
                    this.rows = this.file_rows(cx);
                    this.selected = this.selected.min(this.rows.len().saturating_sub(1));
                    this.ensure_visible();
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn ensure_visible(&mut self) {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + MAX_VISIBLE {
            self.scroll = self.selected + 1 - MAX_VISIBLE;
        }
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(MAX_VISIBLE));
    }

    pub(crate) fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let n = self.rows.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
        self.ensure_visible();
        cx.notify();
    }

    pub(crate) fn scroll_by(&mut self, rows: isize, cx: &mut Context<Self>) {
        let max = self.rows.len().saturating_sub(MAX_VISIBLE) as isize;
        let next = (self.scroll as isize + rows).clamp(0, max) as usize;
        if next != self.scroll {
            self.scroll = next;
            cx.notify();
        }
    }

    fn terminal_prefix(&self, cx: &gpui::App) -> Vec<String> {
        ashd_shell_core::config(cx)
            .apps
            .terminal
            .as_deref()
            .and_then(ashd_svc_apps::terminal_for)
            .unwrap_or_else(|| self.terminal.clone())
    }

    /// Enter, Shift+Enter or Ctrl+Enter on the selected row (or, while a command's output
    /// is shown, on that command).
    pub(crate) fn activate(&mut self, key: Submit, cx: &mut Context<Self>) {
        if self.mode == Some(LauncherMode::Calc) {
            return self.activate_calc(key == Submit::Alt, cx);
        }
        let act = if let Some(run) = &self.run {
            let line = run.command.clone();
            match key {
                Submit::Primary => Act::Run(line),
                Submit::Alt => Act::RunDetached(line),
                Submit::Ctrl => Act::RunInTerminal(line),
            }
        } else {
            let Some(row) = self.rows.get(self.selected) else { return };
            match key {
                Submit::Primary => row.primary.clone(),
                Submit::Alt => row.alt.clone().unwrap_or_else(|| row.primary.clone()),
                Submit::Ctrl => row.ctrl.clone().unwrap_or_else(|| row.primary.clone()),
            }
        };
        self.exec(act, cx);
    }

    fn activate_calc(&mut self, alt: bool, cx: &mut Context<Self>) {
        let q = self.query(cx);
        let out = self.calc.eval(&q);
        let CalcOut::Value { text, int } = out else { return };
        let copy = if alt { int.unwrap_or_else(|| text.clone()) } else { text };
        self.calc.commit(&q);
        self.exec(Act::Copy(copy), cx);
    }

    pub(crate) fn exec(&mut self, act: Act, cx: &mut Context<Self>) {
        match act {
            Act::Mode(mode, q) => {
                self.set_mode(Some(mode), q, cx);
                return;
            }
            Act::LaunchApp(app) => {
                let term = self.terminal_prefix(cx);
                self.ctx.shell.spawn(app.launch_argv(Some(&term)), cx);
                self.ctx.services.apps.handle.record_launch(&app.id);
            }
            Act::FocusWindow(id) => self.ctx.shell.niri_action(Action::FocusWindow { id }, cx),
            Act::Spawn(argv) => self.ctx.shell.spawn(argv, cx),
            Act::Run(line) | Act::RunDetached(line) | Act::RunInTerminal(line) if line.trim().is_empty() => return,
            Act::Run(line) => {
                // the launcher stays open: the output is shown under the input
                self.start_run(line, cx);
                return;
            }
            Act::RunDetached(line) => {
                let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "sh".into());
                if let Some(argv) = plan_run(&line, &shell) {
                    self.ctx.shell.spawn(argv, cx);
                    self.remember_run(&line, cx);
                }
            }
            Act::RunInTerminal(line) => {
                if let Some(argv) = terminal_run_argv(&self.terminal_prefix(cx), &line) {
                    self.ctx.shell.spawn(argv, cx);
                    self.remember_run(&line, cx);
                }
            }
            Act::Copy(text) => {
                if self.mode == Some(LauncherMode::Emoji) {
                    self.recent_emoji.retain(|e| *e != text);
                    self.recent_emoji.insert(0, text.clone());
                    self.recent_emoji.truncate(12);
                }
                self.ctx.shell.spawn(["wl-copy".to_string(), "--".to_string(), text], cx);
            }
        }
        self.ctx.shell.close_modal(cx);
    }

    /// Run `line` here: replaces the output on screen (and ends the command behind it).
    fn start_run(&mut self, line: String, cx: &mut Context<Self>) {
        self.stop_run();
        let command = line.trim().to_string();
        self.remember_run(&command, cx);
        let run = runner::spawn(&command);
        let rx = run.rx;
        let reader = cx.spawn(async move |this, cx| {
            while let Ok(first) = rx.recv().await {
                // take what is already there in one update
                let mut batch = vec![first];
                while batch.len() < 64 && let Ok(m) = rx.try_recv() {
                    batch.push(m);
                }
                let done = batch.iter().any(|m| matches!(m, Msg::Done(_)));
                if this.update(cx, |this, cx| this.on_run_msgs(batch, cx)).is_err() || done {
                    break;
                }
            }
        });
        // the elapsed time of a running command
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let running = this.update(cx, |this, cx| {
                    let running = this.run.as_ref().is_some_and(|r| matches!(r.status, RunStatus::Running));
                    if running {
                        cx.notify();
                    }
                    running
                });
                if !running.unwrap_or(false) {
                    break;
                }
            }
        });
        self.run = Some(RunView {
            command,
            buf: OutputBuf::new(),
            status: RunStatus::Running,
            started: Instant::now(),
            scroll: 0,
            follow: true,
            handle: run.handle,
            _reader: reader,
            _tick: tick,
        });
        cx.notify();
    }

    fn on_run_msgs(&mut self, msgs: Vec<Msg>, cx: &mut Context<Self>) {
        let Some(run) = self.run.as_mut() else { return };
        for m in msgs {
            match m {
                Msg::Chunk(stream, bytes) => run.buf.push(stream, &bytes),
                Msg::Done(result) => {
                    run.buf.flush();
                    run.status = match result {
                        Ok((exit, took)) => RunStatus::Done { exit, took },
                        Err(e) => RunStatus::Failed(e),
                    };
                }
            }
        }
        if run.follow {
            run.scroll = run.max_scroll();
        }
        cx.notify();
    }

    /// Forget the command on screen; a command still running is stopped.
    pub(crate) fn stop_run(&mut self) {
        if let Some(run) = self.run.take() {
            run.handle.kill();
        }
    }

    /// Scroll the output by `lines` (positive = down); reaching the end follows it again.
    pub(crate) fn scroll_output(&mut self, lines: isize, cx: &mut Context<Self>) {
        let Some(run) = self.run.as_mut() else { return };
        let max = run.max_scroll();
        let next = (run.scroll as isize + lines).clamp(0, max as isize) as usize;
        run.follow = next == max;
        if next != run.scroll {
            run.scroll = next;
            cx.notify();
        }
    }

    /// Ctrl+C in the input: put the output on the clipboard, then let the input's own copy
    /// run (it replaces it when the input has a selection).
    pub(crate) fn copy_output(&mut self, cx: &mut Context<Self>) {
        if let Some(run) = &self.run
            && !run.buf.is_empty()
        {
            cx.write_to_clipboard(ClipboardItem::new_string(run.buf.text()));
        }
        cx.propagate();
    }

    fn remember_run(&mut self, line: &str, cx: &mut Context<Self>) {
        let line = line.trim().to_string();
        let now = history::now();
        self.run_history.retain(|(c, _)| *c != line);
        self.run_history.insert(0, (line.clone(), now));
        cx.background_spawn(async move { history::append(&line, now) }).detach();
    }
}

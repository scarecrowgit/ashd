//! Launcher (det-04): one input, modes by prefix. Apps and open windows by default;
//! `=` calc, `/` files, `>` run, `:` emoji, `!` web (`ashd launcher --mode …` opens with the
//! prefix selected). Enter acts on the selected row (spawns go through niri), Shift+Enter is
//! the alternate action, Escape / outside click closes.
//!
//! Owner: launcher. Layout: `view.rs`; result rows per mode: `rows.rs`; pure helpers:
//! `model.rs`, `calc.rs`, `files.rs`, `emoji.rs`, `history.rs`.

mod calc;
mod emoji;
mod files;
mod history;
mod model;
mod rows;
mod view;

use std::{path::PathBuf, time::Duration};

use gpui::{
    AppContext as _, Context, Entity, Focusable as _, KeyBinding, Window, actions,
};
use nucleo_matcher::Matcher;
use ashd_ipc::{LauncherMode, PanelOp, Request, Response};
use ashd_niri::niri_ipc::Action;
use ashd_shell_core::{ModalStyle, Panel, PanelContext, PanelId, Placement};
use ashd_ui::{TextInput, TextInputEvent};

use calc::{Calc, CalcOut};
use files::FileResults;
use model::{Act, MAX_VISIBLE, Row, mode_for_prefix, placeholder, plan_run};

actions!(launcher, [AltSubmit, ExitMode, SelectNext, SelectPrev]);

const FILE_DEBOUNCE: Duration = Duration::from_millis(140);

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
            TextInputEvent::Submit => self.activate(false, cx),
            TextInputEvent::Cancel => self.ctx.shell.close_modal(cx),
            TextInputEvent::Up => self.move_selection(-1, cx),
            TextInputEvent::Down | TextInputEvent::Tab => self.move_selection(1, cx),
            TextInputEvent::Blur => {}
        }
    }

    fn on_changed(&mut self, cx: &mut Context<Self>) {
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

    /// Enter (`alt = false`) or Shift+Enter on the selected row.
    pub(crate) fn activate(&mut self, alt: bool, cx: &mut Context<Self>) {
        if self.mode == Some(LauncherMode::Calc) {
            return self.activate_calc(alt, cx);
        }
        let act = match self.rows.get(self.selected) {
            Some(row) if alt => row.alt.clone().unwrap_or_else(|| row.primary.clone()),
            Some(row) => row.primary.clone(),
            None => {
                // no-results state: run the query as a command, or search the web
                let q = self.query(cx);
                let q = q.trim();
                if q.is_empty() {
                    return;
                }
                match (self.mode, alt) {
                    (None, false) | (Some(LauncherMode::Run), _) => Act::Run(q.to_string()),
                    (None, true) => match model::web_targets(q, None).into_iter().next() {
                        Some((_, url, _)) => Act::Spawn(vec!["xdg-open".into(), url]),
                        None => return,
                    },
                    _ => return,
                }
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
            Act::Run(line) | Act::RunInTerminal(line) if line.trim().is_empty() => return,
            Act::Run(line) => {
                let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "sh".into());
                if let Some(argv) = plan_run(&line, &shell) {
                    self.ctx.shell.spawn(argv, cx);
                    self.remember_run(&line, cx);
                }
            }
            Act::RunInTerminal(line) => {
                let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "sh".into());
                if let Some(argv) = plan_run(&line, &shell) {
                    let mut full = self.terminal_prefix(cx);
                    full.extend(argv);
                    self.ctx.shell.spawn(full, cx);
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

    fn remember_run(&mut self, line: &str, cx: &mut Context<Self>) {
        let line = line.trim().to_string();
        let now = history::now();
        self.run_history.retain(|(c, _)| *c != line);
        self.run_history.insert(0, (line.clone(), now));
        cx.background_spawn(async move { history::append(&line, now) }).detach();
    }
}

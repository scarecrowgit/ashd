//! Running a typed command inside the launcher and showing what it prints.
//!
//! The command runs as `sh -lc <line>` in the home directory with the shell's own
//! environment, in its own process group, with stdin closed. Two reader threads move
//! stdout and stderr into one bounded channel (a command that prints faster than the
//! launcher draws is slowed down by the pipe, memory stays flat); the panel folds the chunks
//! into an [`OutputBuf`]. No GPUI here: the buffer and the command line are plain data.
//!
//! This is the one place the shell starts a process itself instead of asking niri
//! (ARCHITECTURE.md rule 6): the output has to come back through pipes the shell owns.

use std::{
    collections::VecDeque,
    io::Read,
    os::unix::process::{CommandExt as _, ExitStatusExt as _},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use async_channel::{Receiver, Sender};

/// What the buffer keeps: the newest lines, at most this many bytes.
pub const MAX_LINES: usize = 2000;
pub const MAX_BYTES: usize = 1 << 20;
/// A line that never ends is cut here and continues as the next line.
const MAX_LINE_BYTES: usize = 64 * 1024;
/// Chunks in flight between the reader threads and the panel.
const CHANNEL_DEPTH: usize = 64;
/// How long the exit report waits for the pipes to drain after the shell exited (a
/// background child can hold them open for ever).
const DRAIN_GRACE: Duration = Duration::from_millis(300);
/// SIGTERM → SIGKILL delay when the launcher closes under a running command.
const KILL_GRACE: Duration = Duration::from_millis(1500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Out,
    Err,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub stream: Stream,
    pub text: String,
}

/// The output of a command: complete lines in arrival order (stdout and stderr
/// interleaved as they came), the unfinished line of each stream, and a cap.
#[derive(Debug, Default)]
pub struct OutputBuf {
    lines: VecDeque<Line>,
    bytes: usize,
    /// Bytes of the line being read, per stream (`Out`, `Err`).
    pending: [Vec<u8>; 2],
    /// Lines dropped from the front to stay under the cap.
    pub dropped: usize,
}

fn slot(s: Stream) -> usize {
    match s {
        Stream::Out => 0,
        Stream::Err => 1,
    }
}

/// Terminal escape sequences removed (CSI, OSC, two-byte escapes), a carriage return
/// keeps only what follows it (progress bars redraw one line), tabs become spaces and other
/// control characters are dropped.
pub fn clean_line(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                // CSI: parameters, then a final byte 0x40..=0x7e
                Some('[') => {
                    for n in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or ESC \
                Some(']') => {
                    while let Some(n) = chars.next() {
                        if n == '\x07' {
                            break;
                        }
                        if n == '\x1b' {
                            chars.next_if_eq(&'\\');
                            break;
                        }
                    }
                }
                // charset selection etc.: ESC ( B
                Some('(' | ')' | '*' | '+') => {
                    chars.next();
                }
                _ => {}
            },
            '\r' => out.clear(),
            '\t' => out.push_str("    "),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

impl OutputBuf {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes read from `stream`. Lines are split at `\n` (a `\r\n` ends a line too);
    /// a line split across chunks is joined; invalid UTF-8 is replaced.
    pub fn push(&mut self, stream: Stream, chunk: &[u8]) {
        let mut rest = chunk;
        while !rest.is_empty() {
            let take = match rest.iter().position(|&b| b == b'\n') {
                Some(nl) => {
                    self.pending[slot(stream)].extend_from_slice(&rest[..nl]);
                    self.finish_line(stream);
                    nl + 1
                }
                None => {
                    let room = MAX_LINE_BYTES - self.pending[slot(stream)].len();
                    let n = rest.len().min(room);
                    self.pending[slot(stream)].extend_from_slice(&rest[..n]);
                    if self.pending[slot(stream)].len() >= MAX_LINE_BYTES {
                        self.finish_line(stream);
                    }
                    n
                }
            };
            rest = &rest[take..];
        }
    }

    /// The command ended: an unterminated last line of a stream becomes a line.
    pub fn flush(&mut self) {
        for s in [Stream::Out, Stream::Err] {
            if !self.pending[slot(s)].is_empty() {
                self.finish_line(s);
            }
        }
    }

    fn finish_line(&mut self, stream: Stream) {
        let mut raw = std::mem::take(&mut self.pending[slot(stream)]);
        if raw.last() == Some(&b'\r') {
            raw.pop();
        }
        let text = clean_line(&String::from_utf8_lossy(&raw));
        self.bytes += text.len() + 1;
        self.lines.push_back(Line { stream, text });
        while self.lines.len() > MAX_LINES || (self.bytes > MAX_BYTES && self.lines.len() > 1) {
            if let Some(old) = self.lines.pop_front() {
                self.bytes -= old.text.len() + 1;
                self.dropped += 1;
            }
        }
    }

    /// Complete lines kept.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.pending.iter().all(Vec::is_empty)
    }

    /// Lines `from..from + count` of the whole output, the unfinished line of each stream
    /// last (so a prompt without a newline is visible while the command waits).
    pub fn window(&self, from: usize, count: usize) -> Vec<Line> {
        let partial = |s: Stream| {
            let p = &self.pending[slot(s)];
            (!p.is_empty()).then(|| Line { stream: s, text: clean_line(&String::from_utf8_lossy(p)) })
        };
        self.lines
            .iter()
            .skip(from)
            .cloned()
            .chain(partial(Stream::Out))
            .chain(partial(Stream::Err))
            .take(count)
            .collect()
    }

    /// Lines to show, counting the unfinished ones.
    pub fn visible_len(&self) -> usize {
        self.lines.len() + self.pending.iter().filter(|p| !p.is_empty()).count()
    }

    /// Everything, for the clipboard (stdout and stderr in arrival order).
    pub fn text(&self) -> String {
        self.window(0, usize::MAX).iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n")
    }
}

/// `sh -lc <line>`: the login shell reads the profile, so PATH is the one of a terminal.
pub fn sh_argv(line: &str) -> Vec<String> {
    ["sh".to_string(), "-lc".into(), line.to_string()].into()
}

/// How a command ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    Signal(i32),
}

/// `exit 3`, `killed (signal 9)`.
pub fn exit_label(exit: Exit) -> String {
    match exit {
        Exit::Code(c) => format!("exit {c}"),
        Exit::Signal(s) => format!("killed (signal {s})"),
    }
}

/// `0.4 s`, `12 s`, `3 min 05 s`.
pub fn duration_label(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 10.0 {
        format!("{s:.1} s")
    } else if s < 60.0 {
        format!("{:.0} s", s)
    } else {
        format!("{} min {:02} s", d.as_secs() / 60, d.as_secs() % 60)
    }
}

pub enum Msg {
    Chunk(Stream, Vec<u8>),
    /// The command ended (or could not start).
    Done(Result<(Exit, Duration), String>),
}

/// A running command: kill it, and read its messages from [`Run::rx`].
pub struct Run {
    pub rx: Receiver<Msg>,
    pub handle: Handle,
}

#[derive(Clone)]
pub struct Handle {
    pgid: Arc<std::sync::atomic::AtomicI32>,
    finished: Arc<AtomicBool>,
}

impl Handle {
    /// SIGTERM the process group, SIGKILL it if it is still there a moment later. A no-op
    /// once the command has ended (the group id could be reused by then).
    pub fn kill(&self) {
        let pgid = self.pgid.load(Ordering::SeqCst);
        if pgid <= 0 || self.finished.load(Ordering::SeqCst) {
            return;
        }
        signal_group(pgid, rustix::process::Signal::TERM);
        let finished = self.finished.clone();
        std::thread::spawn(move || {
            std::thread::sleep(KILL_GRACE);
            if !finished.load(Ordering::SeqCst) {
                signal_group(pgid, rustix::process::Signal::KILL);
            }
        });
    }
}

fn signal_group(pgid: i32, sig: rustix::process::Signal) {
    if let Some(pid) = rustix::process::Pid::from_raw(pgid) {
        let _ = rustix::process::kill_process_group(pid, sig);
    }
}

/// Start `line` (threads, no blocking). The home directory is the working directory.
pub fn spawn(line: &str) -> Run {
    let (tx, rx) = async_channel::bounded(CHANNEL_DEPTH);
    let handle = Handle { pgid: Arc::default(), finished: Arc::default() };
    let (h, line) = (handle.clone(), line.to_string());
    std::thread::Builder::new()
        .name("ashd-launcher-run".into())
        .spawn(move || supervise(&line, &h, tx))
        .expect("spawn ashd-launcher-run");
    Run { rx, handle }
}

fn supervise(line: &str, handle: &Handle, tx: Sender<Msg>) {
    let start = Instant::now();
    let argv = sh_argv(line);
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(home) = dirs::home_dir() {
        cmd.current_dir(home);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            handle.finished.store(true, Ordering::SeqCst);
            let _ = tx.send_blocking(Msg::Done(Err(format!("cannot run sh: {e}"))));
            return;
        }
    };
    handle.pgid.store(child.id() as i32, Ordering::SeqCst);

    let (eof_tx, eof_rx) = async_channel::unbounded::<()>();
    let mut readers = 0;
    let pipes: [(Stream, Option<Box<dyn Read + Send>>); 2] = [
        (Stream::Out, child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>)),
        (Stream::Err, child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>)),
    ];
    for (stream, pipe) in pipes {
        let Some(mut pipe) = pipe else { continue };
        readers += 1;
        let (tx, eof_tx) = (tx.clone(), eof_tx.clone());
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    // the panel is gone (closed / replaced): stop, the pipe closes
                    Ok(n) => {
                        if tx.send_blocking(Msg::Chunk(stream, buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = eof_tx.send_blocking(());
        });
    }

    let status = child.wait();
    handle.finished.store(true, Ordering::SeqCst);
    // let the readers drain what is already in the pipes
    let deadline = Instant::now() + DRAIN_GRACE;
    for _ in 0..readers {
        let left = deadline.saturating_duration_since(Instant::now());
        if recv_timeout(&eof_rx, left).is_none() {
            break;
        }
    }
    let result = status
        .map_err(|e| format!("wait failed: {e}"))
        .map(|st| (st.code().map(Exit::Code).or_else(|| st.signal().map(Exit::Signal)).unwrap_or(Exit::Code(-1)), start.elapsed()));
    let _ = tx.send_blocking(Msg::Done(result));
}

/// Wait for one message of `rx` for at most `wait`, on a plain thread.
fn recv_timeout(rx: &Receiver<()>, wait: Duration) -> Option<()> {
    let step = Duration::from_millis(5);
    let start = Instant::now();
    loop {
        match rx.try_recv() {
            Ok(()) => return Some(()),
            Err(async_channel::TryRecvError::Closed) => return Some(()),
            Err(async_channel::TryRecvError::Empty) => {}
        }
        if start.elapsed() >= wait {
            return None;
        }
        std::thread::sleep(step);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(b: &OutputBuf) -> Vec<String> {
        b.window(0, usize::MAX).into_iter().map(|l| l.text).collect()
    }

    #[test]
    fn lines_split_across_chunks() {
        let mut b = OutputBuf::new();
        b.push(Stream::Out, b"hel");
        assert_eq!(b.len(), 0);
        assert_eq!(texts(&b), ["hel"], "the unfinished line is shown while it grows");
        b.push(Stream::Out, b"lo\nwor");
        b.push(Stream::Out, b"ld\n\nlast");
        assert_eq!(texts(&b), ["hello", "world", "", "last"]);
        b.flush();
        assert_eq!((b.len(), texts(&b).last().map(String::as_str)), (4, Some("last")));
        // CRLF and a multi-byte character cut in the middle
        let mut b = OutputBuf::new();
        let bytes = "ü\r\nβ".as_bytes();
        b.push(Stream::Out, &bytes[..1]);
        b.push(Stream::Out, &bytes[1..]);
        b.flush();
        assert_eq!(texts(&b), ["ü", "β"]);
    }

    #[test]
    fn streams_interleave_in_arrival_order() {
        let mut b = OutputBuf::new();
        b.push(Stream::Out, b"one\n");
        b.push(Stream::Err, b"oops ");
        b.push(Stream::Out, b"two\n");
        b.push(Stream::Err, b"now\n");
        let lines = b.window(0, 10);
        assert_eq!(
            lines.iter().map(|l| (l.stream, l.text.as_str())).collect::<Vec<_>>(),
            [(Stream::Out, "one"), (Stream::Out, "two"), (Stream::Err, "oops now")],
            "a stream's partial line completes on its own clock"
        );
        // windows
        assert_eq!(b.window(1, 1).len(), 1);
        assert_eq!(b.window(1, 1)[0].text, "two");
        assert_eq!(b.text(), "one\ntwo\noops now");
    }

    #[test]
    fn the_buffer_is_capped_by_lines_and_bytes() {
        let mut b = OutputBuf::new();
        for i in 0..(MAX_LINES + 50) {
            b.push(Stream::Out, format!("line {i}\n").as_bytes());
        }
        assert_eq!(b.len(), MAX_LINES);
        assert_eq!(b.dropped, 50);
        assert_eq!(texts(&b)[0], "line 50");
        // bytes: 40 lines of 100 KB keep about 1 MB
        let mut b = OutputBuf::new();
        let big = format!("{}\n", "x".repeat(100_000));
        for _ in 0..40 {
            b.push(Stream::Out, big.as_bytes());
        }
        assert!(b.len() <= 11 && b.len() >= 9, "{}", b.len());
        assert!(b.bytes <= MAX_BYTES);
        // a line without a newline cannot grow without bound either
        let mut b = OutputBuf::new();
        b.push(Stream::Out, &vec![b'a'; MAX_LINE_BYTES * 3 + 5]);
        assert_eq!(b.len(), 3);
        assert_eq!(b.pending[0].len(), 5);
    }

    #[test]
    fn escapes_cr_and_tabs_are_cleaned() {
        assert_eq!(clean_line("\x1b[31mred\x1b[0m plain"), "red plain");
        assert_eq!(clean_line("\x1b]0;title\x07after"), "after");
        assert_eq!(clean_line("\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\"), "link");
        assert_eq!(clean_line("10%\r50%\r100%"), "100%");
        assert_eq!(clean_line("a\tb"), "a    b");
        assert_eq!(clean_line("bell\x07!"), "bell!");
        assert_eq!(clean_line("\x1b(Bok"), "ok");
        assert_eq!(clean_line("ünï"), "ünï");
    }

    #[test]
    fn the_command_line_goes_to_sh_verbatim() {
        let nasty = "echo 'a b' \"c\" $HOME; ls\nfalse";
        assert_eq!(sh_argv(nasty), ["sh", "-lc", nasty]);
    }

    #[test]
    fn labels() {
        assert_eq!(exit_label(Exit::Code(3)), "exit 3");
        assert_eq!(exit_label(Exit::Signal(9)), "killed (signal 9)");
        assert_eq!(duration_label(Duration::from_millis(420)), "0.4 s");
        assert_eq!(duration_label(Duration::from_secs(12)), "12 s");
        assert_eq!(duration_label(Duration::from_secs(185)), "3 min 05 s");
    }

    /// A real command: both pipes, the exit code, the cwd, a killed group.
    #[test]
    fn runs_a_command_and_reports_the_exit() {
        let run = spawn("echo out; echo err >&2; exit 3");
        let mut b = OutputBuf::new();
        let done = loop {
            match run.rx.recv_blocking().expect("the supervisor reports") {
                Msg::Chunk(s, bytes) => b.push(s, &bytes),
                Msg::Done(r) => break r,
            }
        };
        b.flush();
        let (exit, _) = done.unwrap();
        assert_eq!(exit, Exit::Code(3));
        let lines = b.window(0, 10);
        assert!(lines.contains(&Line { stream: Stream::Out, text: "out".into() }), "{lines:?}");
        assert!(lines.contains(&Line { stream: Stream::Err, text: "err".into() }), "{lines:?}");
    }

    #[test]
    fn kill_ends_the_whole_group() {
        let run = spawn("sleep 30 & sleep 30; echo unreachable");
        std::thread::sleep(Duration::from_millis(300));
        run.handle.kill();
        let exit = loop {
            match run.rx.recv_blocking().expect("done") {
                Msg::Chunk(..) => {}
                Msg::Done(r) => break r.unwrap().0,
            }
        };
        assert!(matches!(exit, Exit::Signal(_) | Exit::Code(_)));
        assert_ne!(exit, Exit::Code(0));
    }
}

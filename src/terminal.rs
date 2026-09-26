use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};

use crate::approval::{self, View};
use crate::clipboard::{Capture, Clipboard};

const QUEUE_LIMIT: usize = 64 * 1024;
// End pending controls, links, and image placements before drawing host prompts.
const CLEAR: &[u8] = concat!(
    "\x18\x1b\\\x1b]8;;\x1b\\\x1b_Ga=d,d=a;\x1b\\",
    "\x1b[?2026l\x1b[0m\x1b[r\x1b[?6l\x1b[?69l\x1b[?7h",
    "\x0f\x1b(B\x1b[?25h\x1b[2J\x1b[H"
)
.as_bytes();
const ENTER: &[u8] = b"\x1b[>0u\x1b[?2004s\x1b[?1000s\x1b[?1002s\x1b[?1003s\x1b[?1004s\x1b[?1006s\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1004l\x1b[?1006l";
const LEAVE: &[u8] = b"\x1b[<u\x1b[?2004r\x1b[?1000r\x1b[?1002r\x1b[?1003r\x1b[?1004r\x1b[?1006r";
static INTERRUPTED: AtomicI32 = AtomicI32::new(0);

pub fn available() -> bool {
    if !(io::stdin().is_terminal() && io::stdout().is_terminal() && io::stderr().is_terminal()) {
        return false;
    }
    unsafe {
        let session = libc::tcgetsid(0);
        session >= 0
            && session == libc::getsid(0)
            && libc::tcgetsid(1) == session
            && libc::tcgetsid(2) == session
            && libc::tcgetpgrp(0) == libc::getpgrp()
    }
}

struct Terminal {
    file: File,
    saved: libc::termios,
    in_view: bool,
    diagnostics: Option<Diagnostics>,
}

impl Terminal {
    fn open() -> Result<Self> {
        ensure!(
            available(),
            "the approval view requires a host terminal on stdin, stdout, and stderr"
        );
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
            .open("/dev/tty")?;
        let session = check(unsafe { libc::tcgetsid(file.as_raw_fd()) })?;
        for descriptor in [0, 1, 2] {
            ensure!(
                unsafe { libc::tcgetsid(descriptor) } == session,
                "stdio must use the controlling terminal"
            );
        }
        ensure!(
            unsafe { libc::tcgetpgrp(file.as_raw_fd()) } == unsafe { libc::getpgrp() },
            "the approval view must run in the foreground"
        );
        let mut saved = unsafe { std::mem::zeroed() };
        check(unsafe { libc::tcgetattr(file.as_raw_fd(), &mut saved) })?;
        Ok(Self {
            file,
            saved,
            in_view: false,
            diagnostics: None,
        })
    }

    fn control(&mut self, mut bytes: &[u8]) -> Result<()> {
        let deadline = Instant::now() + Duration::from_millis(250);
        while !bytes.is_empty() {
            ensure!(
                Instant::now() < deadline,
                "host terminal control output timed out"
            );
            match self.file.write(bytes) {
                Ok(0) => anyhow::bail!("host terminal closed"),
                Ok(n) => bytes = &bytes[n..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    let mut ready = libc::pollfd {
                        fd: self.file.as_raw_fd(),
                        events: libc::POLLOUT,
                        revents: 0,
                    };
                    let _ = poll(std::slice::from_mut(&mut ready), 20);
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn enter(&mut self) -> Result<()> {
        self.control(CLEAR)?;
        self.control(ENTER)?;
        self.in_view = true;
        Ok(())
    }

    fn leave(&mut self) -> Result<()> {
        if self.in_view {
            self.control(CLEAR)?;
            self.control(LEAVE)?;
            self.in_view = false;
        }
        Ok(())
    }

    fn raw(&self) -> Result<()> {
        let mut raw = self.saved;
        unsafe {
            libc::cfmakeraw(&mut raw);
        }
        check(unsafe { libc::tcsetattr(self.file.as_raw_fd(), libc::TCSAFLUSH, &raw) })?;
        Ok(())
    }

    fn flush_input(&self) -> Result<()> {
        check(unsafe { libc::tcflush(self.file.as_raw_fd(), libc::TCIFLUSH) })?;
        Ok(())
    }

    fn size(&self) -> Result<libc::winsize> {
        let mut size = unsafe { std::mem::zeroed::<libc::winsize>() };
        check(unsafe { libc::ioctl(self.file.as_raw_fd(), libc::TIOCGWINSZ, &mut size) })
            .context("failed to read host terminal dimensions")?;
        if size.ws_row == 0 {
            size.ws_row = 24;
        }
        if size.ws_col == 0 {
            size.ws_col = 80;
        }
        Ok(size)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.leave();
        let _ = self.file.write_all(b"\x18\x1b\\\x1b[0m\x1b[?25h");
        unsafe {
            libc::tcsetattr(self.file.as_raw_fd(), libc::TCSAFLUSH, &self.saved);
        }
        self.diagnostics.take();
    }
}

struct Diagnostics {
    saved: File,
    reader: io::PipeReader,
    tail: VecDeque<u8>,
}
impl Diagnostics {
    fn capture() -> Result<Self> {
        let saved = check(unsafe { libc::fcntl(2, libc::F_DUPFD_CLOEXEC, 3) })?;
        let saved = unsafe { File::from_raw_fd(saved) };
        let (reader, writer) = io::pipe()?;
        check(unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) })?;
        check(unsafe { libc::dup2(writer.as_raw_fd(), 2) })?;
        Ok(Self {
            saved,
            reader,
            tail: VecDeque::new(),
        })
    }

    fn drain(&mut self) -> Result<()> {
        let mut bytes = [0u8; 4096];
        match self.reader.read(&mut bytes) {
            Ok(count) => {
                self.tail.extend(&bytes[..count]);
                if self.tail.len() > 16 * 1024 {
                    self.tail.drain(..self.tail.len() - 16 * 1024);
                }
            }
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }
}
impl Drop for Diagnostics {
    fn drop(&mut self) {
        unsafe {
            libc::dup2(self.saved.as_raw_fd(), 2);
        }
        for _ in 0..16 {
            let _ = self.drain();
        }
        if !self.tail.is_empty() {
            let bytes: Vec<_> = self.tail.drain(..).collect();
            let mut output = io::stderr().lock();
            let _ = writeln!(
                output,
                "slopbox: recent broker diagnostics (captured while the terminal was active):"
            );
            for line in String::from_utf8_lossy(&bytes).lines() {
                let _ = writeln!(output, "{}", crate::launch::terminal_text(line));
            }
        }
    }
}

struct Signals(Vec<(libc::c_int, libc::sigaction)>);
impl Signals {
    fn install() -> Result<Self> {
        INTERRUPTED.store(0, Ordering::Relaxed);
        let mut guard = Self(Vec::new());
        for signal in [
            libc::SIGINT,
            libc::SIGTERM,
            libc::SIGHUP,
            libc::SIGQUIT,
            libc::SIGTSTP,
        ] {
            let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
            action.sa_sigaction = interrupted as *const () as usize;
            unsafe {
                libc::sigemptyset(&mut action.sa_mask);
            }
            let mut old = unsafe { std::mem::zeroed() };
            check(unsafe { libc::sigaction(signal, &action, &mut old) })?;
            guard.0.push((signal, old));
        }
        Ok(guard)
    }
}
impl Drop for Signals {
    fn drop(&mut self) {
        for (signal, action) in &self.0 {
            unsafe {
                libc::sigaction(*signal, action, std::ptr::null_mut());
            }
        }
    }
}
extern "C" fn interrupted(signal: libc::c_int) {
    INTERRUPTED.store(signal, Ordering::Relaxed);
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn open_pty(termios: &libc::termios, size: &libc::winsize) -> Result<(File, File)> {
    #[cfg(target_os = "linux")]
    let master = {
        let descriptor = check(unsafe {
            libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC | libc::O_NONBLOCK)
        })?;
        unsafe { File::from_raw_fd(descriptor) }
    };
    #[cfg(target_os = "macos")]
    let master = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
        .open("/dev/ptmx")?;
    check(unsafe { libc::grantpt(master.as_raw_fd()) })?;
    check(unsafe { libc::unlockpt(master.as_raw_fd()) })?;
    #[cfg(target_os = "linux")]
    let slave = {
        let descriptor = check(unsafe {
            libc::ioctl(
                master.as_raw_fd(),
                libc::TIOCGPTPEER,
                libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
            )
        })?;
        unsafe { File::from_raw_fd(descriptor) }
    };
    #[cfg(target_os = "macos")]
    let slave = {
        use std::os::unix::ffi::OsStrExt;
        let mut name = [0_u8; 128];
        check(unsafe {
            libc::ioctl(
                master.as_raw_fd(),
                libc::TIOCPTYGNAME as _,
                name.as_mut_ptr(),
            )
        })?;
        let name =
            std::ffi::CStr::from_bytes_until_nul(&name).context("invalid native PTY name")?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(std::ffi::OsStr::from_bytes(name.to_bytes()))?
    };
    check(unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, termios) })?;
    resize(&master, size)?;
    Ok((master, slave))
}

fn poll(descriptors: &mut [libc::pollfd], timeout_ms: i32) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    let result =
        unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, timeout_ms) };
    #[cfg(target_os = "macos")]
    let result = {
        // Darwin poll(2) does not support terminal devices; select(2) does.
        let mut read = unsafe { std::mem::zeroed() };
        let mut write = unsafe { std::mem::zeroed() };
        let mut error = unsafe { std::mem::zeroed() };
        let mut highest = -1;
        for descriptor in descriptors.iter_mut() {
            descriptor.revents = 0;
            if descriptor.fd < 0 {
                continue;
            }
            if descriptor.fd as usize >= libc::FD_SETSIZE {
                return Err(io::Error::other("terminal descriptor exceeds select limit"));
            }
            highest = highest.max(descriptor.fd);
            unsafe {
                if descriptor.events & libc::POLLIN != 0 {
                    libc::FD_SET(descriptor.fd, &mut read);
                }
                if descriptor.events & libc::POLLOUT != 0 {
                    libc::FD_SET(descriptor.fd, &mut write);
                }
                libc::FD_SET(descriptor.fd, &mut error);
            }
        }
        let mut timeout = libc::timeval {
            tv_sec: (timeout_ms / 1000) as _,
            tv_usec: ((timeout_ms % 1000) * 1000) as _,
        };
        let result =
            unsafe { libc::select(highest + 1, &mut read, &mut write, &mut error, &mut timeout) };
        if result >= 0 {
            for descriptor in descriptors
                .iter_mut()
                .filter(|descriptor| descriptor.fd >= 0)
            {
                unsafe {
                    if libc::FD_ISSET(descriptor.fd, &read) {
                        descriptor.revents |= libc::POLLIN;
                    }
                    if libc::FD_ISSET(descriptor.fd, &write) {
                        descriptor.revents |= libc::POLLOUT;
                    }
                    if libc::FD_ISSET(descriptor.fd, &error) {
                        descriptor.revents |= libc::POLLERR;
                    }
                }
            }
        }
        result
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn resize(master: &File, size: &libc::winsize) -> Result<()> {
    check(unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, size) })
        .context("failed to resize the sandbox terminal")?;
    Ok(())
}

pub fn run(
    command: &mut Command,
    approval_view: bool,
    mut clipboard: Option<&mut Clipboard>,
    context: approval::Context<'_>,
) -> Result<ExitStatus> {
    let _signals = Signals::install()?;
    let mut terminal = Terminal::open()?;
    // Broker diagnostics must not corrupt Pi's display or the host approval view.
    terminal.diagnostics = Some(Diagnostics::capture()?);
    let mut size = terminal.size()?;
    let (mut master, slave) = open_pty(&terminal.saved, &size)?;
    command
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    unsafe {
        command.pre_exec(|| {
            check(libc::setsid())?;
            check(libc::ioctl(0, libc::TIOCSCTTY as _, 0))?;
            Ok(())
        });
    }
    let mut process = Process(
        command
            .spawn()
            .context("failed to start sandbox terminal")?,
    );
    // Command retains its Stdio handles; release the slave ends before waiting for EOF.
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    terminal.raw()?;
    let mut view: Option<View> = None;
    let mut line = Vec::new();
    let mut to_host = VecDeque::new();
    if approval_view {
        to_host.extend(b"slopbox: Ctrl-] opens host network approvals; ] then Enter in the view sends a literal Ctrl-]\r\n");
    }
    let mut to_guest = VecDeque::new();
    let mut hotkey = Hotkey {
        approvals: approval_view,
        clipboard: clipboard.is_some(),
        ..Hotkey::default()
    };
    let mut exited = None;
    let mut eof = false;
    let mut redraw_at = None;
    let mut capture: Option<(Capture, usize)> = None;
    let mut clipboard_notice: Option<String> = None;
    loop {
        let signal = INTERRUPTED.swap(0, Ordering::Relaxed);
        if signal == libc::SIGTSTP {
            capture = None;
            to_guest.clear();
            suspend(&process, &terminal)?;
            resize(&master, &size)?;
            if let Some(view) = &view {
                draw(&mut to_host, view, &context, &size, &line);
            }
            if let Some(message) = &clipboard_notice {
                draw_clipboard_notice(&mut to_host, message);
            }
            continue;
        }
        if signal != 0 {
            return Ok(ExitStatus::from_raw(signal));
        }
        if exited.is_none()
            && let Some(status) = process.0.try_wait()?
        {
            exited = Some((status, Instant::now()));
            capture = None;
            to_guest.clear();
            if view.take().is_some() || clipboard_notice.take().is_some() {
                to_host.clear();
                terminal.leave()?;
            }
        }
        if let Some((status, when)) = exited
            && ((eof && to_host.is_empty()) || when.elapsed() > Duration::from_secs(1))
        {
            return Ok(status);
        }
        let current = terminal.size()?;
        if current.ws_row != size.ws_row
            || current.ws_col != size.ws_col
            || current.ws_xpixel != size.ws_xpixel
            || current.ws_ypixel != size.ws_ypixel
        {
            size = current;
            resize(&master, &size)?;
            if let Some(view) = &view {
                draw(&mut to_host, view, &context, &size, &line);
            }
            if let Some(message) = &clipboard_notice {
                draw_clipboard_notice(&mut to_host, message);
            }
        }
        if redraw_at.is_some_and(|at| Instant::now() >= at) {
            resize(&master, &size)?;
            redraw_at = None;
        }
        if view.is_none()
            && clipboard_notice.is_none()
            && hotkey.expire(&mut to_guest)
            && let Some((_, offset)) = capture.take()
        {
            to_guest.truncate(offset);
            to_guest.push_back(0x1b);
        }
        if let Some((job, offset)) = &mut capture {
            match clipboard.as_mut().expect("clipboard enabled").poll(job) {
                Ok(Some(path)) => {
                    let later_input = to_guest.split_off(*offset);
                    to_guest.extend(format!("\x1b[200~ {path} \x1b[201~").bytes());
                    to_guest.extend(later_input);
                    capture = None;
                }
                Ok(None) => {}
                Err(error) => {
                    capture = None;
                    clipboard_notice = Some(format!("{error:#}"));
                }
            }
        }
        if let Some(message) = &clipboard_notice
            && !terminal.in_view
        {
            terminal.flush_input()?;
            to_guest.clear();
            terminal.enter()?;
            draw_clipboard_notice(&mut to_host, message);
        }
        let in_host_view = view.is_some() || clipboard_notice.is_some();
        let mut poll = [
            libc::pollfd {
                fd: terminal.file.as_raw_fd(),
                events: (if exited.is_none()
                    && to_guest.len() < QUEUE_LIMIT
                    && (!in_host_view || to_host.is_empty())
                {
                    libc::POLLIN
                } else {
                    0
                }) | if to_host.is_empty() { 0 } else { libc::POLLOUT },
                revents: 0,
            },
            libc::pollfd {
                fd: if (in_host_view || eof) && to_guest.is_empty() {
                    -1
                } else {
                    master.as_raw_fd()
                },
                events: (if !in_host_view && !eof && to_host.len() < QUEUE_LIMIT {
                    libc::POLLIN
                } else {
                    0
                }) | if to_guest.is_empty() || capture.is_some() {
                    0
                } else {
                    libc::POLLOUT
                },
                revents: 0,
            },
            libc::pollfd {
                fd: terminal
                    .diagnostics
                    .as_ref()
                    .expect("diagnostics installed")
                    .reader
                    .as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: capture.as_ref().map_or(-1, |(job, _)| job.descriptor()),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        if let Err(error) = self::poll(&mut poll, 50) {
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if poll[2].revents & libc::POLLIN != 0 {
            terminal
                .diagnostics
                .as_mut()
                .expect("diagnostics installed")
                .drain()?;
        }
        if poll[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            anyhow::bail!("host terminal disconnected");
        }
        if poll[0].revents & libc::POLLIN != 0 {
            let mut bytes = [0u8; 4096];
            let count = match terminal.file.read(&mut bytes) {
                Ok(0) => anyhow::bail!("host terminal closed"),
                Ok(n) => n,
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::Interrupted =>
                {
                    0
                }
                Err(e) => return Err(e.into()),
            };
            if clipboard_notice.is_some() {
                if bytes[..count]
                    .iter()
                    .any(|byte| matches!(byte, b'\r' | b'\n' | 0x1b))
                {
                    clipboard_notice = None;
                    to_host.clear();
                    terminal.flush_input()?;
                    terminal.leave()?;
                    let mut narrow = size;
                    narrow.ws_col = size.ws_col.saturating_sub(1).max(1);
                    resize(&master, &narrow)?;
                    redraw_at = Some(Instant::now() + Duration::from_millis(200));
                }
                continue;
            }
            if let Some(panel) = &mut view {
                let mut transition = false;
                for byte in &bytes[..count] {
                    match byte {
                        b'\r' | b'\n' => {
                            let input = std::str::from_utf8(&line)?;
                            let close = if input == "]" || input == "q" {
                                true
                            } else if size.ws_row < 24 || size.ws_col < 80 {
                                panel.cancel_input();
                                false
                            } else {
                                panel.submit(&context, input, size.ws_row as usize)
                            };
                            if input == "]" {
                                to_guest.push_back(0x1d);
                            }
                            line.clear();
                            if close {
                                view = None;
                                to_host.clear();
                                terminal.leave()?;
                                let mut narrow = size;
                                narrow.ws_col = size.ws_col.saturating_sub(1).max(1);
                                resize(&master, &narrow)?;
                                redraw_at = Some(Instant::now() + Duration::from_millis(200));
                            }
                            transition = true;
                            break;
                        }
                        0x7f | 0x08 => {
                            line.pop();
                        }
                        0x20..=0x7e if line.len() < 64 => line.push(*byte),
                        _ => {
                            panel.cancel_input();
                            line.clear();
                            transition = true;
                            break;
                        }
                    }
                }
                if transition {
                    terminal.flush_input()?;
                }
                if let Some(panel) = &view {
                    draw(&mut to_host, panel, &context, &size, &line);
                }
            } else {
                for byte in &bytes[..count] {
                    let Some(shortcut) = hotkey.feed(*byte, &mut to_guest) else {
                        continue;
                    };
                    match shortcut {
                        Shortcut::Clipboard => {
                            if capture.is_none() {
                                match clipboard.as_ref().expect("clipboard enabled").start() {
                                    Ok(job) => capture = Some((job, to_guest.len())),
                                    Err(error) => {
                                        clipboard_notice = Some(format!("{error:#}"));
                                        to_host.clear();
                                        break;
                                    }
                                }
                            }
                            continue;
                        }
                        Shortcut::Cancel => {
                            if let Some((_, offset)) = capture.take() {
                                to_guest.truncate(offset);
                            }
                            to_guest.push_back(0x03);
                            continue;
                        }
                        Shortcut::Suspend => {
                            capture = None;
                            to_guest.clear();
                            INTERRUPTED.store(libc::SIGTSTP, Ordering::Relaxed);
                            break;
                        }
                        Shortcut::Approvals => {}
                    }
                    {
                        capture = None;
                        terminal.flush_input()?;
                        to_guest.clear();
                        line.clear();
                        let panel = View::open(&context).unwrap_or_else(View::failed);
                        to_host.clear();
                        terminal.enter()?;
                        for row in 1..=size.ws_row.min(64) {
                            to_host.extend(format!("\x1b[{row};1H\x1b#5").bytes());
                        }
                        to_host.extend(b"\x1b[H");
                        to_host.extend(
                            panel
                                .render(&context, size.ws_row as usize, size.ws_col as usize)
                                .bytes(),
                        );
                        view = Some(panel);
                        break;
                    }
                }
            }
        }
        if capture.is_none() && clipboard_notice.is_none() && poll[1].revents & libc::POLLOUT != 0 {
            flush(&mut master, &mut to_guest)
                .context("failed to send input to the sandbox terminal")?;
        }
        if view.is_none()
            && clipboard_notice.is_none()
            && !eof
            && poll[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0
        {
            let mut bytes = [0u8; 4096];
            match master.read(&mut bytes) {
                Ok(0) => eof = true,
                Ok(count) => to_host.extend(&bytes[..count]),
                Err(e) if e.raw_os_error() == Some(libc::EIO) => eof = true,
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
        if poll[0].revents & libc::POLLOUT != 0 {
            flush(&mut terminal.file, &mut to_host)
                .context("failed to write output to the host terminal")?;
        }
    }
}

fn suspend(process: &Process, terminal: &Terminal) -> Result<()> {
    check(unsafe { libc::kill(-(process.0.id() as i32), libc::SIGSTOP) })?;
    let restored = check(unsafe {
        libc::tcsetattr(terminal.file.as_raw_fd(), libc::TCSAFLUSH, &terminal.saved)
    });
    if restored.is_ok() {
        unsafe {
            libc::raise(libc::SIGSTOP);
        }
    }
    let raw = terminal.raw();
    unsafe {
        libc::kill(-(process.0.id() as i32), libc::SIGCONT);
    }
    restored?;
    raw
}

fn draw(
    queue: &mut VecDeque<u8>,
    view: &View,
    context: &approval::Context<'_>,
    size: &libc::winsize,
    line: &[u8],
) {
    queue.clear();
    queue.extend(CLEAR);
    queue.extend(
        view.render(context, size.ws_row as usize, size.ws_col as usize)
            .bytes(),
    );
    queue.extend(line);
}

fn draw_clipboard_notice(queue: &mut VecDeque<u8>, message: &str) {
    queue.clear();
    queue.extend(CLEAR);
    queue.extend(
        format!(
            "SLOPBOX CLIPBOARD\r\n\r\n{}\r\n\r\nPress Enter or Escape to return to Pi.\r\n",
            crate::launch::terminal_text(message)
        )
        .bytes(),
    );
}

fn flush(file: &mut File, queue: &mut VecDeque<u8>) -> Result<()> {
    if !queue.is_empty() {
        match file.write(queue.as_slices().0) {
            Ok(0) => anyhow::bail!("terminal write returned zero"),
            Ok(count) => {
                queue.drain(..count);
            }
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn check(result: libc::c_int) -> io::Result<libc::c_int> {
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Shortcut {
    Approvals,
    Suspend,
    Clipboard,
    Cancel,
}

#[derive(Default)]
struct Hotkey {
    approvals: bool,
    clipboard: bool,
    paste: bool,
    paste_end: usize,
    control_string: bool,
    string_bell: bool,
    string_escape: bool,
    pending: Vec<u8>,
    last_byte: Option<Instant>,
}
impl Hotkey {
    fn expire(&mut self, output: &mut VecDeque<u8>) -> bool {
        if self
            .last_byte
            .is_some_and(|when| when.elapsed() >= Duration::from_millis(100))
        {
            let escape = self.pending == b"\x1b";
            output.extend(self.pending.drain(..));
            self.last_byte = None;
            return escape;
        }
        false
    }

    fn feed(&mut self, byte: u8, output: &mut VecDeque<u8>) -> Option<Shortcut> {
        const KEYS: &[(&[u8], Shortcut)] = &[
            (b"\x1d", Shortcut::Approvals),
            (b"\x1b[93;5u", Shortcut::Approvals),
            (b"\x1b[93;5:1u", Shortcut::Approvals),
            (b"\x1b[27;5;93~", Shortcut::Approvals),
            (b"\x1a", Shortcut::Suspend),
            (b"\x1b[122;5u", Shortcut::Suspend),
            (b"\x1b[122;5:1u", Shortcut::Suspend),
            (b"\x1b[27;5;122~", Shortcut::Suspend),
            (b"\x16", Shortcut::Clipboard),
            (b"\x1b[118;5u", Shortcut::Clipboard),
            (b"\x1b[118;5:1u", Shortcut::Clipboard),
            (b"\x1b[27;5;118~", Shortcut::Clipboard),
            (b"\x03", Shortcut::Cancel),
            (b"\x1b[99;5u", Shortcut::Cancel),
            (b"\x1b[99;5:1u", Shortcut::Cancel),
            (b"\x1b[27;5;99~", Shortcut::Cancel),
        ];
        const LITERALS: &[&[u8]] = &[b"\x1b[200~", b"\x1b]", b"\x1bP", b"\x1b_", b"\x1b^"];
        if self.paste {
            const END: &[u8] = b"\x1b[201~";
            output.push_back(byte);
            if byte == END[self.paste_end] {
                self.paste_end += 1;
            } else {
                self.paste_end = usize::from(byte == END[0]);
            }
            if self.paste_end == END.len() {
                self.paste = false;
                self.paste_end = 0;
            }
            return None;
        }
        if self.control_string {
            output.push_back(byte);
            if (self.string_bell && byte == 7) || (self.string_escape && byte == b'\\') {
                self.control_string = false;
            }
            self.string_escape = byte == 0x1b;
            return None;
        }
        self.last_byte = Some(Instant::now());
        self.pending.push(byte);
        while !self.pending.is_empty() {
            if LITERALS.contains(&self.pending.as_slice()) {
                self.paste = self.pending == b"\x1b[200~";
                self.control_string = !self.paste;
                self.string_bell = self.pending == b"\x1b]";
                self.string_escape = false;
                output.extend(self.pending.drain(..));
                return None;
            }
            if let Some((_, shortcut)) =
                KEYS.iter().find(|(key, _)| *key == self.pending.as_slice())
            {
                if (*shortcut == Shortcut::Approvals && !self.approvals)
                    || (matches!(shortcut, Shortcut::Clipboard | Shortcut::Cancel)
                        && !self.clipboard)
                {
                    output.extend(self.pending.drain(..));
                    return None;
                }
                self.pending.clear();
                return Some(*shortcut);
            }
            if KEYS.iter().any(|(key, _)| key.starts_with(&self.pending))
                || LITERALS
                    .iter()
                    .any(|prefix| prefix.starts_with(&self.pending))
            {
                break;
            }
            output.push_back(self.pending.remove(0));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escape_detection_handles_fragmented_keyboard_protocols_without_eating_other_keys() {
        for key in [
            b"\x1d".as_slice(),
            b"\x1b[93;5u",
            b"\x1b[93;5:1u",
            b"\x1b[27;5;93~",
        ] {
            let mut filter = Hotkey {
                approvals: true,
                ..Hotkey::default()
            };
            let mut output = VecDeque::new();
            for byte in b"hello\x1b[A" {
                assert!(filter.feed(*byte, &mut output).is_none());
            }
            for byte in &key[..key.len() - 1] {
                assert!(filter.feed(*byte, &mut output).is_none());
            }
            assert_eq!(
                filter.feed(*key.last().unwrap(), &mut output),
                Some(Shortcut::Approvals)
            );
            assert_eq!(output.into_iter().collect::<Vec<_>>(), b"hello\x1b[A");
        }
    }
    #[test]
    fn standalone_escape_is_forwarded_and_suspend_is_host_owned() {
        let mut filter = Hotkey::default();
        let mut output = VecDeque::new();
        assert!(filter.feed(0x1b, &mut output).is_none());
        assert!(output.is_empty());
        filter.last_byte = Some(Instant::now() - Duration::from_secs(1));
        filter.expire(&mut output);
        assert_eq!(output.pop_front(), Some(0x1b));
        assert_eq!(filter.feed(0x1a, &mut output), Some(Shortcut::Suspend));
        assert!(output.is_empty());
        assert_eq!(filter.feed(0x1d, &mut output), None);
        assert_eq!(output.pop_front(), Some(0x1d));
    }

    #[test]
    fn clipboard_requires_an_enabled_host_key_not_pasted_text_or_terminal_replies() {
        for key in [
            b"\x16".as_slice(),
            b"\x1b[118;5u",
            b"\x1b[118;5:1u",
            b"\x1b[27;5;118~",
        ] {
            let mut filter = Hotkey {
                clipboard: true,
                ..Hotkey::default()
            };
            let mut output = VecDeque::new();
            for byte in &key[..key.len() - 1] {
                assert_eq!(filter.feed(*byte, &mut output), None);
            }
            assert_eq!(
                filter.feed(*key.last().unwrap(), &mut output),
                Some(Shortcut::Clipboard)
            );
            assert!(output.is_empty());
            let mut disabled = Hotkey::default();
            for byte in key {
                assert_eq!(disabled.feed(*byte, &mut output), None);
            }
            assert_eq!(output.into_iter().collect::<Vec<_>>(), key);
        }
        for literal in [
            b"\x1b[200~text\x16\x1b[118;5u\x1d\x1a\x03\x1b[201~".as_slice(),
            b"\x1b]l\x16\x1b[118;5u\x07",
            b"\x1bP\x16\x1b[118;5u\x07\x16\x1b\\",
            b"\x1b_\x16\x1b\\",
        ] {
            let mut filter = Hotkey {
                clipboard: true,
                approvals: true,
                ..Hotkey::default()
            };
            let mut output = VecDeque::new();
            for byte in literal {
                assert_eq!(filter.feed(*byte, &mut output), None);
            }
            assert_eq!(output.into_iter().collect::<Vec<_>>(), literal);
            assert_eq!(
                filter.feed(0x16, &mut VecDeque::new()),
                Some(Shortcut::Clipboard)
            );
        }
    }

    #[test]
    fn private_pty_has_cloexec_descriptors_and_reports_resize_and_eof() {
        let termios = unsafe { std::mem::zeroed::<libc::termios>() };
        let size = libc::winsize {
            ws_row: 30,
            ws_col: 90,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let (mut master, slave) = open_pty(&termios, &size).unwrap();
        for file in [&master, &slave] {
            assert_ne!(
                unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
                0
            );
        }
        let mut actual = unsafe { std::mem::zeroed::<libc::winsize>() };
        check(unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCGWINSZ, &mut actual) }).unwrap();
        assert_eq!((actual.ws_row, actual.ws_col), (30, 90));
        drop(slave);
        #[cfg(target_os = "linux")]
        assert_eq!(
            master.read(&mut [0u8; 1]).unwrap_err().raw_os_error(),
            Some(libc::EIO)
        );
        #[cfg(target_os = "macos")]
        assert_eq!(master.read(&mut [0u8; 1]).unwrap(), 0);
    }
}

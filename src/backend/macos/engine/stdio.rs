use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::{
    coalition::Coalition,
    job::{Job, connect_worker},
    process,
    session::Role,
};

static RESIZED: AtomicBool = AtomicBool::new(false);
extern "C" fn resized(_: i32) {
    RESIZED.store(true, Ordering::Relaxed);
}

pub fn command(
    executable: &Path,
    tasks: &Path,
    role: &Role,
    environment: &Path,
    arguments: &[String],
) -> io::Result<(Job, Command)> {
    let body = format!("{}\0", arguments.join("\0"));
    process::arguments(body.as_bytes())?;
    if body.len() > process::MAX_REQUEST || arguments.iter().any(|argument| argument.contains('\0'))
    {
        return Err(io::Error::other("invalid command arguments"));
    }
    let (job, stream) = Job::start(
        executable,
        tasks,
        "__macos-command-worker",
        &[
            role.profile.clone(),
            process::path_text(&role.workspace)?,
            process::path_text(&role.home)?,
            process::path_text(environment)?,
        ],
        &[],
    )?;
    let mut command = Command::new(executable);
    command
        .arg("__macos-stdio")
        .arg(stream.as_raw_fd().to_string())
        .args(arguments)
        .env_clear();
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(stream.as_raw_fd(), libc::F_SETFD, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok((job, command))
}

pub fn bridge(descriptor: i32, arguments: &[String]) -> io::Result<i32> {
    if descriptor < 3 {
        return Err(io::Error::other("invalid command control descriptor"));
    }
    let mut stream = unsafe { UnixStream::from_raw_fd(descriptor) };
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let parent = unsafe { libc::getppid() };
    if parent == 1 {
        return Err(io::Error::other("missing session coordinator"));
    }
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = resized as *const () as usize;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
    }
    if unsafe { libc::sigaction(libc::SIGWINCH, &action, std::ptr::null_mut()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut byte = *b"I";
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = [0_usize; 8];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = unsafe { libc::CMSG_SPACE(12) };
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(12);
        std::ptr::copy_nonoverlapping([0_i32, 1, 2].as_ptr(), libc::CMSG_DATA(header).cast(), 3);
    }
    if unsafe { libc::sendmsg(stream.as_raw_fd(), &message, 0) } != 1 {
        return Err(io::Error::last_os_error());
    }
    let body = format!("{}\0", arguments.join("\0"));
    stream.write_all(&(body.len() as u32).to_be_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.set_nonblocking(true)?;
    loop {
        if unsafe { libc::getppid() } != parent {
            return Err(io::Error::other("session coordinator exited"));
        }
        if RESIZED.swap(false, Ordering::Relaxed) {
            stream.write_all(b"W")?;
        }
        match stream.read(&mut byte) {
            Ok(1) => return Ok(byte[0] as i32),
            Ok(_) => return Err(io::Error::other("command worker disconnected")),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error),
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub fn worker(
    socket: &Path,
    profile: &str,
    workspace: &Path,
    home: &Path,
    environment: &Path,
) -> io::Result<()> {
    let mut stream = connect_worker(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut byte = [0];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = [0_usize; 8];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = std::mem::size_of_val(&control) as _;
    if unsafe { libc::recvmsg(stream.as_raw_fd(), &mut message, 0) } != 1 {
        return Err(io::Error::last_os_error());
    }
    let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
    if header.is_null()
        || unsafe {
            (*header).cmsg_level != libc::SOL_SOCKET
                || (*header).cmsg_type != libc::SCM_RIGHTS
                || (*header).cmsg_len != libc::CMSG_LEN(12)
        }
        || message.msg_flags & libc::MSG_CTRUNC != 0
        || byte != *b"I"
    {
        return Err(io::Error::other("invalid command stdio"));
    }
    let files: Vec<_> = (0..3)
        .map(|index| unsafe {
            File::from_raw_fd(*libc::CMSG_DATA(header).cast::<i32>().add(index))
        })
        .collect();
    for file in &files {
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    let mut length = [0; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > process::MAX_REQUEST {
        return Err(io::Error::other("invalid command request"));
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    let arguments = process::arguments(&bytes)?;
    let environment = process::read_environment(Some(environment))?;
    let coalition = Coalition::read(std::process::id() as i32)?;
    let mut child = process::spawn(
        profile,
        workspace,
        home,
        &arguments,
        coalition,
        &environment,
        [
            files[0].as_raw_fd(),
            files[1].as_raw_fd(),
            files[2].as_raw_fd(),
        ],
    )?;
    drop(files);
    stream.set_nonblocking(true)?;
    loop {
        if child.exited()? {
            let status = child.finish()?;
            stream.write_all(&[status as u8])?;
            return Ok(());
        }
        match stream.read(&mut byte) {
            Ok(1) if byte == *b"W" => unsafe {
                libc::kill(child.0, libc::SIGWINCH);
            },
            Ok(_) => return Err(io::Error::other("command controller disconnected")),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error),
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

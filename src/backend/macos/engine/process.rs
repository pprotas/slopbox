use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::coalition::Coalition;

pub const MAX_REQUEST: usize = 16 * 1024;

unsafe extern "C" {
    fn posix_spawn_file_actions_addchdir_np(
        actions: *mut libc::posix_spawn_file_actions_t,
        directory: *const libc::c_char,
    ) -> libc::c_int;
}

pub(super) fn arguments(bytes: &[u8]) -> io::Result<Vec<&str>> {
    let body = std::str::from_utf8(bytes).map_err(io::Error::other)?;
    let body = body
        .strip_suffix('\0')
        .ok_or_else(|| io::Error::other("unterminated request"))?;
    let arguments: Vec<_> = body.split('\0').collect();
    if arguments.len() > 256 || !arguments[0].starts_with('/') {
        return Err(io::Error::other("invalid arguments"));
    }
    Ok(arguments)
}

pub(super) struct Child(pub(super) libc::pid_t, Coalition);

impl Child {
    pub(super) fn exited(&self) -> io::Result<bool> {
        let mut info = unsafe { std::mem::zeroed() };
        if unsafe {
            libc::waitid(
                libc::P_PID,
                self.0 as _,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } == -1
        {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { info.si_pid() } != 0)
    }

    pub(super) fn finish(&mut self) -> io::Result<i32> {
        self.1.terminate(Some(std::process::id() as i32))?;
        let mut status = 0;
        loop {
            if unsafe { libc::waitpid(self.0, &mut status, 0) } != -1 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        self.0 = 0;
        Ok(if libc::WIFEXITED(status) {
            libc::WEXITSTATUS(status)
        } else {
            128 + libc::WTERMSIG(status)
        })
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if self.0 != 0 {
            let _ = self.finish();
        }
    }
}

pub(super) fn spawn(
    profile: &str,
    workspace: &Path,
    home: &Path,
    arguments: &[&str],
    coalition: Coalition,
    extra_environment: &[String],
    stdio: [libc::c_int; 3],
) -> io::Result<Child> {
    let executable = cstring("/usr/bin/sandbox-exec")?;
    let mut argv = vec![
        executable.clone(),
        cstring("-p")?,
        cstring(profile)?,
        cstring("--")?,
    ];
    for argument in arguments {
        argv.push(cstring(argument)?);
    }
    let mut argv: Vec<_> = argv.iter().map(|value| value.as_ptr() as *mut _).collect();
    argv.push(std::ptr::null_mut());
    let mut environment = vec![
        cstring(&format!("HOME={}", home.display()))?,
        cstring(&format!("TMPDIR={}", home.display()))?,
        cstring("PATH=/usr/bin:/bin")?,
        cstring("LC_ALL=C")?,
    ];
    for entry in extra_environment {
        let key = entry
            .split_once('=')
            .ok_or_else(|| io::Error::other("invalid environment entry"))?
            .0;
        let prefix = format!("{key}=");
        environment.retain(|value| !value.as_bytes().starts_with(prefix.as_bytes()));
        environment.push(cstring(entry)?);
    }
    let mut environment: Vec<_> = environment
        .iter()
        .map(|value| value.as_ptr() as *mut _)
        .collect();
    environment.push(std::ptr::null_mut());
    let workspace = CString::new(workspace.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let mut actions = unsafe { std::mem::zeroed() };
    let mut attributes = unsafe { std::mem::zeroed() };
    check(unsafe { libc::posix_spawn_file_actions_init(&mut actions) })?;
    let result = (|| {
        check(unsafe { libc::posix_spawnattr_init(&mut attributes) })?;
        let result = (|| {
            check(unsafe {
                posix_spawn_file_actions_addchdir_np(&mut actions, workspace.as_ptr())
            })?;
            for (target, source) in stdio.into_iter().enumerate() {
                check(unsafe {
                    libc::posix_spawn_file_actions_adddup2(&mut actions, source, target as _)
                })?;
            }
            let mut signals = unsafe { std::mem::zeroed() };
            unsafe {
                libc::sigemptyset(&mut signals);
            }
            check(unsafe { libc::posix_spawnattr_setsigmask(&mut attributes, &signals) })?;
            unsafe {
                libc::sigfillset(&mut signals);
            }
            check(unsafe { libc::posix_spawnattr_setsigdefault(&mut attributes, &signals) })?;
            check(unsafe { libc::posix_spawnattr_setpgroup(&mut attributes, 0) })?;
            check(unsafe {
                libc::posix_spawnattr_setflags(
                    &mut attributes,
                    (libc::POSIX_SPAWN_CLOEXEC_DEFAULT
                        | libc::POSIX_SPAWN_SETPGROUP
                        | libc::POSIX_SPAWN_SETSIGMASK
                        | libc::POSIX_SPAWN_SETSIGDEF) as _,
                )
            })?;
            let mut pid = 0;
            check(unsafe {
                libc::posix_spawn(
                    &mut pid,
                    executable.as_ptr(),
                    &actions,
                    &attributes,
                    argv.as_ptr(),
                    environment.as_ptr(),
                )
            })?;
            Ok(Child(pid, coalition))
        })();
        unsafe {
            libc::posix_spawnattr_destroy(&mut attributes);
        }
        result
    })();
    unsafe {
        libc::posix_spawn_file_actions_destroy(&mut actions);
    }
    result
}

pub(super) fn read_environment(path: Option<&Path>) -> io::Result<Vec<String>> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let mut bytes = String::new();
    File::open(path)?
        .take(16 * 1024 + 1)
        .read_to_string(&mut bytes)?;
    if bytes.len() > 16 * 1024 {
        return Err(io::Error::other("environment too large"));
    }
    bytes
        .split_terminator('\0')
        .map(|entry| {
            if !entry.contains('=') {
                return Err(io::Error::other("invalid environment entry"));
            }
            Ok(entry.to_owned())
        })
        .collect()
}

fn check(error: libc::c_int) -> io::Result<()> {
    if error == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(error))
    }
}

pub(super) fn path_text(path: &Path) -> io::Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other("non-UTF-8 native path"))
}

fn cstring(value: &str) -> io::Result<CString> {
    CString::new(value).map_err(io::Error::other)
}

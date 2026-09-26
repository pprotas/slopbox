use std::io;
use std::time::{Duration, Instant};

const PROC_PIDUNIQIDENTIFIERINFO: i32 = 17;
const PROC_PIDCOALITIONINFO: i32 = 20;

#[repr(C)]
#[derive(Default)]
struct UniqueIdentifier {
    uuid: [u8; 16],
    unique: u64,
    parent: u64,
    version: i32,
    parent_version: i32,
    reserved: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AuditToken {
    pub val: [u32; 8],
}

unsafe extern "C" {
    fn proc_signal_with_audittoken(token: *const AuditToken, signal: i32) -> i32;
    fn coalition_info_resource_usage(id: u64, usage: *mut u64, size: usize) -> i32;
}

pub struct Identity(AuditToken, u64);

impl Identity {
    pub fn read(pid: i32) -> io::Result<Self> {
        let info: UniqueIdentifier = pid_info(pid, PROC_PIDUNIQIDENTIFIERINFO)?;
        let mut token = AuditToken { val: [0; 8] };
        token.val[5] = pid as u32;
        token.val[7] = info.version as u32;
        Ok(Self(token, info.unique))
    }

    pub fn unique_id(&self) -> u64 {
        self.1
    }

    pub fn matches(&self, token: &AuditToken) -> bool {
        self.0.val[5] == token.val[5] && self.0.val[7] == token.val[7]
    }

    pub fn signal(&self, signal: i32) -> io::Result<()> {
        let error = unsafe { proc_signal_with_audittoken(&self.0, signal) };
        if error == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(error))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Coalition(u64);

impl Coalition {
    pub fn read(pid: i32) -> io::Result<Self> {
        let values: [u64; 5] = pid_info(pid, PROC_PIDCOALITIONINFO)?;
        Ok(Self(values[0]))
    }

    pub(crate) fn restore(id: u64) -> io::Result<Self> {
        if id <= 1 || Self::read(std::process::id() as i32)?.id() == id {
            return Err(io::Error::other("invalid recorded coalition"));
        }
        Ok(Self(id))
    }

    pub fn id(self) -> u64 {
        self.0
    }

    pub fn is_distinct_from(self, other: Self) -> bool {
        self.0 > 1 && self != other
    }

    pub fn terminate(&self, keep: Option<i32>) -> io::Result<()> {
        let own_pid = std::process::id() as i32;
        if self.0 <= 1
            || keep.is_some_and(|pid| pid != own_pid)
            || (Self::read(own_pid)? == *self && keep != Some(own_pid))
        {
            return Err(io::Error::other("refusing to terminate the host coalition"));
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            for pid in all_pids()? {
                if Some(pid) == keep {
                    continue;
                }
                // Capture the version before checking membership. A recycled PID
                // cannot turn this signal into one addressed to the new process.
                let identity = match Identity::read(pid) {
                    Ok(identity) => identity,
                    Err(error) if error.raw_os_error() == Some(libc::ESRCH) => continue,
                    Err(error) => return Err(error),
                };
                let coalition = match Self::read(pid) {
                    Ok(coalition) => coalition,
                    Err(error) if error.raw_os_error() == Some(libc::ESRCH) => continue,
                    Err(error) => return Err(error),
                };
                if coalition != *self {
                    continue;
                }
                if let Err(error) = identity.signal(libc::SIGKILL)
                    && error.raw_os_error() != Some(libc::ESRCH)
                {
                    return Err(error);
                }
            }
            // An empty PID scan is not proof: fork/exec can race that scan.
            if self.live_tasks()? == u64::from(keep.is_some()) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "coalition cleanup incomplete",
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn live_tasks(&self) -> io::Result<u64> {
        // XNU supports a size-limited prefix of coalition_resource_usage.
        let mut counts = [0_u64; 2];
        if unsafe {
            coalition_info_resource_usage(
                self.0,
                counts.as_mut_ptr(),
                std::mem::size_of_val(&counts),
            )
        } != 0
        {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(libc::ESRCH) {
                // launchd may already have reaped this empty, never-reused ID.
                Ok(0)
            } else {
                Err(error)
            };
        }
        counts[0]
            .checked_sub(counts[1])
            .ok_or_else(|| io::Error::other("invalid coalition task counts"))
    }
}

fn pid_info<T: Default>(pid: i32, flavor: i32) -> io::Result<T> {
    let mut value = T::default();
    let size = std::mem::size_of::<T>() as i32;
    let include_zombies = u64::from(flavor == PROC_PIDUNIQIDENTIFIERINFO);
    let result = unsafe {
        libc::proc_pidinfo(
            pid,
            flavor,
            include_zombies,
            (&mut value as *mut T).cast(),
            size,
        )
    };
    if result == size {
        Ok(value)
    } else if result <= 0 {
        Err(io::Error::last_os_error())
    } else {
        Err(io::Error::other("unexpected proc_pidinfo result size"))
    }
}

fn all_pids() -> io::Result<Vec<i32>> {
    let mut pids = vec![0; 1024];
    loop {
        let count = unsafe {
            libc::proc_listallpids(
                pids.as_mut_ptr().cast(),
                (pids.len() * std::mem::size_of::<i32>()) as i32,
            )
        };
        if count <= 0 {
            return Err(io::Error::last_os_error());
        }
        if (count as usize) < pids.len() {
            pids.truncate(count as usize);
            pids.retain(|pid| *pid > 1);
            return Ok(pids);
        }
        if pids.len() >= 128 * 1024 {
            return Err(io::Error::other("process snapshot limit exceeded"));
        }
        pids.resize(pids.len() * 2, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn the_host_coalition_cannot_be_terminated() {
        let coalition = Coalition::read(std::process::id() as i32).unwrap();
        assert!(coalition.live_tasks().unwrap() > 0);
        assert!(coalition.terminate(None).is_err());
    }

    #[test]
    fn a_stale_process_version_cannot_signal_a_live_process() {
        let mut child = Command::new("/bin/sleep")
            .arg("10")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let identity = Identity::read(child.id() as i32).unwrap();
        let mut stale = Identity(identity.0, identity.1);
        stale.0.val[7] = stale.0.val[7].wrapping_add(1);
        let result = stale.signal(libc::SIGKILL);
        let still_running = child.try_wait().unwrap().is_none();
        identity.signal(libc::SIGKILL).unwrap();
        let status = child.wait().unwrap();
        assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::ESRCH));
        assert!(still_running);
        assert!(!status.success());
    }
}

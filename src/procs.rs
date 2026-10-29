//! What fleet needs to know about running processes: which ones are
//! claude, with what arguments, in which folder, under which parent.
//! Linux reads /proc; macOS has no /proc and asks the kernel instead.

#[cfg(not(target_os = "macos"))]
mod os {
    use std::path::{Path, PathBuf};

    fn comm(pid: u32) -> String {
        std::fs::read_to_string(format!("/proc/{}/comm", pid))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    /// One stat, so the list can drop a session that has gone.
    pub fn alive(pid: u32) -> bool {
        Path::new(&format!("/proc/{}", pid)).exists()
    }

    /// Every process named claude, with its arguments. One comm read per
    /// process on the machine; the arguments only for the claude ones.
    pub fn claude_pids() -> Vec<(u32, Vec<String>)> {
        let mut out = Vec::new();
        let Ok(dir) = std::fs::read_dir("/proc") else { return out };
        for e in dir.flatten() {
            let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else { continue };
            if comm(pid) != "claude" {
                continue;
            }
            let cmd = std::fs::read(e.path().join("cmdline")).unwrap_or_default();
            let args = cmd
                .split(|b| *b == 0)
                .map(|s| String::from_utf8_lossy(s).to_string())
                .collect();
            out.push((pid, args));
        }
        out
    }

    pub fn cwd(pid: u32) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{}/cwd", pid)).ok()
    }

    pub fn ppid(pid: u32) -> Option<u32> {
        let status = std::fs::read_to_string(format!("/proc/{}/status", pid)).ok()?;
        status.lines()
            .find_map(|l| l.strip_prefix("PPid:"))
            .and_then(|v| v.trim().parse().ok())
    }

    /// True when the session reads its fleet messages without being typed
    /// to: deck under gluon starts Claude with FLEET_SELF_WAKE=1.
    pub fn wakes_itself(pid: u32) -> bool {
        std::fs::read(format!("/proc/{}/environ", pid))
            .is_ok_and(|e| e.split(|b| *b == 0).any(|v| v == b"FLEET_SELF_WAKE=1"))
    }

    /// Each child's pid and name, with the children of every thread.
    pub fn children(pid: u32) -> Vec<(u32, String)> {
        let mut out = Vec::new();
        let Ok(tasks) = std::fs::read_dir(format!("/proc/{}/task", pid)) else { return out };
        for t in tasks.flatten() {
            let s = std::fs::read_to_string(t.path().join("children")).unwrap_or_default();
            for c in s.split_whitespace() {
                if let Ok(cp) = c.parse::<u32>() {
                    out.push((cp, comm(cp)));
                }
            }
        }
        out
    }
}

#[cfg(target_os = "macos")]
mod os {
    use libc::{c_char, c_int, c_void};
    use std::ffi::CStr;
    use std::path::PathBuf;

    /// Fills a struct through proc_pidinfo; None unless all of it came back.
    fn pidinfo<T>(pid: u32, flavor: c_int) -> Option<T> {
        let mut info: T = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<T>() as c_int;
        let n = unsafe {
            libc::proc_pidinfo(pid as c_int, flavor, 0, &mut info as *mut T as *mut c_void, size)
        };
        (n == size).then_some(info)
    }

    fn name(pid: u32) -> String {
        let mut buf = [0u8; 64];
        let n = unsafe { libc::proc_name(pid as c_int, buf.as_mut_ptr() as *mut c_void, 64) };
        if n <= 0 {
            return String::new();
        }
        CStr::from_bytes_until_nul(&buf)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Pids from proc_listallpids or proc_listchildpids, which take a
    /// buffer size in bytes and return a count.
    fn pid_list(list: impl Fn(*mut c_void, c_int) -> c_int) -> Vec<u32> {
        let n = list(std::ptr::null_mut(), 0);
        if n <= 0 {
            return Vec::new();
        }
        let mut buf = vec![0 as c_int; n as usize + 64];
        let bytes = (buf.len() * std::mem::size_of::<c_int>()) as c_int;
        let got = list(buf.as_mut_ptr() as *mut c_void, bytes).max(0) as usize;
        buf.truncate(got.min(buf.len()));
        buf.into_iter().filter(|p| *p > 0).map(|p| p as u32).collect()
    }

    /// The arguments, from KERN_PROCARGS2: argc, the program path, NULs,
    /// then argc strings.
    fn args(pid: u32) -> Vec<String> {
        let mut max: c_int = 0;
        let mut len = std::mem::size_of::<c_int>();
        let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
        let r = unsafe {
            libc::sysctl(mib.as_mut_ptr(), 2, &mut max as *mut c_int as *mut c_void,
                         &mut len, std::ptr::null_mut(), 0)
        };
        if r != 0 || max <= 0 {
            return Vec::new();
        }
        let mut buf = vec![0u8; max as usize];
        let mut len = buf.len();
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as c_int];
        let r = unsafe {
            libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr() as *mut c_void,
                         &mut len, std::ptr::null_mut(), 0)
        };
        if r != 0 || len < 4 {
            return Vec::new();
        }
        let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]).max(0) as usize;
        let rest = &buf[4..len];
        let mut i = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
        while i < rest.len() && rest[i] == 0 {
            i += 1;
        }
        rest[i..].split(|b| *b == 0)
            .take(argc)
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect()
    }

    pub fn alive(pid: u32) -> bool {
        let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    pub fn claude_pids() -> Vec<(u32, Vec<String>)> {
        pid_list(|b, n| unsafe { libc::proc_listallpids(b, n) })
            .into_iter()
            .filter(|p| name(*p) == "claude")
            .map(|p| (p, args(p)))
            .collect()
    }

    pub fn cwd(pid: u32) -> Option<PathBuf> {
        let info: libc::proc_vnodepathinfo = pidinfo(pid, libc::PROC_PIDVNODEPATHINFO)?;
        let path = unsafe { CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr() as *const c_char) };
        let path = path.to_string_lossy();
        (!path.is_empty()).then(|| PathBuf::from(path.into_owned()))
    }

    pub fn ppid(pid: u32) -> Option<u32> {
        let info: libc::proc_bsdinfo = pidinfo(pid, libc::PROC_PIDTBSDINFO)?;
        Some(info.pbi_ppid)
    }

    /// gluon, the one host that wakes its session itself, runs on Linux.
    pub fn wakes_itself(_pid: u32) -> bool {
        false
    }

    pub fn children(pid: u32) -> Vec<(u32, String)> {
        pid_list(|b, n| unsafe { libc::proc_listchildpids(pid as libc::pid_t, b, n) })
            .into_iter()
            .map(|c| (c, name(c)))
            .collect()
    }
}

pub use os::{alive, children, claude_pids, cwd, ppid, wakes_itself};

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    #[test]
    fn a_process_started_with_the_mark_wakes_itself() {
        let mut marked = std::process::Command::new("sleep").arg("5")
            .env("FLEET_SELF_WAKE", "1").spawn().unwrap();
        // spawn can return before the child's exec, while it still has
        // the test's own environment; wait for the exec.
        let marked_now = || super::wakes_itself(marked.id());
        let seen = (0..100).any(|_| marked_now() || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            false
        });
        assert!(seen);
        assert!(!super::wakes_itself(std::process::id()));
        let _ = marked.kill();
        let _ = marked.wait();
    }
}

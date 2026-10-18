//! ruos-init — PID 1 for ruOS.
//!
//! Boot: mount the kernel filesystems, set the hostname, bring up the network
//! (static addresses for QEMU user networking), then supervise two services:
//! `ruosd` (the AI system service) and a login shell on the console. Both are
//! restarted if they exit. Orphans are reaped.
//!
//! Shutdown follows the busybox convention so `poweroff`, `reboot` and `halt`
//! work: SIGUSR2 = power off, SIGTERM = reboot, SIGUSR1 = halt.

use std::ffi::CString;
use std::fs;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PATH: &str = "/bin:/sbin:/usr/bin:/usr/sbin";

fn log(msg: &str) {
    // /dev/kmsg puts our messages in the kernel log alongside boot messages.
    if fs::write("/dev/kmsg", format!("ruos-init: {msg}\n")).is_err() {
        eprintln!("ruos-init: {msg}");
    }
}

fn mount(src: &str, target: &str, fstype: &str, flags: libc::c_ulong) {
    let _ = fs::create_dir_all(target);
    let c = |s: &str| CString::new(s).expect("no NUL in mount args");
    let (src, tgt, ty) = (c(src), c(target), c(fstype));
    let rc = unsafe { libc::mount(src.as_ptr(), tgt.as_ptr(), ty.as_ptr(), flags, std::ptr::null()) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EBUSY) {
            log(&format!("mount {target} ({fstype}) failed: {err}"));
        }
    }
}

fn run(cmd: &str, args: &[&str]) {
    match Command::new(cmd).args(args).env("PATH", PATH).status() {
        Ok(s) if s.success() => {}
        Ok(s) => log(&format!("{cmd} {} exited with {s}", args.join(" "))),
        Err(e) => log(&format!("{cmd}: {e}")),
    }
}

fn setup_system() {
    let nodev = (libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC) as libc::c_ulong;
    mount("proc", "/proc", "proc", nodev);
    mount("sysfs", "/sys", "sysfs", nodev);
    // The kernel does not auto-mount devtmpfs when booting from an initramfs.
    mount("devtmpfs", "/dev", "devtmpfs", libc::MS_NOSUID as libc::c_ulong);
    mount("devpts", "/dev/pts", "devpts", (libc::MS_NOSUID | libc::MS_NOEXEC) as libc::c_ulong);
    mount("tmpfs", "/run", "tmpfs", (libc::MS_NOSUID | libc::MS_NODEV) as libc::c_ulong);
    mount("tmpfs", "/tmp", "tmpfs", (libc::MS_NOSUID | libc::MS_NODEV) as libc::c_ulong);

    let hostname = fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()).unwrap_or_else(|_| "ruos".into());
    unsafe { libc::sethostname(hostname.as_ptr().cast(), hostname.len()) };

    // QEMU user-mode networking: guest 10.0.2.15, gateway 10.0.2.2, DNS 10.0.2.3.
    run("ip", &["link", "set", "lo", "up"]);
    if fs::metadata("/sys/class/net/eth0").is_ok() {
        run("ip", &["link", "set", "eth0", "up"]);
        run("ip", &["addr", "add", "10.0.2.15/24", "dev", "eth0"]);
        run("ip", &["route", "add", "default", "via", "10.0.2.2"]);
        let _ = fs::write("/etc/resolv.conf", "nameserver 10.0.2.3\n");
    } else {
        log("no eth0; networking disabled");
    }
}

/// The tty behind /dev/console (e.g. ttyS0), so the shell gets a real
/// controlling terminal and job control works.
fn console_tty() -> String {
    fs::read_to_string("/sys/class/tty/console/active")
        .ok()
        .and_then(|s| s.split_whitespace().last().map(|t| format!("/dev/{t}")))
        .unwrap_or_else(|| "/dev/console".into())
}

fn unblock_signals() {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
    }
}

fn spawn_ruosd() -> Option<Child> {
    let logf = fs::OpenOptions::new().create(true).append(true).open("/var/log/ruosd.log").ok();
    let mut cmd = Command::new("/usr/sbin/ruosd");
    cmd.env_clear().env("PATH", PATH).env("HOME", "/root").stdin(Stdio::null());
    if let Some(f) = logf {
        cmd.stdout(f.try_clone().map(Stdio::from).unwrap_or(Stdio::null())).stderr(Stdio::from(f));
    }
    unsafe { cmd.pre_exec(|| { unblock_signals(); Ok(()) }) };
    cmd.spawn().map_err(|e| log(&format!("cannot start ruosd: {e}"))).ok()
}

fn spawn_shell(tty: &str) -> Option<Child> {
    let tty_c = CString::new(tty).expect("no NUL in tty path");
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-l")
        .current_dir("/root")
        .env_clear()
        .env("PATH", PATH)
        .env("HOME", "/root")
        .env("TERM", "vt100")
        .env("USER", "root");
    unsafe {
        cmd.pre_exec(move || {
            unblock_signals();
            libc::setsid();
            let fd = libc::open(tty_c.as_ptr(), libc::O_RDWR);
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            libc::ioctl(fd, libc::TIOCSCTTY, 0);
            for target in 0..3 {
                libc::dup2(fd, target);
            }
            if fd > 2 {
                libc::close(fd);
            }
            Ok(())
        })
    };
    cmd.spawn().map_err(|e| log(&format!("cannot start shell on {tty}: {e}"))).ok()
}

struct Service {
    name: &'static str,
    child: Option<Child>,
    last_start: Instant,
    start: Box<dyn Fn() -> Option<Child>>,
}

impl Service {
    fn new(name: &'static str, start: Box<dyn Fn() -> Option<Child>>) -> Service {
        Service { name, child: start(), last_start: Instant::now(), start }
    }

    /// Restart if it has exited, at most once a second so a crash loop cannot spin.
    fn supervise(&mut self) {
        let exited = match &mut self.child {
            Some(c) => matches!(c.try_wait(), Ok(Some(_)) | Err(_)),
            None => true,
        };
        if exited && self.last_start.elapsed() >= Duration::from_secs(1) {
            if self.child.is_some() {
                log(&format!("{} exited; restarting", self.name));
            }
            self.child = (self.start)();
            self.last_start = Instant::now();
        }
    }
}

fn shutdown(how: libc::c_int) -> ! {
    log("shutting down");
    unsafe {
        libc::kill(-1, libc::SIGTERM);
        std::thread::sleep(Duration::from_secs(1));
        libc::kill(-1, libc::SIGKILL);
        libc::sync();
        libc::reboot(how);
    }
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

fn main() {
    if std::process::id() != 1 {
        eprintln!("ruos-init must run as PID 1");
        std::process::exit(1);
    }

    // Block the signals we handle; we pick them up synchronously below.
    let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::sigemptyset(&mut set);
        for s in [libc::SIGCHLD, libc::SIGUSR1, libc::SIGUSR2, libc::SIGTERM, libc::SIGINT] {
            libc::sigaddset(&mut set, s);
        }
        libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }

    setup_system();
    let tty = console_tty();
    log(&format!("system up; console on {tty}"));

    let mut services = vec![
        Service::new("ruosd", Box::new(spawn_ruosd)),
        Service::new("shell", Box::new(move || spawn_shell(&tty))),
    ];

    let timeout = libc::timespec { tv_sec: 1, tv_nsec: 0 };
    loop {
        let sig = unsafe { libc::sigtimedwait(&set, std::ptr::null_mut(), &timeout) };
        match sig {
            libc::SIGUSR2 => shutdown(libc::RB_POWER_OFF),
            libc::SIGTERM => shutdown(libc::RB_AUTOBOOT),
            libc::SIGUSR1 => shutdown(libc::RB_HALT_SYSTEM),
            _ => {}
        }
        for s in &mut services {
            s.supervise();
        }
        // Reap orphans re-parented to us (our own children are reaped by try_wait above).
        loop {
            let mut status = 0;
            let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if pid <= 0 {
                break;
            }
        }
    }
}

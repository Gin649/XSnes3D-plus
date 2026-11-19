//! XSnes3D+: the crash catcher (Android only). A crash inside Mesen's C++ code ends the app
//! without a Rust panic, so the play report could only say "the app ended during this game".
//! With the catcher, the report also gets one line saying what the crash was and where in the
//! app's library it happened (`pc` is the crashing instruction, `lr` the code that called it,
//! both as offsets into the library, which can be looked up in the build).
//!
//! The line is written straight to the report's file, without locks or memory allocation,
//! since nothing else is safe inside a crash. Then the system's own handler runs as before, so
//! `adb logcat` still shows its crash dump. Outside Android these functions do nothing.

use std::sync::atomic::{AtomicI32, Ordering};

/// The open play report's file, or -1 when no report is open.
static REPORT_FD: AtomicI32 = AtomicI32::new(-1);

/// Points the crash line at the play report's file `fd`, or at nothing with -1.
pub fn set_report_fd(fd: i32) {
    REPORT_FD.store(fd, Ordering::SeqCst);
}

/// Installs the catcher once, at start-up.
pub fn install() {
    #[cfg(target_os = "android")]
    android::install();
}

#[cfg(target_os = "android")]
mod android {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Once;

    use super::REPORT_FD;

    /// The crashes caught, and the system's own handlers they had before.
    const SIGNALS: [i32; 5] = [
        libc::SIGSEGV,
        libc::SIGBUS,
        libc::SIGILL,
        libc::SIGFPE,
        libc::SIGABRT,
    ];
    static mut OLD: [Option<libc::sigaction>; 5] = [None; 5];
    /// Where the app's library starts in memory, 0 when not known.
    static LIBRARY_BASE: AtomicUsize = AtomicUsize::new(0);
    static ONCE: Once = Once::new();

    pub fn install() {
        ONCE.call_once(|| {
            // SAFETY: dladdr only reads the address of a function of this library, and the
            // sigaction calls get valid, fully set structures. OLD is written here once,
            // before any handler can run.
            unsafe {
                let mut info: libc::Dl_info = std::mem::zeroed();
                if libc::dladdr(install as *const libc::c_void, &mut info) != 0 {
                    LIBRARY_BASE.store(info.dli_fbase as usize, Ordering::SeqCst);
                }
                for (slot, &signal) in SIGNALS.iter().enumerate() {
                    let mut action: libc::sigaction = std::mem::zeroed();
                    action.sa_sigaction = handler as *const () as usize;
                    action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
                    libc::sigemptyset(&mut action.sa_mask);
                    let mut old: libc::sigaction = std::mem::zeroed();
                    if libc::sigaction(signal, &action, &mut old) == 0 {
                        (*std::ptr::addr_of_mut!(OLD))[slot] = Some(old);
                    }
                }
            }
        });
    }

    /// Small text buffer on the stack.
    struct Line {
        bytes: [u8; 256],
        len: usize,
    }

    impl Line {
        fn text(&mut self, text: &str) {
            for &byte in text.as_bytes() {
                if self.len < self.bytes.len() {
                    self.bytes[self.len] = byte;
                    self.len += 1;
                }
            }
        }

        fn number(&mut self, mut value: u64) {
            let mut digits = [0u8; 20];
            let mut count = 0;
            loop {
                digits[count] = b'0' + (value % 10) as u8;
                count += 1;
                value /= 10;
                if value == 0 {
                    break;
                }
            }
            while count > 0 {
                count -= 1;
                self.text(core::str::from_utf8(&digits[count..=count]).unwrap_or("?"));
            }
        }

        fn hex(&mut self, value: u64) {
            self.text("0x");
            let mut started = false;
            for shift in (0..16).rev() {
                let digit = ((value >> (shift * 4)) & 0xF) as usize;
                if digit != 0 || started || shift == 0 {
                    started = true;
                    self.text(&"0123456789abcdef"[digit..=digit]);
                }
            }
        }

        /// An address, as an offset into the app's library when it lies in it.
        #[cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
        fn address(&mut self, value: u64) {
            let base = LIBRARY_BASE.load(Ordering::SeqCst) as u64;
            if base != 0 && value >= base && value - base < 0x1000_0000 {
                self.text("lib+");
                self.hex(value - base);
            } else {
                self.hex(value);
            }
        }
    }

    fn signal_name(signal: i32) -> &'static str {
        match signal {
            libc::SIGSEGV => "SIGSEGV, a bad memory access",
            libc::SIGBUS => "SIGBUS, a bad memory access",
            libc::SIGILL => "SIGILL, a bad instruction",
            libc::SIGFPE => "SIGFPE, a bad calculation",
            libc::SIGABRT => "SIGABRT, the program stopped itself",
            _ => "unknown",
        }
    }

    extern "C" fn handler(signal: i32, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
        let fd = REPORT_FD.load(Ordering::SeqCst);
        if fd >= 0 {
            let mut line = Line {
                bytes: [0; 256],
                len: 0,
            };
            line.text("== NATIVE CRASH: signal ");
            line.number(u64::try_from(signal).unwrap_or(0));
            line.text(" (");
            line.text(signal_name(signal));
            line.text(")");
            // SAFETY: the system hands a valid siginfo and context to a SA_SIGINFO handler.
            unsafe {
                if !info.is_null() {
                    line.text(", address ");
                    line.hex((*info).si_addr() as u64);
                }
                // The registers in the context, at the places the arm64 Linux layout keeps
                // them: lr is register 30, then sp, then pc.
                #[cfg(target_arch = "aarch64")]
                if !context.is_null() {
                    let base = context.cast::<u8>();
                    let lr = base.add(424).cast::<u64>().read_unaligned();
                    let pc = base.add(440).cast::<u64>().read_unaligned();
                    line.text(", pc ");
                    line.address(pc);
                    line.text(", lr ");
                    line.address(lr);
                }
            }
            line.text("\n");
            // SAFETY: write is safe inside a signal handler; the buffer is valid for len bytes.
            unsafe {
                libc::write(fd, line.bytes.as_ptr().cast(), line.len);
            }
        }
        #[cfg(not(target_arch = "aarch64"))]
        let _ = context;

        // Hands the crash back to the system's handler: puts it back and lets the crash
        // happen again (or raises it again for an abort), so the usual crash dump follows.
        // SAFETY: OLD was filled before the handler was installed and is only read here.
        unsafe {
            let slot = SIGNALS.iter().position(|&caught| caught == signal);
            let old = slot.and_then(|slot| (*std::ptr::addr_of!(OLD))[slot]);
            match old {
                Some(old) => {
                    libc::sigaction(signal, &old, std::ptr::null_mut());
                }
                None => {
                    libc::signal(signal, libc::SIG_DFL);
                }
            }
            if signal == libc::SIGABRT {
                libc::raise(signal);
            }
        }
    }
}

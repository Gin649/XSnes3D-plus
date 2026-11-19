//! XSnes3D+: keeps the calling thread on chosen processor cores (Android only). The choice of
//! cores is made in `zax_app::cpu_affinity`; this is only the system call, which needs `unsafe`.

/// Keeps the calling thread on the cores numbered `indices`. Returns whether the system
/// agreed; always false outside Android.
#[cfg(target_os = "android")]
#[must_use]
pub fn pin_calling_thread(indices: &[usize]) -> bool {
    // A set of 1024 cores, one bit each, as the C library's `cpu_set_t` is.
    let mut mask = [0_u64; 16];
    for &index in indices {
        if index < 1024 {
            mask[index / 64] |= 1 << (index % 64);
        }
    }
    extern "C" {
        fn sched_setaffinity(pid: i32, size: usize, mask: *const u64) -> i32;
    }
    // SAFETY: pid 0 is the calling thread, and the mask is a live array of the size given.
    unsafe { sched_setaffinity(0, std::mem::size_of_val(&mask), mask.as_ptr()) == 0 }
}

/// Keeps the calling thread on the cores numbered `indices`. Returns whether the system
/// agreed; always false outside Android.
#[cfg(not(target_os = "android"))]
#[must_use]
pub const fn pin_calling_thread(_indices: &[usize]) -> bool {
    false
}

//! XSnes3D+: which processor cores the emulation and the drawing run on (Android only).
//!
//! A phone or handheld chip mixes slow cores that save power with a few fast ones. Android
//! may leave the emulation thread on a slow core, where a SNES frame took two to three times
//! longer on the Thor. So the emulation thread asks for the fastest core, and the main thread
//! (drawing, 3D conversion) for the other fast ones, leaving the slow cores to the system.
//!
//! The speed of each core is read from `cpuinfo_max_freq` in sysfs. When every core has the
//! same top speed, or the files cannot be read, nothing is changed. On other systems these
//! functions do nothing.

/// One core and its highest speed, in kHz.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Core {
    /// The core's number, as the system counts them.
    pub index: usize,
    /// Its highest speed in kHz.
    pub max_khz: u64,
}

/// The cores for the emulation thread and for the main thread, from the cores of the chip:
/// the emulation gets the fastest core (the lowest numbered when several tie), the main
/// thread every other core faster than the slowest kind. `None` when the cores do not differ.
#[must_use]
pub fn plan(cores: &[Core]) -> Option<(Vec<usize>, Vec<usize>)> {
    let fastest = cores.iter().map(|core| core.max_khz).max()?;
    let slowest = cores.iter().map(|core| core.max_khz).min()?;
    if fastest == slowest {
        return None;
    }
    let emulation = cores
        .iter()
        .filter(|core| core.max_khz == fastest)
        .map(|core| core.index)
        .min()?;
    let mut main: Vec<usize> = cores
        .iter()
        .filter(|core| core.max_khz > slowest && core.index != emulation)
        .map(|core| core.index)
        .collect();
    // A chip with one fast core and the rest slow: the main thread shares nothing better, so
    // it keeps the fast core too rather than being sent to the slow ones only.
    if main.is_empty() {
        main.push(emulation);
    }
    Some((vec![emulation], main))
}

/// Reads the cores of this machine; empty when sysfs cannot be read.
#[must_use]
pub fn cores() -> Vec<Core> {
    let mut found = Vec::new();
    for index in 0..64 {
        let path = format!("/sys/devices/system/cpu/cpu{index}/cpufreq/cpuinfo_max_freq");
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                if let Ok(max_khz) = text.trim().parse::<u64>() {
                    found.push(Core { index, max_khz });
                }
            }
            // The numbering has no gaps on the chips this is for: the first missing core ends it.
            Err(_) => break,
        }
    }
    found
}

/// Which thread asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The emulation thread: the fastest core.
    Emulation,
    /// The main thread: the other fast cores.
    Main,
}

/// The plan for this machine, read once.
fn machine_plan() -> Option<&'static (Vec<usize>, Vec<usize>)> {
    static PLAN: std::sync::OnceLock<Option<(Vec<usize>, Vec<usize>)>> = std::sync::OnceLock::new();
    PLAN.get_or_init(|| plan(&cores())).as_ref()
}

/// [`pin`] without the log line, for threads started every frame. Does nothing outside Android.
pub fn pin_quietly(role: Role) {
    if !cfg!(target_os = "android") {
        return;
    }
    if let Some((emulation, main)) = machine_plan() {
        let chosen = match role {
            Role::Emulation => emulation,
            Role::Main => main,
        };
        let _ = zax_platform::thread_cores::pin_calling_thread(chosen);
    }
}

/// Keeps the calling thread on the cores [`plan`] gives its `role`, and logs what was done.
/// Does nothing outside Android.
pub fn pin(role: Role) {
    if !cfg!(target_os = "android") {
        return;
    }
    let cores = cores();
    let Some((emulation, main)) = plan(&cores) else {
        return;
    };
    let (name, chosen) = match role {
        Role::Emulation => ("emulation thread", emulation),
        Role::Main => ("main thread", main),
    };
    let speeds: Vec<String> = cores
        .iter()
        .map(|core| format!("{}:{}", core.index, core.max_khz / 1000))
        .collect();
    let result = if zax_platform::thread_cores::pin_calling_thread(&chosen) {
        "kept on"
    } else {
        "could not be kept on"
    };
    crate::app_log::line(&format!(
        "XSnes3D+: cores (MHz) {}; the {name} {result} cores {chosen:?}",
        speeds.join(" ")
    ));
}

#[cfg(test)]
#[allow(clippy::expect_used)] // test code
mod tests {
    use super::*;

    fn chip(speeds: &[u64]) -> Vec<Core> {
        speeds
            .iter()
            .enumerate()
            .map(|(index, &max_khz)| Core { index, max_khz })
            .collect()
    }

    #[test]
    fn the_thor_chip_gives_the_top_core_to_the_emulation() {
        // Snapdragon 8 Gen 2: three slow cores, four middle ones, one top core.
        let cores = chip(&[
            2_016_000, 2_016_000, 2_016_000, 2_803_200, 2_803_200, 2_803_200, 2_803_200, 3_187_200,
        ]);
        let (emulation, main) = plan(&cores).expect("the cores differ");
        assert_eq!(vec![7], emulation);
        assert_eq!(vec![3, 4, 5, 6], main);
    }

    #[test]
    fn same_speed_cores_are_left_alone() {
        assert_eq!(None, plan(&chip(&[3_000_000; 8])));
        assert_eq!(None, plan(&[]));
    }

    #[test]
    fn one_fast_core_is_shared() {
        let (emulation, main) = plan(&chip(&[1_800_000, 1_800_000, 2_400_000])).expect("differ");
        assert_eq!(vec![2], emulation);
        assert_eq!(vec![2], main);
    }
}

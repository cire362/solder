//! In-app measurements for the numbers the website promises.
//!
//! - Input latency: from the moment an input handler starts to the end of the
//!   paint that shows its result. This includes waiting for the next display
//!   refresh, so it is what the user feels minus the GPU present.
//! - Frame time: prepaint plus paint of the editor element.
//! - Cold start: `main` entered to the first frame painted.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use gpui::{App, Global};

const WINDOW: usize = 240;

pub struct Perf {
    pub process_start: Instant,
    pub first_frame: Option<Duration>,
    pending_input: Option<Instant>,
    input: VecDeque<Duration>,
    frames: VecDeque<Duration>,
    pub hud_visible: bool,
}

impl Global for Perf {}

impl Perf {
    pub fn new(process_start: Instant) -> Self {
        Self {
            process_start,
            first_frame: None,
            pending_input: None,
            input: VecDeque::with_capacity(WINDOW),
            frames: VecDeque::with_capacity(WINDOW),
            hud_visible: true,
        }
    }

    /// Marks the start of an input. Repeated inputs before the next frame keep
    /// the earliest timestamp, so key repeat does not hide latency.
    pub fn input_started(cx: &mut App) {
        let perf = cx.global_mut::<Perf>();
        perf.pending_input.get_or_insert_with(Instant::now);
    }

    pub fn frame_painted(cx: &mut App, frame: Duration) {
        let perf = cx.global_mut::<Perf>();
        push(&mut perf.frames, frame);
        if let Some(start) = perf.pending_input.take() {
            push(&mut perf.input, start.elapsed());
        }
    }

    pub fn input_p50_p99(&self) -> Option<(Duration, Duration)> {
        percentiles(&self.input)
    }

    pub fn frame_p50_p99(&self) -> Option<(Duration, Duration)> {
        percentiles(&self.frames)
    }
}

fn push(q: &mut VecDeque<Duration>, d: Duration) {
    if q.len() == WINDOW {
        q.pop_front();
    }
    q.push_back(d);
}

fn percentiles(q: &VecDeque<Duration>) -> Option<(Duration, Duration)> {
    if q.is_empty() {
        return None;
    }
    let mut v: Vec<Duration> = q.iter().copied().collect();
    v.sort_unstable();
    let at = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    Some((at(0.5), at(0.99)))
}

pub fn ms(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms < 10.0 {
        format!("{ms:.1}")
    } else {
        format!("{ms:.0}")
    }
}

/// Resident memory of this process, in bytes.
#[cfg(target_os = "macos")]
pub fn resident_memory() -> Option<u64> {
    // Read straight from mach so the status bar never spawns `ps`.
    use std::mem::MaybeUninit;

    #[repr(C)]
    struct TaskVmInfo {
        virtual_size: u64,
        region_count: i32,
        page_size: i32,
        resident_size: u64,
        resident_size_peak: u64,
        device: u64,
        device_peak: u64,
        internal: u64,
        internal_peak: u64,
        external: u64,
        external_peak: u64,
        reusable: u64,
        reusable_peak: u64,
        purgeable_volatile_pmap: u64,
        purgeable_volatile_resident: u64,
        purgeable_volatile_virtual: u64,
        compressed: u64,
        compressed_peak: u64,
        compressed_lifetime: u64,
        phys_footprint: u64,
    }
    unsafe extern "C" {
        static mach_task_self_: u32;
        fn task_info(task: u32, flavor: i32, info: *mut i32, count: *mut u32) -> i32;
    }
    const TASK_VM_INFO: i32 = 22;
    let mut info = MaybeUninit::<TaskVmInfo>::zeroed();
    let mut count = (size_of::<TaskVmInfo>() / size_of::<i32>()) as u32;
    // SAFETY: `info` is sized for TASK_VM_INFO and `count` says so.
    let kr = unsafe {
        task_info(
            mach_task_self_,
            TASK_VM_INFO,
            info.as_mut_ptr().cast(),
            &mut count,
        )
    };
    // phys_footprint is what Activity Monitor reports as "Memory".
    (kr == 0).then(|| unsafe { info.assume_init() }.phys_footprint)
}

#[cfg(not(target_os = "macos"))]
pub fn resident_memory() -> Option<u64> {
    None
}

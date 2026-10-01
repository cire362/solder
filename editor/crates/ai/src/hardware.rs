//! What the machine can run: memory, the GPU llama.cpp would use and how
//! much of its memory a model may take. Read from `sysctl`, `/proc` and
//! `nvidia-smi` rather than a system-information crate.

use std::{path::Path, process::Command};

const GB: u64 = 1_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Apple Silicon: one pool of memory shared with the CPU.
    Metal,
    Vulkan,
    Cpu,
}

#[derive(Clone, Debug)]
pub struct Gpu {
    pub name: String,
    /// Dedicated memory; 0 when the GPU shares system memory.
    pub memory: u64,
}

#[derive(Clone, Debug)]
pub struct Hardware {
    pub os: &'static str,
    pub arch: &'static str,
    pub cpu: String,
    pub cores: usize,
    pub memory: u64,
    /// The macOS GPU wired limit set with `sysctl iogpu.wired_limit_mb`.
    pub wired_limit: Option<u64>,
    pub gpu: Option<Gpu>,
    pub backend: Backend,
}

impl Hardware {
    /// Bytes a model may use for weights and cache while the editor, a
    /// browser and dev servers keep running.
    pub fn model_budget(&self) -> u64 {
        match self.backend {
            // macOS lets the GPU wire about two thirds of memory on smaller
            // machines and three quarters on larger ones, unless raised.
            Backend::Metal => self.wired_limit.unwrap_or(if self.memory <= 36 * GB {
                self.memory * 2 / 3
            } else {
                self.memory * 3 / 4
            }),
            Backend::Vulkan => match &self.gpu {
                // Layers that do not fit spill to system memory, slowly.
                Some(gpu) if gpu.memory > 0 => gpu.memory.saturating_sub(GB / 2),
                _ => cpu_budget(self.memory),
            },
            Backend::Cpu => cpu_budget(self.memory),
        }
    }

    /// `Apple M4 · 10 cores · 16 GB`.
    pub fn summary(&self) -> String {
        let mut parts = vec![self.cpu.clone(), format!("{} cores", self.cores)];
        // Memory is sold in binary gigabytes: 16 GB is 2^34 bytes.
        parts.push(format!(
            "{} GB",
            (self.memory as f64 / (1u64 << 30) as f64).round()
        ));
        if let Some(gpu) = &self.gpu
            && gpu.memory > 0
        {
            parts.push(format!("{} ({})", gpu.name, crate::format_size(gpu.memory)));
        }
        parts.join(" · ")
    }
}

fn cpu_budget(memory: u64) -> u64 {
    (memory * 3 / 5).min(memory.saturating_sub(4 * GB))
}

/// Reads the machine. Blocks for a few milliseconds (or longer when
/// `nvidia-smi` is installed): call it from a background executor.
pub fn detect() -> Hardware {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut hw = Hardware {
        os,
        arch,
        cpu: String::new(),
        cores,
        memory: 0,
        wired_limit: None,
        gpu: None,
        backend: Backend::Cpu,
    };
    match os {
        "macos" => {
            hw.memory = sysctl("hw.memsize")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            hw.cpu = sysctl("machdep.cpu.brand_string").unwrap_or_default();
            hw.wired_limit = sysctl("iogpu.wired_limit_mb")
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|&mb| mb > 0)
                .map(|mb| mb * 1024 * 1024);
            if arch == "aarch64" {
                hw.backend = Backend::Metal;
                hw.gpu = Some(Gpu {
                    name: hw.cpu.clone(),
                    memory: 0,
                });
            }
        }
        "linux" => {
            if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
                hw.memory = meminfo_total(&text).unwrap_or(0);
            }
            if let Ok(text) = std::fs::read_to_string("/proc/cpuinfo") {
                hw.cpu = cpuinfo_model(&text).unwrap_or_default();
            }
            hw.gpu = nvidia().or_else(|| {
                // Any render node means a GPU Vulkan can use; its memory is
                // unknown, so the budget stays the CPU one.
                std::fs::read_dir("/dev/dri")
                    .ok()?
                    .flatten()
                    .any(|e| e.file_name().to_string_lossy().starts_with("renderD"))
                    .then(|| Gpu {
                        name: "GPU".into(),
                        memory: 0,
                    })
            });
            if hw.gpu.is_some() {
                hw.backend = Backend::Vulkan;
            }
        }
        "windows" => {
            let script = "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory; \
                          (Get-CimInstance Win32_Processor).Name";
            if let Some(out) = run("powershell", &["-NoProfile", "-Command", script]) {
                let mut lines = out.lines();
                hw.memory = lines
                    .next()
                    .and_then(|l| l.trim().parse().ok())
                    .unwrap_or(0);
                hw.cpu = lines.next().unwrap_or("").trim().to_string();
            }
            hw.gpu = nvidia();
            if hw.gpu.is_some() {
                hw.backend = Backend::Vulkan;
            }
        }
        _ => {}
    }
    if hw.cpu.is_empty() {
        hw.cpu = arch.to_string();
    }
    hw
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn sysctl(name: &str) -> Option<String> {
    run("sysctl", &["-n", name]).filter(|s| !s.is_empty())
}

fn nvidia() -> Option<Gpu> {
    let out = run(
        "nvidia-smi",
        &[
            "--query-gpu=name,memory.total",
            "--format=csv,noheader,nounits",
        ],
    )?;
    nvidia_smi(&out)
}

/// The largest GPU in `nvidia-smi --query-gpu=name,memory.total` CSV (MiB).
pub fn nvidia_smi(csv: &str) -> Option<Gpu> {
    csv.lines()
        .filter_map(|line| {
            let (name, mib) = line.rsplit_once(',')?;
            Some(Gpu {
                name: name.trim().to_string(),
                memory: mib.trim().parse::<u64>().ok()? * 1024 * 1024,
            })
        })
        .max_by_key(|g| g.memory)
}

pub fn meminfo_total(text: &str) -> Option<u64> {
    let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

pub fn cpuinfo_model(text: &str) -> Option<String> {
    text.lines()
        .find(|l| l.starts_with("model name"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.trim().to_string())
}

/// Free bytes on the volume holding `dir`, from `df` (or PowerShell).
pub fn free_space(dir: &Path) -> Option<u64> {
    if cfg!(windows) {
        let root = dir
            .components()
            .next()?
            .as_os_str()
            .to_string_lossy()
            .into_owned();
        let letter = root.trim_end_matches([':', '\\']);
        let script = format!("(Get-PSDrive {letter}).Free");
        return run("powershell", &["-NoProfile", "-Command", &script])?
            .trim()
            .parse()
            .ok();
    }
    let out = run("df", &["-Pk", &dir.to_string_lossy()])?;
    df_available(&out)
}

/// The available column of `df -Pk`, in bytes.
pub fn df_available(out: &str) -> Option<u64> {
    let line = out.lines().nth(1)?;
    let kb: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
    Some(kb * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mac(memory_gb: u64) -> Hardware {
        Hardware {
            os: "macos",
            arch: "aarch64",
            cpu: "Apple M4".into(),
            cores: 10,
            memory: memory_gb * GB,
            wired_limit: None,
            gpu: Some(Gpu {
                name: "Apple M4".into(),
                memory: 0,
            }),
            backend: Backend::Metal,
        }
    }

    #[test]
    fn budgets() {
        assert_eq!(mac(16).model_budget(), 16 * GB * 2 / 3);
        assert_eq!(mac(64).model_budget(), 48 * GB);
        let mut raised = mac(16);
        raised.wired_limit = Some(14 * GB);
        assert_eq!(raised.model_budget(), 14 * GB);

        let mut pc = mac(32);
        pc.backend = Backend::Vulkan;
        pc.gpu = Some(Gpu {
            name: "RTX 4070".into(),
            memory: 12 * GB,
        });
        assert_eq!(pc.model_budget(), 12 * GB - GB / 2);
        pc.backend = Backend::Cpu;
        pc.gpu = None;
        assert_eq!(pc.model_budget(), 32 * GB * 3 / 5);
        let small = Hardware {
            memory: 8 * GB,
            ..pc
        };
        assert_eq!(small.model_budget(), 4 * GB);
        let m4 = Hardware {
            memory: 16 << 30,
            ..mac(16)
        };
        assert_eq!(m4.summary(), "Apple M4 · 10 cores · 16 GB");
    }

    #[test]
    fn parses_system_output() {
        let gpu = nvidia_smi("NVIDIA GeForce RTX 3060, 12288\nNVIDIA RTX A6000, 49140\n").unwrap();
        assert_eq!(gpu.name, "NVIDIA RTX A6000");
        assert_eq!(gpu.memory, 49140 * 1024 * 1024);
        assert!(nvidia_smi("").is_none());
        assert_eq!(
            meminfo_total("MemTotal:       32768000 kB\nMemFree: 1 kB\n"),
            Some(32768000 * 1024)
        );
        assert_eq!(
            cpuinfo_model("processor\t: 0\nmodel name\t: AMD Ryzen 9 7950X\n").as_deref(),
            Some("AMD Ryzen 9 7950X")
        );
        let df = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/disk3s5 239362496 136877096 78822408 64% /\n";
        assert_eq!(df_available(df), Some(78822408 * 1024));
    }

    #[test]
    fn detects_this_machine() {
        let hw = detect();
        assert!(hw.memory > GB, "{hw:?}");
        assert!(hw.cores >= 1);
        assert!(hw.model_budget() > 0);
        assert!(free_space(&std::env::temp_dir()).is_some_and(|b| b > 0));
    }
}

//! Models Solder can install, and which of them suit this machine.
//!
//! Generation speed is bound by memory bandwidth: every token reads the
//! active weights once. Prompt processing is bound by compute, which grows
//! with the active parameter count. So one measured model predicts the rest:
//! the benchmark times the smallest model and scales by these two ratios.

use std::collections::HashMap;

use crate::{Speed, hardware::Hardware};

const GB: u64 = 1_000_000_000;
/// Compute buffers and the server itself, on top of weights and cache.
const OVERHEAD: u64 = GB / 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// Chat, edits and agent tasks: quality first, as long as it is usable.
    Chat,
    /// Inline completions: speed first.
    Completion,
}

impl Role {
    /// The slowest a model may be for this role, in tokens per second.
    pub fn floor(self) -> Speed {
        match self {
            Role::Chat => Speed {
                prompt: 150.,
                generate: 15.,
            },
            Role::Completion => Speed {
                prompt: 600.,
                generate: 40.,
            },
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Model {
    pub id: &'static str,
    pub name: &'static str,
    /// Hugging Face repository and file.
    pub repo: &'static str,
    pub file: &'static str,
    /// Approximate file size; the exact one is read before downloading.
    pub size: u64,
    /// Billions of parameters, and how many of them each token uses
    /// (fewer for mixture-of-experts models).
    pub params: f64,
    pub active: f64,
    /// Cache for the context Solder runs it with.
    pub cache: u64,
    pub context: u32,
    /// Higher is better at code and tool use, relative to the others here.
    pub rank: u8,
}

impl Model {
    /// Memory the running server needs.
    pub fn memory(&self) -> u64 {
        self.size + self.cache + OVERHEAD
    }

    /// Bytes of weights read per generated token.
    fn active_bytes(&self) -> f64 {
        self.size as f64 * self.active / self.params
    }

    /// Speed on this machine predicted from `measured` on `calibration`.
    pub fn predict(&self, calibration: &Model, measured: Speed) -> Speed {
        Speed {
            prompt: measured.prompt * calibration.active / self.active,
            generate: measured.generate * calibration.active_bytes() / self.active_bytes(),
        }
    }
}

/// Ordered from smallest to largest. The first is the benchmark's
/// calibration model: small enough to fetch quickly on any connection.
pub const MODELS: &[Model] = &[
    Model {
        id: "qwen3.5-0.8b",
        name: "Qwen3.5 0.8B",
        repo: "ggml-org/Qwen3.5-0.8B-GGUF",
        file: "Qwen3.5-0.8B-Q8_0.gguf",
        size: 833_592_096,
        params: 0.8,
        active: 0.8,
        cache: 400_000_000,
        context: 32768,
        rank: 10,
    },
    Model {
        id: "qwen3.5-4b",
        name: "Qwen3.5 4B",
        repo: "unsloth/Qwen3.5-4B-GGUF",
        file: "Qwen3.5-4B-Q4_K_M.gguf",
        size: 2_740_000_000,
        params: 4.0,
        active: 4.0,
        cache: 800_000_000,
        context: 32768,
        rank: 30,
    },
    Model {
        id: "qwen3.5-9b",
        name: "Qwen3.5 9B",
        repo: "unsloth/Qwen3.5-9B-GGUF",
        file: "Qwen3.5-9B-Q4_K_M.gguf",
        size: 5_680_522_464,
        params: 9.0,
        active: 9.0,
        cache: 1_000_000_000,
        context: 32768,
        rank: 50,
    },
    Model {
        id: "gpt-oss-20b",
        name: "gpt-oss 20B",
        repo: "ggml-org/gpt-oss-20b-GGUF",
        file: "gpt-oss-20b-MXFP4.gguf",
        size: 12_110_000_000,
        params: 21.0,
        active: 3.6,
        cache: 800_000_000,
        context: 32768,
        rank: 60,
    },
    Model {
        id: "qwen3.8-27b",
        name: "Qwen3.8 27B",
        repo: "unsloth/Qwen3.8-27B-GGUF",
        file: "Qwen3.8-27B-UD-Q4_K_M.gguf",
        size: 16_460_000_000,
        params: 27.0,
        active: 27.0,
        cache: 2_000_000_000,
        context: 32768,
        rank: 80,
    },
    Model {
        id: "qwen3.6-35b-a3b",
        name: "Qwen3.6 35B A3B",
        repo: "unsloth/Qwen3.6-35B-A3B-GGUF",
        file: "Qwen3.6-35B-A3B-UD-Q4_K_M.gguf",
        size: 22_130_000_000,
        params: 35.0,
        active: 3.0,
        cache: 700_000_000,
        context: 32768,
        rank: 75,
    },
    Model {
        id: "qwen3.8-27b-q8",
        name: "Qwen3.8 27B Q8",
        repo: "unsloth/Qwen3.8-27B-GGUF",
        file: "Qwen3.8-27B-Q8_0.gguf",
        size: 29_050_000_000,
        params: 27.0,
        active: 27.0,
        cache: 2_000_000_000,
        context: 32768,
        rank: 85,
    },
];

pub fn calibration() -> &'static Model {
    &MODELS[0]
}

pub fn find(id: &str) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.id == id)
}

/// One model as it would run here.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub model: &'static Model,
    /// Fits in the memory a model may use.
    pub fits: bool,
    /// Predicted, once the benchmark has run.
    pub speed: Option<Speed>,
    /// Roles it is recommended for.
    pub picks: Vec<Role>,
}

/// Every model with whether it fits, its speed and the recommended picks:
/// the best-ranked model fast enough for each role. A model measured after
/// its download (`verified`) is judged by that speed, not the prediction.
/// The completion model must fit next to the chat model, so both can stay
/// loaded.
pub fn recommend(
    hw: &Hardware,
    measured: Option<Speed>,
    verified: &HashMap<String, Speed>,
) -> Vec<Candidate> {
    let budget = hw.model_budget();
    let mut out: Vec<Candidate> = MODELS
        .iter()
        .map(|model| Candidate {
            model,
            fits: model.memory() <= budget,
            speed: verified
                .get(model.id)
                .copied()
                .or_else(|| measured.map(|m| model.predict(calibration(), m))),
            picks: Vec::new(),
        })
        .collect();
    if measured.is_none() {
        return out;
    }
    let best = |out: &[Candidate], role: Role, budget: u64| {
        out.iter()
            .enumerate()
            .filter(|(_, c)| c.model.memory() <= budget)
            .filter(|(_, c)| c.speed.is_some_and(|s| s.at_least(role.floor())))
            .max_by_key(|(_, c)| c.model.rank)
            .map(|(i, _)| i)
    };
    let chat = best(&out, Role::Chat, budget);
    let rest = budget.saturating_sub(chat.map_or(0, |i| out[i].model.memory()));
    let completion = best(&out, Role::Completion, rest)
        .filter(|&i| Some(i) != chat)
        .or(chat);
    if let Some(i) = chat {
        out[i].picks.push(Role::Chat);
    }
    if let Some(i) = completion {
        out[i].picks.push(Role::Completion);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::{Backend, Gpu};

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

    fn picked(c: &[Candidate], role: Role) -> Option<&'static str> {
        c.iter()
            .find(|c| c.picks.contains(&role))
            .map(|c| c.model.id)
    }

    #[test]
    fn catalog_is_ordered_and_unique() {
        assert!(MODELS.windows(2).all(|w| w[0].size < w[1].size));
        for (i, m) in MODELS.iter().enumerate() {
            assert!(MODELS[i + 1..].iter().all(|o| o.id != m.id), "{}", m.id);
            assert!(m.file.ends_with(".gguf"));
            assert!(m.active <= m.params);
        }
        assert_eq!(find("qwen3.5-9b").unwrap().name, "Qwen3.5 9B");
    }

    #[test]
    fn predicts_from_the_calibration_model() {
        let small = calibration();
        let measured = Speed {
            prompt: 2000.,
            generate: 100.,
        };
        assert_eq!(small.predict(small, measured), measured);
        let dense = find("qwen3.5-9b").unwrap();
        let s = dense.predict(small, measured);
        assert!((s.prompt - 2000. * 0.8 / 9.).abs() < 0.01);
        let ratio = small.size as f64 / dense.size as f64;
        assert!((s.generate - 100. * ratio).abs() < 0.01);
        // A mixture of experts reads only its active weights per token.
        let moe = find("qwen3.6-35b-a3b").unwrap();
        assert!(moe.predict(small, measured).generate > dense.predict(small, measured).generate);
    }

    #[test]
    fn fits_by_memory_before_measuring() {
        let none = HashMap::new();
        let c = recommend(&mac(16), None, &none);
        let fits: Vec<_> = c.iter().filter(|c| c.fits).map(|c| c.model.id).collect();
        assert_eq!(fits, ["qwen3.5-0.8b", "qwen3.5-4b", "qwen3.5-9b"]);
        assert!(c.iter().all(|c| c.picks.is_empty() && c.speed.is_none()));
    }

    #[test]
    fn picks_the_best_model_fast_enough() {
        let none = HashMap::new();
        // An M4 with 16 GB: the 9B is usable, nothing bigger fits.
        let m4 = Speed {
            prompt: 2400.,
            generate: 120.,
        };
        let c = recommend(&mac(16), Some(m4), &none);
        assert_eq!(picked(&c, Role::Chat), Some("qwen3.5-9b"));
        assert_eq!(picked(&c, Role::Completion), Some("qwen3.5-0.8b"));

        // 64 GB fits everything, but dense 27B models generate too slowly
        // on this bandwidth, so the mixture of experts wins.
        let c = recommend(&mac(64), Some(m4), &none);
        assert_eq!(picked(&c, Role::Chat), Some("qwen3.6-35b-a3b"));

        let older = Speed {
            prompt: 900.,
            generate: 60.,
        };
        let c = recommend(&mac(16), Some(older), &none);
        assert_eq!(picked(&c, Role::Chat), Some("qwen3.5-4b"));
        assert_eq!(picked(&c, Role::Completion), Some("qwen3.5-0.8b"));

        // Nothing is fast enough to complete: the chat model does both.
        let slow = Speed {
            prompt: 500.,
            generate: 45.,
        };
        let c = recommend(&mac(16), Some(slow), &none);
        assert_eq!(picked(&c, Role::Chat), Some("qwen3.5-0.8b"));
        assert_eq!(picked(&c, Role::Completion), Some("qwen3.5-0.8b"));

        // A model measured faster than predicted is judged by its measurement.
        let m4_measured = Speed {
            prompt: 1160.,
            generate: 82.,
        };
        let c = recommend(&mac(16), Some(m4_measured), &none);
        assert_eq!(picked(&c, Role::Chat), Some("qwen3.5-4b"));
        let verified = HashMap::from([(
            "qwen3.5-9b".to_string(),
            Speed {
                prompt: 260.,
                generate: 19.,
            },
        )]);
        let c = recommend(&mac(16), Some(m4_measured), &verified);
        assert_eq!(picked(&c, Role::Chat), Some("qwen3.5-9b"));

        // Too slow for anything: no picks.
        let c = recommend(
            &mac(16),
            Some(Speed {
                prompt: 50.,
                generate: 5.,
            }),
            &none,
        );
        assert!(c.iter().all(|c| c.picks.is_empty()));
    }
}

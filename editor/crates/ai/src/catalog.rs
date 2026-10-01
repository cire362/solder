//! Models Solder can install, models added by hand, and which of them suit
//! this machine.
//!
//! Generation speed is bound by memory bandwidth: every token reads the
//! active weights once. Prompt processing is bound by compute, which grows
//! with the active parameter count. So one measured model predicts the rest:
//! the benchmark times the smallest model and scales by these two ratios.

use std::{collections::HashMap, path::PathBuf, sync::OnceLock};

use crate::{Speed, gguf, hardware::Hardware};

const GB: u64 = 1_000_000_000;
/// Compute buffers and the server itself, on top of weights and cache.
const OVERHEAD: u64 = GB / 2;
/// The context Solder runs models with, when they support it.
pub const CONTEXT: u32 = 32768;

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

#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// A file in a Hugging Face repository, downloaded into Solder's models
    /// directory.
    Hub { repo: String, file: String },
    /// A file already on disk (added by hand or found in LM Studio's
    /// folder), used where it is and never deleted by Solder.
    File(PathBuf),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub source: Source,
    /// File size; the exact one is read again before downloading.
    pub size: u64,
    /// Billions of parameters, and how many of them each token uses
    /// (fewer for mixture-of-experts models).
    pub params: f64,
    pub active: f64,
    /// Cache for the context Solder runs it with.
    pub cache: u64,
    pub context: u32,
    /// Higher is better at code and tool use, relative to the others in the
    /// catalog. Models added by hand have none and are never recommended.
    pub rank: Option<u8>,
}

impl Model {
    /// Memory the running server needs.
    pub fn memory(&self) -> u64 {
        self.size + self.cache + OVERHEAD
    }

    /// Bytes of weights read per generated token.
    fn active_bytes(&self) -> f64 {
        self.size as f64 * self.active / self.params.max(f64::MIN_POSITIVE)
    }

    /// Speed on this machine predicted from `measured` on `calibration`.
    pub fn predict(&self, calibration: &Model, measured: Speed) -> Speed {
        Speed {
            prompt: measured.prompt * calibration.active / self.active,
            generate: measured.generate * calibration.active_bytes() / self.active_bytes(),
        }
    }

    pub fn is_custom(&self) -> bool {
        self.rank.is_none()
    }

    /// A model described by its file's header.
    pub fn from_header(info: &gguf::Info, source: Source, size: u64) -> Result<Self, String> {
        if let Some(parts) = info.split {
            return Err(format!(
                "This model is split into {parts} files; pick a single-file version"
            ));
        }
        let (id, file) = match &source {
            Source::Hub { repo, file } => (format!("hf:{repo}/{file}"), file.clone()),
            Source::File(path) => (
                format!("file:{}", path.display()),
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
        };
        let stem = file.trim_end_matches(".gguf");
        let context = info.context.min(CONTEXT);
        Ok(Self {
            id,
            // The file name says which quantization this is; the header's
            // name often does not.
            name: if stem.is_empty() {
                info.name
                    .clone()
                    .unwrap_or_else(|| info.architecture.clone())
            } else {
                stem.to_string()
            },
            source,
            size,
            params: info.params as f64 / 1e9,
            active: info.active as f64 / 1e9,
            cache: info.cache(context),
            context,
            rank: None,
        })
    }

    pub fn to_json(&self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "id": self.id,
            "name": self.name,
            "size": self.size,
            "params": self.params,
            "active": self.active,
            "cache": self.cache,
            "context": self.context,
        });
        match &self.source {
            Source::Hub { repo, file } => {
                value["repo"] = repo.clone().into();
                value["file"] = file.clone().into();
            }
            Source::File(path) => value["path"] = path.display().to_string().into(),
        }
        value
    }

    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        let source = match (
            value["repo"].as_str(),
            value["file"].as_str(),
            value["path"].as_str(),
        ) {
            (Some(repo), Some(file), _) => Source::Hub {
                repo: repo.into(),
                file: file.into(),
            },
            (_, _, Some(path)) => Source::File(path.into()),
            _ => return None,
        };
        Some(Self {
            id: value["id"].as_str()?.into(),
            name: value["name"].as_str()?.into(),
            source,
            size: value["size"].as_u64()?,
            params: value["params"].as_f64()?,
            active: value["active"].as_f64()?,
            cache: value["cache"].as_u64()?,
            context: value["context"].as_u64()? as u32,
            rank: None,
        })
    }
}

/// One catalog entry. Parameters and cache come from each file's header
/// (`gguf::read_remote`), the cache at `CONTEXT` tokens.
struct Entry {
    id: &'static str,
    name: &'static str,
    repo: &'static str,
    file: &'static str,
    size: u64,
    params: f64,
    active: f64,
    cache: u64,
    rank: u8,
}

/// Ordered from smallest to largest. The first is the benchmark's
/// calibration model: small enough to fetch quickly on any connection.
const ENTRIES: &[Entry] = &[
    Entry {
        id: "qwen3.5-0.8b",
        name: "Qwen3.5 0.8B",
        repo: "ggml-org/Qwen3.5-0.8B-GGUF",
        file: "Qwen3.5-0.8B-Q8_0.gguf",
        size: 833_592_096,
        params: 0.773,
        active: 0.773,
        cache: 402_653_184,
        rank: 10,
    },
    Entry {
        id: "qwen3.5-4b",
        name: "Qwen3.5 4B",
        repo: "unsloth/Qwen3.5-4B-GGUF",
        file: "Qwen3.5-4B-Q4_K_M.gguf",
        size: 2_740_000_000,
        params: 4.206,
        active: 4.206,
        cache: 1_073_741_824,
        rank: 30,
    },
    Entry {
        id: "gemma-4-e2b",
        name: "Gemma 4 E2B",
        repo: "ggml-org/gemma-4-E2B-it-GGUF",
        file: "gemma-4-E2B-it-Q4_0.gguf",
        size: 2_841_481_184,
        params: 4.629,
        active: 2.280,
        cache: 499_122_176,
        rank: 20,
    },
    Entry {
        id: "gemma-4-e4b",
        name: "Gemma 4 E4B",
        repo: "ggml-org/gemma-4-E4B-it-GGUF",
        file: "gemma-4-E4B-it-Q4_0.gguf",
        size: 4_590_807_392,
        params: 7.463,
        active: 4.644,
        cache: 1_012_924_416,
        rank: 35,
    },
    Entry {
        id: "qwen3.5-9b",
        name: "Qwen3.5 9B",
        repo: "unsloth/Qwen3.5-9B-GGUF",
        file: "Qwen3.5-9B-Q4_K_M.gguf",
        size: 5_680_522_464,
        params: 8.954,
        active: 7.937,
        cache: 1_073_741_824,
        rank: 50,
    },
    Entry {
        id: "gemma-4-12b",
        name: "Gemma 4 12B",
        repo: "unsloth/gemma-4-12b-it-GGUF",
        file: "gemma-4-12b-it-Q4_K_M.gguf",
        size: 7_121_861_440,
        params: 11.907,
        active: 11.907,
        cache: 1_207_959_552,
        rank: 52,
    },
    Entry {
        id: "gpt-oss-20b",
        name: "gpt-oss 20B",
        repo: "ggml-org/gpt-oss-20b-GGUF",
        file: "gpt-oss-20b-MXFP4.gguf",
        size: 12_110_000_000,
        params: 20.915,
        active: 3.608,
        cache: 1_610_612_736,
        rank: 60,
    },
    Entry {
        id: "gemma-4-26b-a4b",
        name: "Gemma 4 26B A4B",
        repo: "ggml-org/gemma-4-26B-A4B-it-GGUF",
        file: "gemma-4-26B-A4B-it-Q4_0.gguf",
        size: 14_618_145_824,
        params: 25.233,
        active: 3.823,
        cache: 1_090_519_040,
        rank: 66,
    },
    Entry {
        id: "qwen3.8-27b",
        name: "Qwen3.8 27B",
        repo: "unsloth/Qwen3.8-27B-GGUF",
        file: "Qwen3.8-27B-UD-Q4_K_M.gguf",
        size: 16_460_000_000,
        params: 27.321,
        active: 26.049,
        cache: 2_147_483_648,
        rank: 80,
    },
    Entry {
        id: "gemma-4-31b",
        name: "Gemma 4 31B",
        repo: "unsloth/gemma-4-31B-it-qat-GGUF",
        file: "gemma-4-31B-it-qat-UD-Q4_K_XL.gguf",
        size: 17_287_670_048,
        params: 30.697,
        active: 30.697,
        cache: 4_362_076_160,
        rank: 76,
    },
    Entry {
        id: "qwen3-coder-30b-a3b",
        name: "Qwen3 Coder 30B A3B",
        repo: "unsloth/Qwen3-Coder-30B-A3B-Instruct-GGUF",
        file: "Qwen3-Coder-30B-A3B-Instruct-Q4_K_M.gguf",
        size: 18_556_689_568,
        params: 30.532,
        active: 3.042,
        cache: 3_221_225_472,
        rank: 70,
    },
    Entry {
        id: "nemotron-3.5-30b-a3b",
        name: "Nemotron 3.5 Lightning 30B A3B",
        repo: "ggml-org/NVIDIA-Nemotron-3.5-Lightning-30B-A3B-GGUF",
        file: "NVIDIA-Nemotron-3.5-Lightning-30B-A3B-Q4_0.gguf",
        size: 18_898_091_584,
        params: 31.578,
        active: 3.228,
        cache: 201_326_592,
        rank: 68,
    },
    Entry {
        id: "qwen3.6-35b-a3b",
        name: "Qwen3.6 35B A3B",
        repo: "unsloth/Qwen3.6-35B-A3B-GGUF",
        file: "Qwen3.6-35B-A3B-UD-Q4_K_M.gguf",
        size: 22_130_000_000,
        params: 34.661,
        active: 2.946,
        cache: 671_088_640,
        rank: 75,
    },
    Entry {
        id: "qwen3.8-27b-q8",
        name: "Qwen3.8 27B Q8",
        repo: "unsloth/Qwen3.8-27B-GGUF",
        file: "Qwen3.8-27B-Q8_0.gguf",
        size: 29_050_000_000,
        params: 27.321,
        active: 26.049,
        cache: 2_147_483_648,
        rank: 85,
    },
    Entry {
        id: "gpt-oss-120b",
        name: "gpt-oss 120B",
        repo: "ggml-org/gpt-oss-120b-GGUF",
        file: "gpt-oss-120b-MXFP4.gguf",
        size: 63_387_346_208,
        params: 116.829,
        active: 5.133,
        cache: 2_415_919_104,
        rank: 88,
    },
];

/// The catalog, smallest first.
pub fn models() -> &'static [Model] {
    static MODELS: OnceLock<Vec<Model>> = OnceLock::new();
    MODELS.get_or_init(|| {
        ENTRIES
            .iter()
            .map(|e| Model {
                id: e.id.into(),
                name: e.name.into(),
                source: Source::Hub {
                    repo: e.repo.into(),
                    file: e.file.into(),
                },
                size: e.size,
                params: e.params,
                active: e.active,
                cache: e.cache,
                context: CONTEXT,
                rank: Some(e.rank),
            })
            .collect()
    })
}

pub fn calibration() -> &'static Model {
    &models()[0]
}

pub fn find(id: &str) -> Option<&'static Model> {
    models().iter().find(|m| m.id == id)
}

/// One model as it would run here.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub model: Model,
    /// Fits in the memory a model may use.
    pub fits: bool,
    /// Measured after its download, or predicted once the benchmark ran.
    pub speed: Option<Speed>,
    pub measured: bool,
    /// Roles it is recommended for.
    pub picks: Vec<Role>,
}

/// Every model with whether it fits, its speed and the recommended picks:
/// the best-ranked catalog model fast enough for each role. A model measured
/// after its download (`verified`) is judged by that speed, not the
/// prediction. The completion model must fit next to the chat model, so both
/// can stay loaded.
pub fn recommend(
    hw: &Hardware,
    measured: Option<Speed>,
    verified: &HashMap<String, Speed>,
    models: &[Model],
) -> Vec<Candidate> {
    let budget = hw.model_budget();
    let mut out: Vec<Candidate> = models
        .iter()
        .map(|model| {
            let real = verified.get(&model.id).copied();
            Candidate {
                model: model.clone(),
                fits: model.memory() <= budget,
                speed: real.or_else(|| measured.map(|m| model.predict(calibration(), m))),
                measured: real.is_some(),
                picks: Vec::new(),
            }
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
            .filter_map(|(i, c)| Some((i, c.model.rank?)))
            .max_by_key(|&(_, rank)| rank)
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

    fn picked(c: &[Candidate], role: Role) -> Option<&str> {
        c.iter()
            .find(|c| c.picks.contains(&role))
            .map(|c| c.model.id.as_str())
    }

    fn run(memory_gb: u64, prompt: f64, generate: f64) -> Vec<Candidate> {
        recommend(
            &mac(memory_gb),
            Some(Speed { prompt, generate }),
            &HashMap::new(),
            models(),
        )
    }

    #[test]
    fn catalog_is_ordered_and_unique() {
        let models = models();
        assert!(models.windows(2).all(|w| w[0].size < w[1].size));
        for (i, m) in models.iter().enumerate() {
            assert!(models[i + 1..].iter().all(|o| o.id != m.id), "{}", m.id);
            assert!(matches!(&m.source, Source::Hub { file, .. } if file.ends_with(".gguf")));
            assert!(m.active <= m.params && m.rank.is_some());
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
        assert!((s.prompt - 2000. * small.active / dense.active).abs() < 0.01);
        // A mixture of experts reads only its active weights per token.
        let moe = find("qwen3.6-35b-a3b").unwrap();
        assert!(moe.predict(small, measured).generate > dense.predict(small, measured).generate);
    }

    #[test]
    fn fits_by_memory_before_measuring() {
        let c = recommend(&mac(16), None, &HashMap::new(), models());
        let fits: Vec<_> = c
            .iter()
            .filter(|c| c.fits)
            .map(|c| c.model.id.as_str())
            .collect();
        assert_eq!(
            fits,
            [
                "qwen3.5-0.8b",
                "qwen3.5-4b",
                "gemma-4-e2b",
                "gemma-4-e4b",
                "qwen3.5-9b",
                "gemma-4-12b"
            ]
        );
        assert!(c.iter().all(|c| c.picks.is_empty() && c.speed.is_none()));
    }

    #[test]
    fn picks_the_best_model_fast_enough() {
        // An M4 with 16 GB, as measured on the 0.8B model.
        let c = run(16, 1165., 83.);
        assert_eq!(picked(&c, Role::Chat), Some("gemma-4-e4b"));
        assert_eq!(picked(&c, Role::Completion), Some("qwen3.5-0.8b"));

        // 64 GB fits much more; dense 27B models write too slowly on this
        // bandwidth, so a mixture of experts wins.
        let c = run(64, 2400., 120.);
        assert_eq!(picked(&c, Role::Chat), Some("qwen3.6-35b-a3b"));
        // 128 GB fits gpt-oss 120B, which reads few weights per token.
        let c = run(128, 2400., 120.);
        assert_eq!(picked(&c, Role::Chat), Some("gpt-oss-120b"));

        // A slow machine: a model with few active parameters reads fast
        // enough to chat, nothing completes fast enough, so it does both.
        let c = run(16, 500., 45.);
        assert_eq!(picked(&c, Role::Chat), Some("gemma-4-e2b"));
        assert_eq!(picked(&c, Role::Completion), Some("gemma-4-e2b"));

        // Too slow for anything: no picks.
        assert!(run(16, 50., 5.).iter().all(|c| c.picks.is_empty()));
    }

    #[test]
    fn measured_speed_beats_the_prediction() {
        let verified = HashMap::from([(
            "qwen3.5-9b".to_string(),
            Speed {
                prompt: 260.,
                generate: 19.,
            },
        )]);
        let c = recommend(
            &mac(16),
            Some(Speed {
                prompt: 1165.,
                generate: 83.,
            }),
            &verified,
            models(),
        );
        assert_eq!(picked(&c, Role::Chat), Some("qwen3.5-9b"));
        assert!(
            c.iter()
                .find(|c| c.model.id == "qwen3.5-9b")
                .unwrap()
                .measured
        );
    }

    #[test]
    fn custom_models_are_listed_but_never_picked() {
        let info = gguf::Info {
            name: Some("Mystery".into()),
            architecture: "llama".into(),
            params: 1_000_000_000,
            active: 1_000_000_000,
            context: 131072,
            cache_full: 1000,
            cache_window: 0,
            window: 0,
            split: None,
        };
        let model = Model::from_header(
            &info,
            Source::Hub {
                repo: "me/mystery-GGUF".into(),
                file: "mystery-Q4_K_M.gguf".into(),
            },
            600_000_000,
        )
        .unwrap();
        assert_eq!(model.id, "hf:me/mystery-GGUF/mystery-Q4_K_M.gguf");
        assert_eq!(model.name, "mystery-Q4_K_M");
        assert_eq!(model.context, CONTEXT);
        assert_eq!(model.cache, 1000 * CONTEXT as u64);
        assert!(model.is_custom());
        assert_eq!(Model::from_json(&model.to_json()), Some(model.clone()));
        let mut all = models().to_vec();
        all.push(model);
        let c = recommend(
            &mac(16),
            Some(Speed {
                prompt: 99999.,
                generate: 9999.,
            }),
            &HashMap::new(),
            &all,
        );
        let last = c.last().unwrap();
        assert!(last.picks.is_empty() && last.fits);

        let split = gguf::Info {
            split: Some(3),
            ..info
        };
        let err = Model::from_header(&split, Source::File("/m.gguf".into()), 1).unwrap_err();
        assert!(err.contains("split into 3 files"), "{err}");
    }
}

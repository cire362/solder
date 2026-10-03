//! The plugin the host's tests run. A command's id says what to do, with an
//! argument after a colon; the outcome goes to the status bar.

use solder_plugin::{Event, Plugin, status};

#[derive(Default)]
struct Probe {
    events: u32,
}

/// Work the editor can count: `rounds` passes of a small loop, kept from
/// being optimized away.
fn spin(rounds: u64) -> u64 {
    let mut x = 0u64;
    for i in 0..std::hint::black_box(rounds) {
        x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(i));
    }
    x
}

fn show<T>(result: Result<T, String>, ok: impl FnOnce(T) -> String) {
    status(match result {
        Ok(value) => ok(value),
        Err(e) => format!("refused: {e}"),
    });
}

impl Plugin for Probe {
    fn event(&mut self, event: Event) {
        self.events += 1;
        let Event::Command { id } = event else {
            match event {
                Event::Activate => status("active"),
                Event::Open { path, .. } => status(format!("open {path}")),
                Event::Save { path } => status(format!("save {path}")),
                // Typing: enough work to go over a small budget.
                Event::Change { path } => {
                    spin(200_000);
                    status(format!("change {path}"));
                }
                Event::Command { .. } => {}
            }
            return;
        };
        let (name, arg) = id.split_once(':').unwrap_or((&id, ""));
        match name {
            "count" => status(format!("{} events", self.events)),
            "editor" => show(solder_plugin::editor(), |e| {
                format!(
                    "{} {}..{} {}",
                    e.path.unwrap_or_default(),
                    e.selection_start,
                    e.selection_end,
                    e.text.len()
                )
            }),
            "shout" => {
                if let Ok(e) = solder_plugin::editor() {
                    let path = e.path.unwrap_or_default();
                    let upper = e.text.to_uppercase();
                    show(solder_plugin::edit(path, 0..e.text.len(), upper), |_| {
                        "edited".into()
                    });
                }
            }
            "read" => show(solder_plugin::read_file(arg), |text| text),
            "get" => show(solder_plugin::get(arg), |r| {
                format!("{} {}", r.status, r.body)
            }),
            "spin" => {
                spin(arg.parse().unwrap_or(0));
                status("spun");
            }
            "forever" => loop {
                spin(1_000_000);
            },
            "alloc" => {
                let bytes = vec![1u8; arg.parse().unwrap_or(0)];
                status(format!("allocated {}", bytes.len()));
            }
            "panic" => panic!("probe asked to panic"),
            "log" => solder_plugin::log(arg),
            _ => status(format!("unknown {name}")),
        }
    }
}

solder_plugin::register!(Probe);

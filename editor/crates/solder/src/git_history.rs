//! History is read on demand. The graph is Git's, so merges and renamed files
//! have the same ancestry here as they do at the command line.

use crate::git::{Repo, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub id: String,
    pub author: String,
    pub date: String,
    pub refs: String,
    pub subject: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub graph: String,
    pub commit: Option<Commit>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blame {
    pub id: String,
    pub author: String,
    pub summary: String,
}

impl Blame {
    pub fn label(&self) -> String {
        if self.id.bytes().all(|b| b == b'0') {
            "Not committed".into()
        } else {
            format!(
                "{} {}",
                &self.id[..7.min(self.id.len())],
                self.author.chars().take(12).collect::<String>()
            )
        }
    }
}

impl Repo {
    pub fn history(&self, file: Option<&str>, limit: usize) -> Result<Vec<Row>> {
        let count = format!("--max-count={limit}");
        let mut args = vec![
            "log",
            "--graph",
            "--no-color",
            "--topo-order",
            &count,
            "--format=%x00%H%x00%an%x00%aI%x00%D%x00%s%x00",
        ];
        if file.is_some() {
            args.extend(["--follow", "HEAD"]);
        } else {
            args.push("--all");
        }
        args.push("--");
        if let Some(file) = file {
            args.push(file);
        }
        self.run(&args)
            .map(|out| history_rows(&String::from_utf8_lossy(&out)))
    }

    pub fn commit_patch(&self, id: &str, file: Option<&str>) -> Result<String> {
        let mut args = vec![
            "show",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--format=fuller",
            "--stat",
            "--patch",
            id,
            "--",
        ];
        if let Some(file) = file {
            args.push(file);
        }
        self.run(&args).map(|out| {
            let mut text = String::from_utf8_lossy(&out).into_owned();
            if text.len() > 500_000 {
                let mut end = 500_000;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
                text.push_str("\n[The rest of this patch is not shown.]\n");
            }
            text
        })
    }

    /// Includes the current buffer, so unsaved lines are never blamed on
    /// an author who did not write them.
    pub fn blame(&self, file: &str, text: &str) -> Result<Vec<Blame>> {
        self.run_with_input(
            &["blame", "--line-porcelain", "--contents", "-", "--", file],
            Some(text.as_bytes()),
        )
        .map(|out| blame_lines(&String::from_utf8_lossy(&out)))
    }
}

fn history_rows(out: &str) -> Vec<Row> {
    out.lines()
        .filter_map(|line| {
            let mut fields = line.split('\0');
            let graph = fields.next()?.to_string();
            let commit = fields.next().filter(|id| !id.is_empty()).and_then(|id| {
                Some(Commit {
                    id: id.into(),
                    author: fields.next()?.into(),
                    date: fields.next()?.into(),
                    refs: fields.next()?.into(),
                    subject: fields.next()?.into(),
                })
            });
            (!graph.trim().is_empty() || commit.is_some()).then_some(Row { graph, commit })
        })
        .collect()
}

fn blame_lines(out: &str) -> Vec<Blame> {
    let mut rows = Vec::new();
    let mut current = Blame {
        id: String::new(),
        author: String::new(),
        summary: String::new(),
    };
    for line in out.lines() {
        if line.starts_with('\t') {
            rows.push(current.clone());
        } else if let Some(author) = line.strip_prefix("author ") {
            current.author = author.into();
        } else if let Some(summary) = line.strip_prefix("summary ") {
            current.summary = summary.into();
        } else if let Some(id) = line.split_whitespace().next()
            && (id.len() == 40 || id.len() == 64)
            && id.bytes().all(|b| b.is_ascii_hexdigit())
        {
            current = Blame {
                id: id.into(),
                author: String::new(),
                summary: String::new(),
            };
        }
    }
    rows
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::process::Command;

    pub struct Fixture(pub Repo);
    impl Fixture {
        pub fn new(name: &str) -> Self {
            let workdir = db::testing::dir(name);
            let f = Self(Repo { workdir });
            f.git(&["init", "-q", "-b", "main"]);
            f.git(&["config", "user.name", "Test Author"]);
            f.git(&["config", "user.email", "test@example.com"]);
            f
        }
        pub fn git(&self, args: &[&str]) -> String {
            let out = Command::new("git")
                .arg("-C")
                .arg(&self.0.workdir)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).into_owned()
        }
        pub fn write(&self, file: &str, text: &str) {
            std::fs::write(self.0.workdir.join(file), text).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0.workdir).unwrap();
        }
    }

    #[test]
    fn history_follows_renames_and_graph_keeps_merges() {
        let f = Fixture::new("history");
        f.write("before.txt", "one\ntwo\n");
        f.0.stage(&["before.txt"]).unwrap();
        f.0.commit("initial", false).unwrap();
        f.git(&["switch", "-q", "-c", "side"]);
        f.write("side.txt", "side");
        f.0.stage(&["side.txt"]).unwrap();
        f.0.commit("side", false).unwrap();
        f.git(&["switch", "-q", "main"]);
        f.git(&["mv", "before.txt", "after name.txt"]);
        f.0.commit("rename", false).unwrap();
        f.git(&["merge", "-q", "--no-ff", "side", "-m", "merge"]);
        let graph = f.0.history(None, 200).unwrap();
        assert_eq!(graph.iter().filter(|r| r.commit.is_some()).count(), 4);
        assert!(
            graph
                .iter()
                .any(|r| r.graph.contains('\\') || r.graph.contains('/'))
        );
        let file = f.0.history(Some("after name.txt"), 200).unwrap();
        let titles: Vec<_> = file
            .iter()
            .filter_map(|r| r.commit.as_ref().map(|c| c.subject.as_str()))
            .collect();
        assert_eq!(titles, ["rename", "initial"]);
        let first = file.last().unwrap().commit.as_ref().unwrap();
        let patch = f.0.commit_patch(&first.id, None).unwrap();
        assert!(patch.contains("+one"));
        let blame = f.0.blame("after name.txt", "one\nchanged\ntwo\n").unwrap();
        assert_eq!(blame.len(), 3);
        assert_eq!(blame[0].author, "Test Author");
        assert_eq!(blame[1].label(), "Not committed");
        assert_eq!(blame[2].id, first.id);
        assert!(f.0.blame("untracked.txt", "x").is_err());
    }
}

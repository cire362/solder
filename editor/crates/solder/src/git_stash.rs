use crate::git::{GitError, Repo, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stash {
    pub id: String,
    pub reference: String,
    pub subject: String,
}

impl Repo {
    pub fn stashes(&self) -> Result<Vec<Stash>> {
        self.run(&["stash", "list", "--format=%H%x00%gd%x00%gs%x00"])
            .map(|out| {
                String::from_utf8_lossy(&out)
                    .lines()
                    .filter_map(|line| {
                        let mut fields = line.split('\0');
                        Some(Stash {
                            id: fields.next()?.into(),
                            reference: fields.next()?.into(),
                            subject: fields.next()?.into(),
                        })
                    })
                    .collect()
            })
    }
    pub fn stash(&self) -> Result<()> {
        self.run(&["stash", "push", "--include-untracked", "-m", "Solder stash"])
            .map(drop)
    }
    /// Stash numbers change whenever a newer one is made or removed.
    /// Resolve the selected object again instead of applying another stash.
    pub fn change_stash(&self, id: &str, operation: &str) -> Result<()> {
        let stash = self
            .stashes()?
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(|| GitError("This stash is no longer in the list. Refresh it.".into()))?;
        match operation {
            "apply" | "pop" => self
                .run(&["stash", operation, "--index", &stash.reference])
                .map(drop),
            "drop" => self.run(&["stash", "drop", &stash.reference]).map(drop),
            _ => Err(GitError("Unknown stash action.".into())),
        }
    }
    pub fn fetch(&self) -> Result<()> {
        self.run(&["fetch", "--all"]).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use crate::git_history::tests::Fixture;

    #[test]
    fn stashes_keep_staged_unstaged_and_untracked_files_and_use_object_ids() {
        let f = Fixture::new("stash-roundtrip");
        f.write("a.txt", "base");
        f.0.stage(&["a.txt"]).unwrap();
        f.0.commit("initial", false).unwrap();
        f.write("a.txt", "staged");
        f.0.stage(&["a.txt"]).unwrap();
        f.write("a.txt", "unstaged");
        f.write("untracked.txt", "untracked");
        f.0.stash().unwrap();
        assert!(f.0.status().unwrap().files.is_empty());
        let first = f.0.stashes().unwrap().remove(0);
        f.write("a.txt", "second");
        f.0.stash().unwrap();
        let second = f.0.stashes().unwrap().remove(0);
        assert_ne!(first.id, second.id);
        f.0.change_stash(&first.id, "apply").unwrap();
        assert_eq!(f.0.index_text("a.txt").as_deref(), Some("staged"));
        assert_eq!(
            std::fs::read_to_string(f.0.workdir.join("a.txt")).unwrap(),
            "unstaged"
        );
        assert!(f.0.workdir.join("untracked.txt").exists());
        // A conflict must leave the stash available.
        assert!(f.0.change_stash(&second.id, "pop").is_err());
        assert_eq!(f.0.stashes().unwrap().len(), 2);
        f.0.change_stash(&first.id, "drop").unwrap();
        assert_eq!(f.0.stashes().unwrap()[0].id, second.id);
        assert!(f.0.change_stash(&first.id, "drop").is_err());
        f.git(&["reset", "--hard", "-q"]);
        f.git(&["clean", "-f", "-q"]);
        f.0.change_stash(&second.id, "pop").unwrap();
        assert!(f.0.stashes().unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(f.0.workdir.join("a.txt")).unwrap(),
            "second"
        );
    }
    #[test]
    fn fetch_reads_a_local_remote_without_touching_working_files() {
        let remote = Fixture::new("fetch-remote");
        remote.write("a.txt", "base");
        remote.0.stage(&["a.txt"]).unwrap();
        remote.0.commit("initial", false).unwrap();
        let f = Fixture::new("fetch-local");
        f.git(&[
            "remote",
            "add",
            "origin",
            remote.0.workdir.to_str().unwrap(),
        ]);
        f.0.fetch().unwrap();
        assert_eq!(f.0.show("origin/main:a.txt").as_deref(), Some("base"));
        remote.write("a.txt", "new");
        remote.0.stage(&["a.txt"]).unwrap();
        remote.0.commit("new", false).unwrap();
        f.write("mine.txt", "mine");
        f.0.fetch().unwrap();
        assert_eq!(f.0.show("origin/main:a.txt").as_deref(), Some("new"));
        assert_eq!(
            std::fs::read_to_string(f.0.workdir.join("mine.txt")).unwrap(),
            "mine"
        );
    }
}

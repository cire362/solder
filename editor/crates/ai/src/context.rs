use std::path::Path;

pub fn allows_file(path: &Path) -> bool {
    path.components().all(|component| {
        component
            .as_os_str()
            .as_encoded_bytes()
            .get(..4)
            .is_none_or(|prefix| !prefix.eq_ignore_ascii_case(b".env"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excludes_env_files_and_directories_at_any_depth() {
        for path in [
            ".env",
            ".env.local",
            ".env.production",
            ".env.example",
            ".envrc",
            ".ENV",
            "config/.Env.local",
            "/project/apps/web/.env.production",
            "/project/.env.d/settings.json",
        ] {
            assert!(!allows_file(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn allows_source_files_and_other_hidden_paths() {
        for path in [
            "src/main.rs",
            "config/environment.ts",
            "/project/.github/workflows/ci.yml",
            "/project/.solder/services.json",
        ] {
            assert!(allows_file(Path::new(path)), "{path}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn excludes_env_names_that_are_not_utf8() {
        use std::os::unix::ffi::OsStrExt;

        let path = Path::new(std::ffi::OsStr::from_bytes(b"config/.env.\xff"));
        assert!(!allows_file(path));
    }
}

//! The world of API 0.0.4: every function belongs to the world itself, and a
//! server is asked about by its name and language.

use super::*;

mod bindings {
    wasmtime::component::bindgen!({
        path: "wit/since_v0.0.4",
        world: "extension",
        with: {
            "worktree": super::super::Worktree,
        },
    });
}

use bindings::zed::extension as api;

// The two interfaces are here for their types, but a component may call
// their functions through them as well as through the world.
impl api::platform::Host for State {
    fn current_platform(&mut self) -> (api::platform::Os, api::platform::Architecture) {
        platform!(api)
    }
}

impl api::github::Host for State {
    tools!(@latest api);
}

worktree!(bindings);

impl bindings::ExtensionImports for State {
    fn current_platform(&mut self) -> (api::platform::Os, api::platform::Architecture) {
        platform!(api)
    }

    fn node_binary_path(&mut self) -> Result<String, String> {
        self.world.node()
    }

    fn npm_package_latest_version(&mut self, package: String) -> Result<String, String> {
        self.world.npm_latest(&package)
    }

    fn npm_package_installed_version(&mut self, package: String) -> Result<Option<String>, String> {
        self.npm_installed(&package)
    }

    fn npm_install_package(&mut self, package: String, version: String) -> Result<(), String> {
        self.world.npm_install(&self.work_dir, &package, &version)
    }

    fn latest_github_release(
        &mut self,
        repo: String,
        options: api::github::GithubReleaseOptions,
    ) -> Result<api::github::GithubRelease, String> {
        Ok(github_release!(
            api,
            self.latest_release(&repo, options.require_assets, options.pre_release)?
        ))
    }

    fn download_file(
        &mut self,
        url: String,
        path: String,
        kind: bindings::DownloadedFileType,
    ) -> Result<(), String> {
        self.download(&url, &path, file_kind!(bindings, kind))
    }

    fn make_file_executable(&mut self, path: String) -> Result<(), String> {
        self.make_executable(&path)
    }

    fn set_language_server_installation_status(
        &mut self,
        server: String,
        status: bindings::LanguageServerInstallationStatus,
    ) {
        use bindings::LanguageServerInstallationStatus as S;
        self.world.status(
            &server,
            match status {
                S::None => Status::Ready,
                S::Downloading => Status::Downloading,
                S::CheckingForUpdate => Status::CheckingForUpdate,
                S::Failed(why) => Status::Failed(why),
            },
        );
    }
}

start!(bindings, by_config);

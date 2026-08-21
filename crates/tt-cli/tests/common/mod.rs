//! Shared test-isolation helpers for `tt-cli` integration tests.
//!
//! `tt` shells out to `jj` for project identity extraction
//! (`commands::ingest::get_git_identity`), and the tmux hook it is normally
//! driven by can pass any real directory as `--cwd`. A test that spawns the
//! real `tt` binary (or, in-process, anything that itself spawns `jj`/`git`)
//! without isolating `$HOME` lets that subprocess see — and, on some
//! git/jj versions, write — the developer's real `~/.gitconfig`. Every test
//! fixture is scratch space; nothing spawned against it should ever be able
//! to reach the real machine's identity.
//!
//! Every integration test that spawns `tt` (or anything `tt` may spawn)
//! must route its [`std::process::Command`] through [`CommandExt::sandboxed_home`].

use std::path::Path;
use std::process::Command;

pub trait CommandExt {
    /// Points this command at an isolated `$HOME` so neither it nor anything
    /// it spawns can see or touch the developer's real machine config.
    ///
    /// `home` is a scratch directory scoped to the calling test; it does not
    /// need to exist yet.
    fn sandboxed_home(&mut self, home: &Path) -> &mut Self;
}

impl CommandExt for Command {
    fn sandboxed_home(&mut self, home: &Path) -> &mut Self {
        self.env("HOME", home)
            // git >= 2.32: pins the global config file itself, so a write
            // lands in scratch space even if something resolves identity
            // before `HOME` takes effect for it.
            .env("GIT_CONFIG_GLOBAL", home.join("sandbox.gitconfig"))
            // Belt and suspenders: drop `/etc/gitconfig` from the search
            // path too.
            .env("GIT_CONFIG_NOSYSTEM", "1")
    }
}

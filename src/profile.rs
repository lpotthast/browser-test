//! Chrome profiles of the sessions of a run.
//!
//! Without `--user-data-dir`, `ChromeDriver` creates every session's profile in an
//! `org.chromium.Chromium.scoped_dir.*` temporary directory. It removes the profile only when the
//! session quits cleanly, so every killed session leaves tens of megabytes behind. The runner
//! therefore manages profiles itself:
//!
//! ```text
//! <ChromeProfilesDir>/
//!     run-<pid>-<nanos>-<n>/    one per run, removed when the run ends
//!         run.lock              locked while the run lives
//!         session-0/            one per session, removed when the session ends
//!         session-1/
//! ```
//!
//! # Cleaning up after killed runs
//!
//! A killed run cannot remove its directory, so a starting run removes the directories of ended
//! runs. Their locks tell them apart:
//!
//! - The operating system releases a lock when its process exits, however it exits. A run
//!   directory whose lock nobody holds belongs to an ended run, and is removed.
//! - A run directory without a lock file is removed as well. A run creates its lock file before
//!   it names the directory `run-*`, and the file only goes away while the directory is removed.
//!   So such a directory is left over from an interrupted removal, or was recreated by a Chrome
//!   that outlived its run.
//! - A lock file that cannot be opened or locked counts as held, and its directory stays.
//!
//! Entries not named `run-*` are never removed.

use std::{
    fs::{self, File, TryLockError},
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use rootcause::{Report, prelude::ResultExt as _};
use thirtyfour::{
    BrowserCapabilitiesHelper as _, ChromeCapabilities, ChromiumLikeCapabilities,
    error::{WebDriverError, WebDriverResult},
};

use crate::BrowserTestError;

/// Name of the default [`ChromeProfilesDir`] in the system's temporary directory, followed by the
/// user id on Unix.
const DEFAULT_DIR_NAME: &str = "browser-test-profiles";

/// Prefix of a run's directory. Only directories named like this are ever removed by a sweep.
const RUN_DIR_PREFIX: &str = "run-";

/// Prefix of a run's directory before it holds its lock.
const STAGING_DIR_PREFIX: &str = ".staging-";

const LOCK_FILE_NAME: &str = "run.lock";

const USER_DATA_DIR_SWITCH: &str = "user-data-dir";

/// Tells apart the runs one process starts.
static NEXT_RUN: AtomicUsize = AtomicUsize::new(0);

/// Where runs keep the Chrome profiles of their sessions, see
/// [`BrowserTestRunner::with_chrome_profiles_dir`](crate::BrowserTestRunner::with_chrome_profiles_dir).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChromeProfilesDir {
    path: PathBuf,
}

impl ChromeProfilesDir {
    /// Keep profiles in `path`. A relative path is resolved against the current directory when a
    /// run starts.
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Keep profiles in `browser-test-profiles-<uid>` (Unix) or `browser-test-profiles` in
    /// [`std::env::temp_dir`]. The runner's default. On Unix, users share the temporary directory
    /// (`/tmp`), and one user's directory, accessible by its owner only, would lock out the others.
    pub(crate) fn in_temp_dir() -> Self {
        #[cfg(unix)]
        let name = format!("{DEFAULT_DIR_NAME}-{}", nix::unistd::geteuid());
        #[cfg(not(unix))]
        let name = DEFAULT_DIR_NAME;
        Self::new(std::env::temp_dir().join(name))
    }

    /// The path as configured, which may be relative.
    #[cfg(test)]
    fn path(&self) -> &Path {
        &self.path
    }

    /// Create the directory unless it exists, check that it is usable, and return its absolute
    /// path. Chrome does not run in the current directory, so it needs an absolute one.
    fn resolve(&self) -> io::Result<PathBuf> {
        let path = std::path::absolute(&self.path)?;
        if path.to_str().is_none() {
            return Err(io::Error::other(format!(
                "{} is not valid UTF-8, as Chrome's command line needs it",
                path.display()
            )));
        }
        private_dir_builder().recursive(true).create(&path)?;
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_symlink() {
            return Err(io::Error::other(format!(
                "{} is a symlink. Configure the directory it points to instead",
                path.display()
            )));
        }
        if !metadata.is_dir() {
            return Err(io::Error::other(format!(
                "{} is not a directory",
                path.display()
            )));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
            if metadata.uid() != nix::unistd::geteuid().as_raw() {
                return Err(io::Error::other(format!(
                    "{} belongs to another user. Choose another directory",
                    path.display()
                )));
            }
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(io::Error::other(format!(
                    "{} is accessible by other users. Restrict it with `chmod 700`",
                    path.display()
                )));
            }
        }
        Ok(path)
    }
}

/// The directory of one run, holding the profiles of its sessions.
#[derive(Debug)]
pub(crate) struct RunProfiles {
    // Fields drop in declaration order. The lock goes first, because Windows cannot remove a
    // directory holding an open file.
    lock: RunLock,
    dir: OwnedDir,
    next_session: AtomicUsize,
}

impl RunProfiles {
    /// Remove the directories of ended runs from `profiles_dir`, then create the directory of
    /// this run in it.
    ///
    /// The file system work runs on a blocking thread, keeping the runtime responsive while the
    /// profiles of ended runs are removed.
    pub(crate) async fn create(
        profiles_dir: &ChromeProfilesDir,
    ) -> Result<Self, Report<BrowserTestError>> {
        let profiles_dir = profiles_dir.clone();
        let result =
            match tokio::task::spawn_blocking(move || Self::create_blocking(&profiles_dir)).await {
                Ok(result) => result,
                Err(join_error) => Err(io::Error::other(join_error)),
            };
        result.context(BrowserTestError::CreateChromeProfiles)
    }

    fn create_blocking(profiles_dir: &ChromeProfilesDir) -> io::Result<Self> {
        let base = profiles_dir.resolve()?;
        Self::remove_abandoned(&base);

        let name = Self::unique_name();
        // The directory gets its run name only once it holds its lock, so that a concurrent sweep
        // never sees it unlocked. Should a step fail, dropping `dir` removes it.
        let mut dir = OwnedDir::create(base.join(format!("{STAGING_DIR_PREFIX}{name}")))?;
        let lock = RunLock::acquire_new(&dir.path().join(LOCK_FILE_NAME))?;
        dir.rename(base.join(format!("{RUN_DIR_PREFIX}{name}")))?;
        Ok(Self {
            lock,
            dir,
            next_session: AtomicUsize::new(0),
        })
    }

    /// A name no other run uses: the process id, the time, and the run's number in the process.
    fn unique_name() -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since_epoch| since_epoch.as_nanos());
        let run = NEXT_RUN.fetch_add(1, Ordering::Relaxed);
        format!("{}-{nanos}-{run}", std::process::id())
    }

    /// Remove every run directory in `base` that belongs to an ended run. See the module docs.
    fn remove_abandoned(base: &Path) {
        let Ok(entries) = fs::read_dir(base) else {
            return;
        };
        for entry in entries.flatten() {
            let is_run_dir = entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(RUN_DIR_PREFIX))
                // `file_type` does not follow symlinks.
                && entry.file_type().is_ok_and(|file_type| file_type.is_dir());
            let path = entry.path();
            if is_run_dir && !RunLock::is_held(&path.join(LOCK_FILE_NAME)) {
                tracing::debug!(
                    "Removing Chrome profiles of an ended run: {}",
                    path.display()
                );
                remove_dir(&path);
            }
        }
    }

    /// Create an empty profile for one session.
    pub(crate) fn create_session_profile(&self) -> io::Result<SessionProfile> {
        let session = self.next_session.fetch_add(1, Ordering::Relaxed);
        let dir = OwnedDir::create(self.dir.path().join(format!("session-{session}")))?;
        // `ChromeProfilesDir::resolve` checked that the path is valid UTF-8, so `display` is
        // lossless.
        let user_data_dir_arg = format!("--{USER_DATA_DIR_SWITCH}={}", dir.path().display());
        Ok(SessionProfile {
            dir,
            user_data_dir_arg,
        })
    }

    /// Remove the run's directory without blocking the runtime.
    ///
    /// Dropping removes it as well, blocking the thread. That only happens if the run ends early.
    pub(crate) async fn remove(self) {
        let Self { lock, dir, .. } = self;
        drop(lock);
        dir.remove().await;
    }
}

/// The profile of one session.
#[derive(Debug)]
pub(crate) struct SessionProfile {
    dir: OwnedDir,
    user_data_dir_arg: String,
}

impl SessionProfile {
    /// Let Chrome use this profile.
    ///
    /// # Errors
    ///
    /// Returns an error if `caps` already name a profile directory. Only the runner chooses it.
    pub(crate) fn configure(&self, caps: &mut ChromeCapabilities) -> WebDriverResult<()> {
        let names_profile_dir = caps.args().iter().any(|arg| {
            // Chrome takes switches with one dash, two, or none (`ChromeDriver` adds them).
            let switch = arg.trim_start_matches('-');
            switch == USER_DATA_DIR_SWITCH
                || switch
                    .strip_prefix(USER_DATA_DIR_SWITCH)
                    .is_some_and(|value| value.starts_with('='))
        });
        if names_profile_dir {
            return Err(WebDriverError::SessionCreateError(format!(
                "`--{USER_DATA_DIR_SWITCH}` must not be set, because browser-test gives every \
                 session a profile of its own. Choose where profiles are kept with \
                 `BrowserTestRunner::with_chrome_profiles_dir`."
            )));
        }
        caps.add_arg(&self.user_data_dir_arg)
    }

    /// Remove the profile without blocking the runtime.
    ///
    /// Dropping removes it as well, blocking the thread. That only happens if the session's
    /// worker is cancelled.
    pub(crate) async fn remove(self) {
        self.dir.remove().await;
    }
}

/// An exclusive lock on a run's lock file. The operating system releases it when the process
/// exits, however it exits.
#[derive(Debug)]
struct RunLock {
    _file: File,
}

impl RunLock {
    /// Create the lock file at `path` and lock it.
    fn acquire_new(path: &Path) -> io::Result<Self> {
        let file = File::create_new(path)?;
        file.try_lock()?;
        Ok(Self { _file: file })
    }

    /// Whether a process holds the lock on the lock file at `path`.
    ///
    /// A missing lock file counts as released, as nobody can hold a lock on it. Unknown states (the
    /// file cannot be opened, locking is unsupported) count as held.
    fn is_held(path: &Path) -> bool {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return false,
            Err(_) => return true,
        };
        // Taking the lock succeeds only if no process holds it. Dropping `file` releases it.
        match file.try_lock() {
            Ok(()) => false,
            Err(TryLockError::WouldBlock | TryLockError::Error(_)) => true,
        }
    }
}

/// A directory created by this module, accessible by its owner only. Removed, with everything in
/// it, when dropped.
#[derive(Debug)]
struct OwnedDir {
    path: PathBuf,
}

impl OwnedDir {
    fn create(path: PathBuf) -> io::Result<Self> {
        private_dir_builder().create(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn rename(&mut self, to: PathBuf) -> io::Result<()> {
        fs::rename(&self.path, &to)?;
        self.path = to;
        Ok(())
    }

    /// Remove the directory on a blocking thread.
    async fn remove(self) {
        // Should the runtime shut down before the task ran, dropping the task drops `self`, which
        // removes the directory right away.
        let _ = tokio::task::spawn_blocking(move || drop(self)).await;
    }
}

impl Drop for OwnedDir {
    fn drop(&mut self) {
        remove_dir(&self.path);
    }
}

fn private_dir_builder() -> fs::DirBuilder {
    #[cfg_attr(not(unix), expect(unused_mut))]
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
}

/// Remove `path` and everything in it, logging failures.
fn remove_dir(path: &Path) {
    match fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            "Failed to remove Chrome profile directory {}: {error}",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use assertr::prelude::*;

    use super::*;

    /// A profiles directory for one test, removed when dropped, even if the test fails.
    struct TestProfilesDir {
        profiles_dir: ChromeProfilesDir,
    }

    impl TestProfilesDir {
        /// A fresh directory in the system's temporary directory for the test called `name`.
        fn new(name: &str) -> Self {
            Self::at(std::env::temp_dir().join(format!(
                "browser-test-profile-tests-{}-{name}",
                std::process::id()
            )))
        }

        fn at(path: impl Into<PathBuf>) -> Self {
            let profiles_dir = ChromeProfilesDir::new(path);
            let _ = fs::remove_dir_all(profiles_dir.path());
            Self { profiles_dir }
        }

        fn path(&self) -> &Path {
            self.profiles_dir.path()
        }

        fn create_run(&self) -> RunProfiles {
            RunProfiles::create_blocking(&self.profiles_dir).expect("run should be created")
        }
    }

    impl Drop for TestProfilesDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.path());
        }
    }

    fn entry_count(dir: &Path) -> usize {
        fs::read_dir(dir).expect("dir should be readable").count()
    }

    #[test]
    fn profiles_are_removed_with_their_session_and_run() {
        let profiles_dir = TestProfilesDir::new("removal");
        let run = profiles_dir.create_run();
        let first = run
            .create_session_profile()
            .expect("profile should be created");
        let second = run
            .create_session_profile()
            .expect("profile should be created");
        let first_path = first.dir.path().to_owned();
        let second_path = second.dir.path().to_owned();
        fs::write(first_path.join("Preferences"), "{}").expect("profile should be writable");
        assert_that!(first_path.clone()).is_not_equal_to(second_path.clone());

        drop(first);
        assert_that!(first_path.exists()).is_false();
        assert_that!(second_path.is_dir()).is_true();

        let run_path = run.dir.path().to_owned();
        drop(second);
        drop(run);
        assert_that!(run_path.exists()).is_false();
        assert_that!(entry_count(profiles_dir.path())).is_equal_to(0);
    }

    #[test]
    fn a_starting_run_removes_only_abandoned_runs() {
        let profiles_dir = TestProfilesDir::new("sweep");
        let base = profiles_dir.path();
        let alive = profiles_dir.create_run();

        // A run whose process is gone: its lock file exists, but nobody holds the lock.
        let abandoned = base.join(format!("{RUN_DIR_PREFIX}abandoned"));
        fs::create_dir_all(abandoned.join("session-0")).expect("dir should be created");
        File::create(abandoned.join(LOCK_FILE_NAME)).expect("lock file should be created");
        // The remainder of an interrupted removal, or recreated by a Chrome that outlived its run.
        let without_lock = base.join(format!("{RUN_DIR_PREFIX}without-lock-file"));
        fs::create_dir_all(without_lock.join("session-0").join("Default"))
            .expect("dir should be created");
        // Directories not created by this module stay, even if they hold a profile.
        let foreign = base.join("org.chromium.Chromium.scoped_dir.abc");
        fs::create_dir_all(&foreign).expect("dir should be created");
        let staging = base.join(format!("{STAGING_DIR_PREFIX}starting"));
        fs::create_dir_all(&staging).expect("dir should be created");

        let next = profiles_dir.create_run();

        assert_that!(abandoned.exists()).is_false();
        assert_that!(without_lock.exists()).is_false();
        assert_that!(alive.dir.path().is_dir()).is_true();
        assert_that!(foreign.is_dir()).is_true();
        assert_that!(staging.is_dir()).is_true();
        assert_that!(entry_count(base)).is_equal_to(4);

        drop(alive);
        drop(next);
    }

    #[cfg(unix)]
    #[test]
    fn a_run_whose_lock_file_cannot_be_opened_stays() {
        use std::os::unix::fs::PermissionsExt as _;

        let profiles_dir = TestProfilesDir::new("unreadable-lock");
        let unknown = profiles_dir
            .path()
            .join(format!("{RUN_DIR_PREFIX}unreadable-lock"));
        private_dir_builder()
            .recursive(true)
            .create(&unknown)
            .expect("dir should be created");
        let lock_file = unknown.join(LOCK_FILE_NAME);
        File::create(&lock_file).expect("lock file should be created");
        fs::set_permissions(&lock_file, fs::Permissions::from_mode(0o000))
            .expect("permissions should be set");
        // Root opens the file regardless, so the state would not be unknown.
        if File::open(&lock_file).is_ok() {
            return;
        }

        let _run = profiles_dir.create_run();

        assert_that!(unknown.is_dir()).is_true();
    }

    #[test]
    fn session_profile_sets_user_data_dir() {
        let profiles_dir = TestProfilesDir::new("caps");
        let run = profiles_dir.create_run();
        let profile = run
            .create_session_profile()
            .expect("profile should be created");

        let mut caps = ChromeCapabilities::new();
        profile.configure(&mut caps).expect("arg should be added");
        assert_that!(caps.args()).is_equal_to(vec![format!(
            "--user-data-dir={}",
            profile.dir.path().display()
        )]);
    }

    #[test]
    fn session_profile_rejects_a_configured_user_data_dir() {
        let profiles_dir = TestProfilesDir::new("conflict");
        let run = profiles_dir.create_run();
        let profile = run
            .create_session_profile()
            .expect("profile should be created");

        for configured in [
            "--user-data-dir=/custom",
            "-user-data-dir=/custom",
            "user-data-dir=/custom",
            "--user-data-dir",
        ] {
            let mut caps = ChromeCapabilities::new();
            caps.add_arg(configured).expect("arg should be added");
            assert_that!(profile.configure(&mut caps).is_err()).is_true();
        }
        let mut caps = ChromeCapabilities::new();
        caps.add_arg("--user-data-dir-like=x")
            .expect("arg should be added");
        assert_that!(profile.configure(&mut caps).is_ok()).is_true();
    }

    #[test]
    fn relative_profiles_dir_is_made_absolute() {
        let profiles_dir = TestProfilesDir::at("target/browser-test-profile-tests-relative");
        let run = profiles_dir.create_run();

        assert_that!(run.dir.path().is_absolute()).is_true();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_profiles_dir_other_users_can_access() {
        use std::os::unix::fs::PermissionsExt as _;

        let profiles_dir = TestProfilesDir::new("shared");
        fs::create_dir(profiles_dir.path()).expect("profiles dir should be created");
        fs::set_permissions(profiles_dir.path(), fs::Permissions::from_mode(0o777))
            .expect("permissions should be set");

        let error = RunProfiles::create_blocking(&profiles_dir.profiles_dir)
            .expect_err("a shared profiles dir should be rejected");
        assert_that!(error.to_string()).contains("chmod 700");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_profiles_dir_of_another_user() {
        if nix::unistd::geteuid().is_root() {
            return;
        }
        // Owned by root.
        let error = RunProfiles::create_blocking(&ChromeProfilesDir::new("/"))
            .expect_err("a profiles dir of another user should be rejected");
        assert_that!(error.to_string()).contains("belongs to another user");
    }

    #[cfg(unix)]
    #[test]
    fn the_default_profiles_dir_is_per_user() {
        let name = format!("browser-test-profiles-{}", nix::unistd::geteuid());
        assert_that!(ChromeProfilesDir::in_temp_dir().path())
            .is_equal_to(std::env::temp_dir().join(name).as_path());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symlinked_profiles_dir() {
        let target = TestProfilesDir::new("symlink-target");
        fs::create_dir(target.path()).expect("target should be created");
        let link = TestProfilesDir::new("symlink");
        std::os::unix::fs::symlink(target.path(), link.path()).expect("link should be created");

        let error = RunProfiles::create_blocking(&link.profiles_dir)
            .expect_err("a symlinked profiles dir should be rejected");
        assert_that!(error.to_string()).contains("is a symlink");
        assert_that!(entry_count(target.path())).is_equal_to(0);
    }
}

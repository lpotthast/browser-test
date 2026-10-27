use std::{
    borrow::Cow,
    env,
    ffi::{OsStr, OsString},
    marker::PhantomData,
    sync::{Mutex, MutexGuard, PoisonError},
};

use thirtyfour::WebDriver;

use crate::{BrowserTest, TestFuture, TestSettings};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Restores an environment variable on drop and serializes environment access across tests.
///
/// [`EnvVarGuard::new`] takes the environment lock. [`EnvVarGuard::also`] guards a further
/// variable under the lock of an existing guard, which it borrows, so it cannot outlive that lock.
pub(crate) struct EnvVarGuard<'lock> {
    name: &'static str,
    original: Option<OsString>,
    _lock: Option<MutexGuard<'static, ()>>,
    _borrowed_lock: PhantomData<&'lock ()>,
}

impl EnvVarGuard<'static> {
    pub(crate) fn new(name: &'static str) -> Self {
        // A test that panics while holding the lock poisons it. Its guards still restore the
        // environment while unwinding, so later tests can safely proceed.
        let lock = ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        Self {
            name,
            original: env::var_os(name),
            _lock: Some(lock),
            _borrowed_lock: PhantomData,
        }
    }
}

impl EnvVarGuard<'_> {
    /// Guard another variable under the environment lock this guard holds.
    #[expect(
        clippy::unused_self,
        reason = "the borrow of `self` keeps the lock alive for the returned guard"
    )]
    pub(crate) fn also(&self, name: &'static str) -> EnvVarGuard<'_> {
        EnvVarGuard {
            name,
            original: env::var_os(name),
            _lock: None,
            _borrowed_lock: PhantomData,
        }
    }

    pub(crate) fn set(&self, value: impl AsRef<OsStr>) {
        // SAFETY: ENV_LOCK serializes environment mutations in this crate's tests.
        unsafe {
            env::set_var(self.name, value);
        }
    }

    pub(crate) fn remove(&self) {
        // SAFETY: ENV_LOCK serializes environment mutations in this crate's tests.
        unsafe {
            env::remove_var(self.name);
        }
    }
}

impl Drop for EnvVarGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: ENV_LOCK serializes environment mutations in this crate's tests.
        unsafe {
            match &self.original {
                Some(value) => env::set_var(self.name, value),
                None => env::remove_var(self.name),
            }
        }
    }
}

/// A metadata method of [`StubTest`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Metadata {
    Name,
    Tags,
    Settings,
}

/// A passing test with the given metadata, or one whose metadata method panics.
pub(crate) struct StubTest {
    name: &'static str,
    tags: &'static [&'static str],
    settings: TestSettings,
    panics_in: Option<Metadata>,
}

impl StubTest {
    pub(crate) const fn new(name: &'static str) -> Self {
        Self {
            name,
            tags: &[],
            settings: TestSettings::new(),
            panics_in: None,
        }
    }

    pub(crate) const fn with_tags(mut self, tags: &'static [&'static str]) -> Self {
        self.tags = tags;
        self
    }

    pub(crate) const fn with_settings(mut self, settings: TestSettings) -> Self {
        self.settings = settings;
        self
    }

    pub(crate) const fn panicking_in(mut self, metadata: Metadata) -> Self {
        self.panics_in = Some(metadata);
        self
    }

    fn panic_if(&self, metadata: Metadata) {
        assert!(self.panics_in != Some(metadata), "{} panics", self.name);
    }
}

impl BrowserTest for StubTest {
    fn name(&self) -> Cow<'_, str> {
        self.panic_if(Metadata::Name);
        Cow::Borrowed(self.name)
    }

    fn tags(&self) -> Vec<Cow<'_, str>> {
        self.panic_if(Metadata::Tags);
        self.tags.iter().copied().map(Cow::Borrowed).collect()
    }

    fn settings(&self) -> TestSettings {
        self.panic_if(Metadata::Settings);
        self.settings
    }

    fn run<'a>(&'a self, _driver: &'a WebDriver, (): &'a ()) -> TestFuture<'a> {
        Box::pin(async move { Ok(()) })
    }
}

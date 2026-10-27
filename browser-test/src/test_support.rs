use std::{
    borrow::Cow,
    env,
    ffi::{OsStr, OsString},
    sync::{Mutex, MutexGuard},
};

use rootcause::Report;
use thirtyfour::WebDriver;

use crate::{BrowserTest, TestSettings};

static ENV_LOCK: Mutex<()> = Mutex::new(());

pub(crate) struct EnvVarGuard {
    name: &'static str,
    original: Option<OsString>,
    _lock: Option<MutexGuard<'static, ()>>,
}

impl EnvVarGuard {
    pub(crate) fn new(name: &'static str) -> Self {
        let lock = ENV_LOCK
            .lock()
            .expect("environment lock should not be poisoned");
        let original = env::var_os(name);
        Self {
            name,
            original,
            _lock: Some(lock),
        }
    }

    /// Guard another variable while a guard created with [`Self::new`] holds the environment
    /// lock.
    pub(crate) fn new_unlocked(name: &'static str) -> Self {
        Self {
            name,
            original: env::var_os(name),
            _lock: None,
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

impl Drop for EnvVarGuard {
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

#[async_trait::async_trait]
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

    async fn run(&self, _driver: &WebDriver, (): &()) -> Result<(), Report> {
        Ok(())
    }
}

use std::{
    env,
    ffi::{OsStr, OsString},
    sync::{Mutex, MutexGuard},
};

static ENV_LOCK: Mutex<()> = Mutex::new(());

pub(crate) struct EnvVarGuard {
    name: &'static str,
    original: Option<OsString>,
    _lock: MutexGuard<'static, ()>,
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
            _lock: lock,
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

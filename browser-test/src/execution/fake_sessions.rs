//! Fake browser sessions, to test the engine without a browser. Every step that takes time in a
//! real session sleeps instead, so that tests with paused time run instantly.

use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use chrome_for_testing_manager::CancellationToken;
use rootcause::Report;

use super::sessions::SessionFactory;
use crate::{
    BrowserTest, ElementQueryWait, SessionReset, Timeouts, frozen_pages::PageCheck,
    session_reuse::ResetError,
};

/// How long creating a fake session takes, unless set.
const CREATION_TIME: Duration = Duration::from_secs(1);

/// How long a fake test runs, unless set.
const TEST_TIME: Duration = Duration::from_secs(5);

/// How long resetting a fake session takes.
const RESET_TIME: Duration = Duration::from_millis(100);

/// What a fake test does.
#[derive(Clone, Copy)]
pub(super) enum FakeTest {
    /// Pass after this long.
    Pass(Duration),
    /// Fail right away.
    Fail,
    /// Cancel the run after this long, and then wait for that to end the test.
    Cancel(Duration),
}

impl Default for FakeTest {
    fn default() -> Self {
        Self::Pass(TEST_TIME)
    }
}

/// What resetting a fake session does.
#[derive(Clone, Copy, Default)]
pub(super) enum FakeReset {
    #[default]
    Succeed,
    /// Never finish.
    Hang,
    /// Cancel the run, and then wait for that to end the reset.
    Cancel,
}

/// A fake session, numbered in the order the fake sessions were created.
pub(super) struct FakeSession {
    id: usize,
}

/// Creates fake sessions, and logs what happens in them.
#[derive(Default)]
pub(super) struct FakeSessions {
    cancellation: CancellationToken,
    next_session: AtomicUsize,
    /// How long creating each session takes, by number. [`CREATION_TIME`] for the others.
    creation_times: HashMap<usize, Duration>,
    /// The sessions that can't be created, by number.
    failing_creations: Vec<usize>,
    /// The sessions that can't be quit, by number.
    failing_quits: Vec<usize>,
    tests: HashMap<&'static str, FakeTest>,
    reset: FakeReset,
    events: Mutex<Vec<String>>,
}

impl FakeSessions {
    /// Fake sessions of a run cancelled with `cancellation`.
    pub(super) fn new(cancellation: &CancellationToken) -> Self {
        Self {
            cancellation: cancellation.clone(),
            ..Self::default()
        }
    }

    pub(super) fn with_creation_time(mut self, session: usize, time: Duration) -> Self {
        self.creation_times.insert(session, time);
        self
    }

    pub(super) fn failing_creation(mut self, session: usize) -> Self {
        self.failing_creations.push(session);
        self
    }

    pub(super) fn failing_quit(mut self, session: usize) -> Self {
        self.failing_quits.push(session);
        self
    }

    pub(super) fn with_test(mut self, name: &'static str, test: FakeTest) -> Self {
        self.tests.insert(name, test);
        self
    }

    pub(super) fn with_reset(mut self, reset: FakeReset) -> Self {
        self.reset = reset;
        self
    }

    /// What happened so far: sessions created (`create 0`), failing to be created (`fail 0`),
    /// reset (`reset 0`) and quit (`quit 0`), and tests starting (`start a`) and ending (`end a`).
    pub(super) fn events(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn log(&self, event: String) {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event);
    }
}

impl SessionFactory for FakeSessions {
    type Session = FakeSession;
    type Baseline = ();

    async fn run_session<F>(
        &self,
        _element_query_wait: Option<ElementQueryWait>,
        serve: F,
    ) -> Result<(), Report>
    where
        F: for<'s> AsyncFnOnce(&'s FakeSession) -> Result<(), Report>,
    {
        let id = self.next_session.fetch_add(1, Ordering::SeqCst);
        let run = async {
            let creation_time = self.creation_times.get(&id).copied();
            tokio::time::sleep(creation_time.unwrap_or(CREATION_TIME)).await;
            if self.failing_creations.contains(&id) {
                self.log(format!("fail {id}"));
                rootcause::bail!("session {id} could not be created");
            }
            self.log(format!("create {id}"));
            let served = serve(&FakeSession { id }).await;
            self.log(format!("quit {id}"));
            if self.failing_quits.contains(&id) {
                rootcause::bail!("session {id} could not be quit");
            }
            served
        };
        // Like a session of Chrome for Testing, which a cancelled run drops.
        tokio::select! {
            result = run => result,
            () = self.cancellation.cancelled() => Err(rootcause::report!("cancelled")),
        }
    }

    fn ready(&self, _session: &FakeSession) -> impl Future<Output = Result<(), Report>> {
        std::future::ready(Ok(()))
    }

    fn baseline(
        &self,
        _session: &FakeSession,
        _reset: SessionReset,
    ) -> impl Future<Output = Result<(), Report>> {
        std::future::ready(Ok(()))
    }

    async fn reset(&self, session: &FakeSession, (): &mut ()) -> Result<(), ResetError> {
        self.log(format!("reset {}", session.id));
        match self.reset {
            FakeReset::Succeed => tokio::time::sleep(RESET_TIME).await,
            FakeReset::Hang => std::future::pending().await,
            FakeReset::Cancel => {
                self.cancellation.cancel();
                std::future::pending::<()>().await;
            }
        }
        Ok(())
    }

    fn set_timeouts(
        &self,
        _session: &FakeSession,
        _timeouts: Timeouts,
    ) -> impl Future<Output = Result<(), Report>> {
        std::future::ready(Ok(()))
    }

    async fn run_test<'a, Context>(
        &'a self,
        _session: &'a FakeSession,
        test: &'a dyn BrowserTest<Context>,
        _context: &'a Context,
    ) -> Result<(), Report>
    where
        Context: Sync + ?Sized,
    {
        let name = test.name().into_owned();
        self.log(format!("start {name}"));
        let result = match self.tests.get(name.as_str()).copied().unwrap_or_default() {
            FakeTest::Pass(time) => {
                tokio::time::sleep(time).await;
                Ok(())
            }
            FakeTest::Fail => Err(rootcause::report!("{name} failed")),
            FakeTest::Cancel(time) => {
                tokio::time::sleep(time).await;
                self.cancellation.cancel();
                std::future::pending().await
            }
        };
        self.log(format!("end {name}"));
        result
    }

    fn session_id(&self, session: &FakeSession) -> String {
        format!("fake-{}", session.id)
    }

    fn close_frozen_pages(&self, _session: &FakeSession) -> impl Future<Output = PageCheck> {
        std::future::ready(PageCheck::Checked {
            frozen: Vec::new(),
            unchecked: Vec::new(),
        })
    }
}

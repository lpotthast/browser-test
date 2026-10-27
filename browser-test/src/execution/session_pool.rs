//! The pool of sessions kept ready for starting tests.

use std::{
    collections::VecDeque,
    sync::{Mutex, PoisonError},
    time::Instant,
};

use chrome_for_testing_manager::CancellationToken;
use rootcause::Report;
use tokio::sync::{mpsc, oneshot};

use super::worker::Job;
use crate::{ElementQueryWait, SessionPreparation};

/// A session ready to run a test.
pub(super) struct Ticket {
    pub(super) job: oneshot::Sender<Job>,
    /// How the session was prepared: created, or reset after an earlier test.
    pub(super) preparation: SessionPreparation,
}

/// A session that could not be created, or ended before its test started.
pub(super) struct CreationFailure {
    pub(super) report: Report,
    pub(super) preparation: SessionPreparation,
}

pub(super) type TicketResult = Result<Ticket, CreationFailure>;

/// Where a new session is offered.
pub(super) enum Delivery {
    Pool,
    Dedicated(oneshot::Sender<TicketResult>),
}

/// A request to create a session.
pub(super) struct WorkerRequest {
    pub(super) session: usize,
    pub(super) element_query_wait: Option<ElementQueryWait>,
    pub(super) delivery: Delivery,
}

/// Keeps sessions with the runner's default settings ready for starting tests: fresh ones, and,
/// with [`SessionReuse`](crate::SessionReuse), those of finished tests after resetting them.
/// Creates sessions while starting tests need more than it has (counting spares), and takes back
/// reset sessions only while upcoming tests need them, so that the others quit. Waiting tests get
/// sessions in the order they asked for them.
pub(super) struct SessionPool {
    state: Mutex<PoolState>,
    /// How many tests can run at the same time.
    max_concurrency: usize,
    /// Sessions to keep ready beyond those requested.
    spare: usize,
    pub(super) default_wait: Option<ElementQueryWait>,
    requests: mpsc::UnboundedSender<Option<WorkerRequest>>,
    /// Ends the wait of tests for sessions: a cancelled run drops the sessions on their way.
    cancellation: CancellationToken,
    /// Cancelled once no further tests start, except those that run always: a failure stopped the
    /// run (see [`Self::stop`]), or it was cancelled.
    stop: CancellationToken,
}

struct PoolState {
    /// Sessions ready for a test, not yet taken. Only those that no waiting test can take: one
    /// needing a fresh session can't take a reset one.
    idle: VecDeque<Ticket>,
    /// A spare session that could not be created once the retries were spent, which fails the next
    /// test taking a session (see [`PoolState::offer_failure`]).
    failure: Option<CreationFailure>,
    /// How many more spares that could not be created are created again.
    creation_retries: usize,
    /// Sessions being created for the pool.
    creating: usize,
    /// Sessions of finished tests being reset for the pool.
    resetting: usize,
    /// Pool sessions running a test.
    running: usize,
    /// Pool sessions whose browser is open, or opening: from the request to create them until
    /// their worker ended, which includes sessions that are quitting.
    alive: usize,
    /// Tests waiting for a pool session, in the order they asked for one.
    waiters: VecDeque<Waiter>,
    next_waiter: u64,
    /// Pool tests that have not requested a session yet.
    upcoming: Upcoming,
    /// Those of `upcoming` that start even after a failure stopped the run (see
    /// [`TestSuite::run_always`](crate::TestSuite::run_always)).
    upcoming_always: Upcoming,
    next_session: usize,
    closed: bool,
}

/// A test waiting for a pool session.
struct Waiter {
    id: u64,
    /// Whether the test needs a fresh session.
    fresh: bool,
    ticket: oneshot::Sender<TicketResult>,
}

/// A test's place in the queue of tests waiting for a pool session. Dropping it leaves the queue,
/// and offers a session the test was handed meanwhile to the next one.
struct Waiting<'p> {
    pool: &'p SessionPool,
    id: u64,
    ticket: oneshot::Receiver<TicketResult>,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        let mut state = self.pool.lock();
        if let Some(position) = state.waiters.iter().position(|waiter| waiter.id == self.id) {
            state.waiters.remove(position);
        }
        match self.ticket.try_recv() {
            Ok(Ok(ticket)) => {
                state.running -= 1;
                state.offer(ticket);
            }
            Ok(Err(failure)) => state.offer_failure(failure),
            Err(_) => {}
        }
    }
}

/// A number of tests: those taking any session, and those needing a fresh one.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Upcoming {
    pub(super) any: usize,
    pub(super) fresh: usize,
}

impl Upcoming {
    fn total(self) -> usize {
        self.any + self.fresh
    }

    fn count(&mut self, fresh: bool) -> &mut usize {
        if fresh {
            &mut self.fresh
        } else {
            &mut self.any
        }
    }

    /// Count one test less.
    fn remove(&mut self, fresh: bool) {
        let count = self.count(fresh);
        *count = count.saturating_sub(1);
    }
}

impl PoolState {
    /// The tests waiting for a session.
    fn waiting(&self) -> Upcoming {
        let fresh = self.waiters.iter().filter(|waiter| waiter.fresh).count();
        Upcoming {
            any: self.waiters.len() - fresh,
            fresh,
        }
    }

    /// Sessions ready or on their way to the pool. A kept creation failure counts as well, so that
    /// no further spare is created while it waits for a test.
    fn supply(&self) -> usize {
        self.idle.len() + self.creating + self.resetting + usize::from(self.failure.is_some())
    }

    /// Fresh sessions ready or on their way to the pool.
    fn fresh_supply(&self) -> usize {
        self.idle.iter().filter(|ticket| ticket.is_fresh()).count()
            + self.creating
            + usize::from(self.failure.is_some())
    }

    /// The idle session a test takes: a fresh one if it needs one, otherwise preferably a reset
    /// one, which keeps the fresh ones for tests that need them. Without one, a kept creation
    /// failure.
    fn take_idle(&mut self, fresh: bool) -> Option<TicketResult> {
        let position = if fresh {
            self.idle.iter().position(Ticket::is_fresh)
        } else {
            self.idle
                .iter()
                .position(|ticket| !ticket.is_fresh())
                .or_else(|| (!self.idle.is_empty()).then_some(0))
        };
        match position {
            Some(position) => self.idle.remove(position).map(Ok),
            None => self.failure.take().map(Err),
        }
    }

    /// Hand `ticket` to the first waiting test that can take it, or else keep it idle. Dropped if
    /// the pool closed, so that its session quits.
    fn offer(&mut self, ticket: Ticket) {
        if self.closed {
            return;
        }
        let fresh = ticket.is_fresh();
        match self.hand_to_waiter(Ok(ticket), |waiter| fresh || !waiter.fresh) {
            None => self.running += 1,
            Some(ticket) => self.idle.extend(ticket.ok()),
        }
    }

    /// Hand the failure to create a pool session to the first waiting test. If no test waits,
    /// the session was a spare: the failure is logged, and the spare is created again while
    /// retries are left. Once they are spent, the failure is kept for the next test taking a
    /// session, which stops the pool from creating spares without end when every creation fails.
    fn offer_failure(&mut self, failure: CreationFailure) {
        if self.closed {
            return;
        }
        let Some(Err(failure)) = self.hand_to_waiter(Err(failure), |_| true) else {
            return;
        };
        if self.creation_retries > 0 {
            self.creation_retries -= 1;
            tracing::warn!(
                "A spare browser session could not be created, so another one is: {}",
                failure.report
            );
        } else if self.failure.is_none() {
            self.failure = Some(failure);
        } else {
            tracing::warn!(
                "A spare browser session could not be created: {}",
                failure.report
            );
        }
    }

    /// Send `ticket` to the first waiting test that `fits`. Returns it if none takes it.
    fn hand_to_waiter(
        &mut self,
        mut ticket: TicketResult,
        fits: impl Fn(&Waiter) -> bool,
    ) -> Option<TicketResult> {
        while let Some(position) = self.waiters.iter().position(&fits) {
            let waiter = self
                .waiters
                .remove(position)
                .expect("the position is in bounds");
            match waiter.ticket.send(ticket) {
                Ok(()) => return None,
                // The test left the queue: try the next one.
                Err(returned) => ticket = returned,
            }
        }
        Some(ticket)
    }
}

impl Ticket {
    /// Whether no test ran in the session yet.
    fn is_fresh(&self) -> bool {
        matches!(self.preparation, SessionPreparation::Created(_))
    }
}

impl SessionPool {
    pub(super) fn new(
        max_concurrency: usize,
        spare: usize,
        upcoming: Upcoming,
        upcoming_always: Upcoming,
        default_wait: Option<ElementQueryWait>,
        requests: mpsc::UnboundedSender<Option<WorkerRequest>>,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            state: Mutex::new(PoolState {
                idle: VecDeque::new(),
                failure: None,
                creation_retries: creation_retries(spare),
                creating: 0,
                resetting: 0,
                running: 0,
                alive: 0,
                waiters: VecDeque::new(),
                next_waiter: 0,
                upcoming,
                upcoming_always,
                next_session: 0,
                closed: false,
            }),
            max_concurrency,
            spare,
            default_wait,
            requests,
            stop: cancellation.child_token(),
            cancellation,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PoolState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether further tests start: the run neither stopped on a failure nor was cancelled.
    pub(super) fn keep_starting(&self) -> bool {
        !self.stop.is_cancelled()
    }

    /// Whether a test starts: unless the run stopped on a failure, or, also for tests that run
    /// always, was cancelled.
    pub(super) fn may_start(&self, run_always: bool) -> bool {
        if run_always {
            !self.cancellation.is_cancelled()
        } else {
            self.keep_starting()
        }
    }

    /// The upcoming tests that will still start: all of them while tests keep starting, only
    /// those that run always once a failure stopped the run, and none once it is cancelled.
    fn starting(&self, state: &PoolState) -> Upcoming {
        if self.cancellation.is_cancelled() {
            Upcoming::default()
        } else if self.keep_starting() {
            state.upcoming
        } else {
            state.upcoming_always
        }
    }

    /// Create sessions for waiting tests (fresh ones for tests that need them) and spares for
    /// upcoming ones that will still start.
    ///
    /// Waiting tests get their sessions right away. Spares are only created while fewer sessions
    /// are alive than tests can run at the same time plus spares, counting those still quitting:
    /// a spare that can't be created yet is once a session ended ([`Self::session_ended`]).
    pub(super) fn replenish(&self) {
        self.replenish_locked(&mut self.lock());
    }

    fn replenish_locked(&self, state: &mut PoolState) {
        let spares = self.spare.min(self.starting(state).total());
        let bound = self.max_concurrency + self.spare;
        let waiting = state.waiting();
        while !state.closed {
            let needs_fresh = state.fresh_supply() < waiting.fresh;
            let needs_any = state.supply() < waiting.total();
            let wants_spare = state.supply() < waiting.total() + spares && state.alive < bound;
            if !(needs_fresh || needs_any || wants_spare) {
                break;
            }
            if needs_fresh && state.alive >= bound {
                // A reset session no waiting test takes makes way for the fresh one: dropping
                // its ticket quits it.
                let reset = state
                    .idle
                    .iter()
                    .filter(|ticket| !ticket.is_fresh())
                    .count();
                if reset > waiting.any
                    && let Some(position) = state.idle.iter().position(|ticket| !ticket.is_fresh())
                {
                    state.idle.remove(position);
                }
            }
            state.creating += 1;
            state.alive += 1;
            let session = state.next_session;
            state.next_session += 1;
            let _ = self.requests.send(Some(WorkerRequest {
                session,
                element_query_wait: self.default_wait,
                delivery: Delivery::Pool,
            }));
        }
    }

    /// Take a session for a starting test, a `fresh` one if it needs one, waiting until one is
    /// ready. `None` if the test doesn't start anymore: the run was cancelled, or, for a test that
    /// doesn't run always, a failure stopped it (see [`Self::stop`]). A cancelled run drops the
    /// sessions being created or reset, so the one a test waits for may never arrive.
    pub(super) async fn take(&self, fresh: bool, run_always: bool) -> Option<TicketResult> {
        let mut waiting = {
            let mut state = self.lock();
            state.upcoming.remove(fresh);
            if run_always {
                state.upcoming_always.remove(fresh);
            }
            if !self.may_start(run_always) {
                return None;
            }
            if let Some(ticket) = state.take_idle(fresh) {
                // A kept creation failure doesn't count, as no session runs the test.
                if ticket.is_ok() {
                    state.running += 1;
                }
                self.replenish_locked(&mut state);
                return Some(ticket);
            }
            let (ticket_tx, ticket_rx) = oneshot::channel();
            let id = state.next_waiter;
            state.next_waiter += 1;
            state.waiters.push_back(Waiter {
                id,
                fresh,
                ticket: ticket_tx,
            });
            self.replenish_locked(&mut state);
            Waiting {
                pool: self,
                id,
                ticket: ticket_rx,
            }
        };
        let given_up = if run_always {
            &self.cancellation
        } else {
            &self.stop
        };
        tokio::select! {
            ticket = &mut waiting.ticket => ticket.ok(),
            () = given_up.cancelled() => None,
        }
    }

    /// Start no further tests, except those that run always (after a failure): tests waiting for a
    /// session that don't run always give up.
    pub(super) fn stop(&self) {
        self.stop.cancel();
    }

    /// Create a session with non-default settings for one test.
    pub(super) async fn dedicated(
        &self,
        element_query_wait: Option<ElementQueryWait>,
    ) -> TicketResult {
        let creation_start = Instant::now();
        let (ticket_tx, ticket_rx) = oneshot::channel();
        let session = {
            let mut state = self.lock();
            let session = state.next_session;
            state.next_session += 1;
            session
        };
        let _ = self.requests.send(Some(WorkerRequest {
            session,
            element_query_wait,
            delivery: Delivery::Dedicated(ticket_tx),
        }));
        ticket_rx.await.unwrap_or_else(|_| {
            Err(CreationFailure {
                report: rootcause::report!("the browser session could not be created"),
                preparation: SessionPreparation::Created(creation_start.elapsed()),
            })
        })
    }

    pub(super) fn deliver(&self, delivery: Delivery, ticket: TicketResult) {
        match delivery {
            Delivery::Pool => {
                let mut state = self.lock();
                state.creating -= 1;
                match ticket {
                    Ok(ticket) => {
                        state.creation_retries = creation_retries(self.spare);
                        state.offer(ticket);
                    }
                    Err(failure) => state.offer_failure(failure),
                }
            }
            Delivery::Dedicated(ticket_tx) => {
                let _ = ticket_tx.send(ticket);
            }
        }
    }

    /// The test of a pool session finished.
    pub(super) fn test_finished(&self) {
        self.lock().running -= 1;
    }

    /// The worker of a pool session ended: its browser quit, or never started. Create the spares
    /// that had to wait for it.
    pub(super) fn session_ended(&self) {
        let mut state = self.lock();
        state.alive -= 1;
        self.replenish_locked(&mut state);
    }

    /// Whether the session of a finished test should be reset: while tests (not needing a fresh
    /// session) are still to run, otherwise it quits. If so, it counts as a session on its way to
    /// the pool until [`Self::deliver_reset`] or [`Self::reset_failed`]. Claimed before the next
    /// test starts, so that the pool doesn't create a session for that test in the meantime.
    pub(super) fn claim_reset(&self) -> bool {
        let mut state = self.lock();
        let wanted = !state.closed && state.waiting().any + self.starting(&state).any > 0;
        if wanted {
            state.resetting += 1;
        }
        wanted
    }

    /// Offer a session reset after its test. The pool keeps it only while it has fewer sessions
    /// (running a test, ready or on their way) than tests that can run at the same time and
    /// spares need: otherwise it drops the ticket, and the session quits.
    pub(super) fn deliver_reset(&self, ticket: Ticket) {
        let mut state = self.lock();
        state.resetting -= 1;
        let starting = self.starting(&state).total();
        let spares = self.spare.min(starting);
        let running_at_once = self
            .max_concurrency
            .min(state.running + state.waiting().total() + starting);
        if state.running + state.supply() < running_at_once + spares {
            state.offer(ticket);
        }
    }

    /// A claimed reset failed, so the session quits: create another one if tests need it.
    pub(super) fn reset_failed(&self) {
        let mut state = self.lock();
        state.resetting -= 1;
        self.replenish_locked(&mut state);
    }

    /// Create no further sessions and let idle ones quit.
    pub(super) fn close(&self) {
        {
            let mut state = self.lock();
            state.closed = true;
            state.idle.clear();
            state.failure = None;
        }
        let _ = self.requests.send(None);
    }
}

/// How many spares that could not be created in a row the pool with `spare` spares creates again:
/// one per spare.
fn creation_retries(spare: usize) -> usize {
    spare.max(1)
}

#[cfg(test)]
mod tests {
    use std::pin::pin;

    use assertr::prelude::*;

    use super::*;

    fn pool(upcoming: Upcoming, cancellation: &CancellationToken) -> SessionPool {
        let (requests, _) = mpsc::unbounded_channel();
        SessionPool::new(
            1,
            0,
            upcoming,
            Upcoming::default(),
            None,
            requests,
            cancellation.clone(),
        )
    }

    /// A pool running one test at a time with one spare, and its session requests.
    fn pool_with_spare(
        upcoming: Upcoming,
    ) -> (SessionPool, mpsc::UnboundedReceiver<Option<WorkerRequest>>) {
        pool_with_spare_and_always(upcoming, Upcoming::default())
    }

    /// [`pool_with_spare`], where `upcoming_always` of the upcoming tests run always.
    fn pool_with_spare_and_always(
        upcoming: Upcoming,
        upcoming_always: Upcoming,
    ) -> (SessionPool, mpsc::UnboundedReceiver<Option<WorkerRequest>>) {
        let (requests, requested) = mpsc::unbounded_channel();
        let pool = SessionPool::new(
            1,
            1,
            upcoming,
            upcoming_always,
            None,
            requests,
            CancellationToken::new(),
        );
        (pool, requested)
    }

    fn requested(requests: &mut mpsc::UnboundedReceiver<Option<WorkerRequest>>) -> usize {
        std::iter::from_fn(|| requests.try_recv().ok()).count()
    }

    /// A session ready to run a test, and where its test arrives.
    fn ticket(preparation: SessionPreparation) -> (Ticket, oneshot::Receiver<Job>) {
        let (job, jobs) = oneshot::channel();
        (Ticket { job, preparation }, jobs)
    }

    #[tokio::test]
    async fn spares_wait_for_quitting_sessions() {
        let created = SessionPreparation::Created(std::time::Duration::ZERO);
        let (pool, mut requests) = pool_with_spare(Upcoming { any: 3, fresh: 0 });
        pool.replenish();
        assert_that!(requested(&mut requests)).is_equal_to(1);
        pool.deliver(Delivery::Pool, Ok(ticket(created).0));

        // The first test takes the spare, and the pool creates the next one.
        assert_that!(pool.take(false, false).await).is_some();
        assert_that!(requested(&mut requests)).is_equal_to(1);
        pool.deliver(Delivery::Pool, Ok(ticket(created).0));
        pool.test_finished();

        // While the first test's session quits, two browsers are open: the next test takes the
        // spare, and the pool creates no further one until the session ended.
        assert_that!(pool.take(false, false).await).is_some();
        assert_that!(requested(&mut requests)).is_equal_to(0);
        pool.session_ended();
        assert_that!(requested(&mut requests)).is_equal_to(1);
    }

    #[tokio::test]
    async fn fresh_sessions_replace_idle_reset_ones_at_the_bound() {
        let (pool, mut requests) = pool_with_spare(Upcoming { any: 0, fresh: 1 });
        let (reset, reset_jobs) = ticket(SessionPreparation::Reset(std::time::Duration::ZERO));
        {
            let mut state = pool.lock();
            state.alive = 2;
            state.idle.push_back(reset);
        }

        let take = pin!(pool.take(true, false));
        assert_that!(futures_util::poll!(take).is_pending()).is_true();

        assert_that!(requested(&mut requests)).is_equal_to(1);
        // The idle reset session's ticket was dropped, so it quits.
        assert_that!(reset_jobs.await.is_err()).is_true();
    }

    #[tokio::test]
    async fn waiting_for_a_reset_ends_when_the_run_is_cancelled() {
        let cancellation = CancellationToken::new();
        let pool = pool(Upcoming { any: 2, fresh: 0 }, &cancellation);
        // The session of a finished test is reset for the next one, which then waits for it
        // instead of a new session. A cancelled run drops the reset.
        assert_that!(pool.claim_reset()).is_true();
        let take = pool.take(false, false);
        cancellation.cancel();

        assert_that!(take.await.is_none()).is_true();
        assert_that!(pool.lock().waiting().total()).is_equal_to(0);
    }

    #[tokio::test]
    async fn tests_that_run_always_keep_spares_and_resets_after_a_stop() {
        let (pool, mut requests) = pool_with_spare_and_always(
            Upcoming { any: 3, fresh: 0 },
            Upcoming { any: 2, fresh: 0 },
        );
        // A failure stopped the run, but two tests that run always are still to start: a
        // finished test's session is reset rather than quit, and a spare is kept ready for them.
        pool.stop();
        assert_that!(pool.claim_reset()).is_true();
        pool.reset_failed();
        assert_that!(requested(&mut requests)).is_equal_to(1);
    }

    #[tokio::test]
    async fn without_tests_that_run_always_a_stop_ends_spares_and_resets() {
        let (pool, mut requests) = pool_with_spare(Upcoming { any: 3, fresh: 0 });
        pool.stop();
        assert_that!(pool.claim_reset()).is_false();
        pool.replenish();
        assert_that!(requested(&mut requests)).is_equal_to(0);
    }

    #[tokio::test]
    async fn a_stop_ends_the_wait_of_tests_that_dont_run_always() {
        let (pool, _requests) = pool_with_spare(Upcoming { any: 2, fresh: 0 });
        let take = pool.take(false, false);
        let mut take = pin!(take);
        assert_that!(futures_util::poll!(take.as_mut()).is_pending()).is_true();

        pool.stop();

        assert_that!(take.await.is_none()).is_true();
        assert_that!(pool.lock().waiting().total()).is_equal_to(0);
    }

    #[tokio::test]
    async fn waiting_tests_get_sessions_in_the_order_they_asked() {
        let created = SessionPreparation::Created(std::time::Duration::ZERO);
        let (pool, _requests) = pool_with_spare(Upcoming { any: 3, fresh: 0 });
        let mut first = pin!(pool.take(false, false));
        let mut second = pin!(pool.take(false, false));
        assert_that!(futures_util::poll!(first.as_mut()).is_pending()).is_true();
        assert_that!(futures_util::poll!(second.as_mut()).is_pending()).is_true();

        // Polled in the other order, the second test still waits until the first got a session.
        pool.deliver(Delivery::Pool, Ok(ticket(created).0));
        assert_that!(futures_util::poll!(second.as_mut()).is_pending()).is_true();
        assert_that!(first.await).is_some();
        pool.deliver(Delivery::Pool, Ok(ticket(created).0));
        assert_that!(second.await).is_some();
        assert_that!(pool.lock().running).is_equal_to(2);
    }

    #[tokio::test]
    async fn a_session_handed_to_a_test_that_gave_up_goes_to_the_next_one() {
        let created = SessionPreparation::Created(std::time::Duration::ZERO);
        let (pool, _requests) = pool_with_spare(Upcoming { any: 2, fresh: 0 });
        let mut first = Box::pin(pool.take(false, false));
        let mut second = pin!(pool.take(false, false));
        assert_that!(futures_util::poll!(first.as_mut()).is_pending()).is_true();
        assert_that!(futures_util::poll!(second.as_mut()).is_pending()).is_true();

        pool.deliver(Delivery::Pool, Ok(ticket(created).0));
        drop(first);
        assert_that!(second.await).is_some();
        assert_that!(pool.lock().running).is_equal_to(1);
    }

    fn creation_failure() -> CreationFailure {
        CreationFailure {
            report: rootcause::report!("no browser"),
            preparation: SessionPreparation::Created(std::time::Duration::ZERO),
        }
    }

    #[tokio::test]
    async fn a_waiting_test_gets_the_failure_to_create_its_session() {
        let (pool, _requests) = pool_with_spare(Upcoming { any: 1, fresh: 0 });
        let mut take = pin!(pool.take(false, false));
        assert_that!(futures_util::poll!(take.as_mut()).is_pending()).is_true();

        pool.deliver(Delivery::Pool, Err(creation_failure()));

        assert_that!(matches!(take.await, Some(Err(_)))).is_true();
    }

    #[tokio::test]
    async fn a_spare_that_could_not_be_created_is_created_again_then_fails_the_next_test() {
        let created = SessionPreparation::Created(std::time::Duration::ZERO);
        let (pool, mut requests) = pool_with_spare(Upcoming { any: 3, fresh: 0 });
        pool.replenish();
        assert_that!(requested(&mut requests)).is_equal_to(1);

        // No test waits for the spare: it is created again, once.
        pool.deliver(Delivery::Pool, Err(creation_failure()));
        pool.session_ended();
        assert_that!(requested(&mut requests)).is_equal_to(1);

        // A creation succeeding restores the retry.
        pool.deliver(Delivery::Pool, Ok(ticket(created).0));
        assert_that!(pool.take(false, false).await).is_some();
        assert_that!(requested(&mut requests)).is_equal_to(1);
        pool.deliver(Delivery::Pool, Err(creation_failure()));
        pool.session_ended();
        assert_that!(requested(&mut requests)).is_equal_to(1);

        // Once the retry is spent, the failure is kept for the next test, instead of creating
        // spares without end.
        pool.deliver(Delivery::Pool, Err(creation_failure()));
        pool.session_ended();
        assert_that!(requested(&mut requests)).is_equal_to(0);
        assert_that!(matches!(pool.take(false, false).await, Some(Err(_)))).is_true();
        assert_that!(pool.lock().running).is_equal_to(1);
    }
}

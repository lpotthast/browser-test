//! The pool of sessions kept ready for starting tests.

use std::{
    collections::VecDeque,
    pin::pin,
    sync::{Mutex, PoisonError},
    time::Instant,
};

use chrome_for_testing_manager::CancellationToken;
use rootcause::Report;
use tokio::sync::{Notify, mpsc, oneshot};

use super::Job;
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
/// with [`SessionReuse`], those of finished tests after resetting them. Creates sessions while
/// starting tests need more than it has (counting spares), and takes back reset sessions only
/// while upcoming tests need them, so that the others quit.
pub(super) struct SessionPool {
    state: Mutex<PoolState>,
    available: Notify,
    /// How many tests can run at the same time.
    max_concurrency: usize,
    /// Sessions to keep ready beyond those requested.
    spare: usize,
    pub(super) default_wait: Option<ElementQueryWait>,
    requests: mpsc::UnboundedSender<Option<WorkerRequest>>,
    /// Ends the wait of tests for sessions: a cancelled run drops the sessions on their way.
    cancellation: CancellationToken,
}

struct PoolState {
    /// Sessions ready for a test, not yet taken.
    idle: VecDeque<TicketResult>,
    /// Sessions being created for the pool.
    creating: usize,
    /// Sessions of finished tests being reset for the pool.
    resetting: usize,
    /// Pool sessions running a test.
    running: usize,
    /// Pool sessions whose browser is open, or opening: from the request to create them until
    /// their worker ended, which includes sessions that are quitting.
    alive: usize,
    /// Tests waiting for a pool session.
    waiting: Upcoming,
    /// Pool tests that have not requested a session yet.
    upcoming: Upcoming,
    next_session: usize,
    closed: bool,
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
}

impl PoolState {
    /// Sessions ready or on their way to the pool.
    fn supply(&self) -> usize {
        self.idle.len() + self.creating + self.resetting
    }

    /// Fresh sessions ready or on their way to the pool.
    fn fresh_supply(&self) -> usize {
        self.idle.iter().filter(|ticket| is_fresh(ticket)).count() + self.creating
    }

    /// The idle session a test takes: a fresh one if it needs one, otherwise preferably a reset
    /// one, which keeps the fresh ones for tests that need them.
    fn take_idle(&mut self, fresh: bool) -> Option<TicketResult> {
        let position = if fresh {
            self.idle.iter().position(is_fresh)
        } else {
            self.idle
                .iter()
                .position(|ticket| !is_fresh(ticket))
                .or_else(|| (!self.idle.is_empty()).then_some(0))
        }?;
        self.idle.remove(position)
    }
}

/// Whether no test ran in the session of `ticket` yet. A session that could not be created counts
/// as fresh: its failure goes to whichever test takes it.
fn is_fresh(ticket: &TicketResult) -> bool {
    match ticket {
        Ok(ticket) => matches!(ticket.preparation, SessionPreparation::Created(_)),
        Err(_) => true,
    }
}

impl SessionPool {
    pub(super) fn new(
        max_concurrency: usize,
        spare: usize,
        upcoming: Upcoming,
        default_wait: Option<ElementQueryWait>,
        requests: mpsc::UnboundedSender<Option<WorkerRequest>>,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            state: Mutex::new(PoolState {
                idle: VecDeque::new(),
                creating: 0,
                resetting: 0,
                running: 0,
                alive: 0,
                waiting: Upcoming::default(),
                upcoming,
                next_session: 0,
                closed: false,
            }),
            available: Notify::new(),
            max_concurrency,
            spare,
            default_wait,
            requests,
            cancellation,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PoolState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Create sessions for waiting tests (fresh ones for tests that need them) and, unless tests
    /// stopped starting, spares for upcoming ones.
    ///
    /// Waiting tests get their sessions right away. Spares are only created while fewer sessions
    /// are alive than tests can run at the same time plus spares, counting those still quitting:
    /// a spare that can't be created yet is once a session ended ([`Self::session_ended`]).
    pub(super) fn replenish(&self, keep_starting: bool) {
        self.replenish_locked(&mut self.lock(), keep_starting);
    }

    fn replenish_locked(&self, state: &mut PoolState, keep_starting: bool) {
        let spares = if keep_starting {
            self.spare.min(state.upcoming.total())
        } else {
            0
        };
        let bound = self.max_concurrency + self.spare;
        while !state.closed {
            let waiting = state.waiting.total();
            let needs_fresh = state.fresh_supply() < state.waiting.fresh;
            let needs_any = state.supply() < waiting;
            let wants_spare = state.supply() < waiting + spares && state.alive < bound;
            if !(needs_fresh || needs_any || wants_spare) {
                break;
            }
            if needs_fresh && state.alive >= bound {
                // A reset session no waiting test takes makes way for the fresh one: dropping
                // its ticket quits it.
                let reset = state.idle.iter().filter(|ticket| !is_fresh(ticket)).count();
                if reset > state.waiting.any
                    && let Some(position) = state.idle.iter().position(|ticket| !is_fresh(ticket))
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
    /// ready. `None` if the run is cancelled first: a cancelled run drops the sessions being
    /// created or reset, so the one a test waits for may never arrive.
    pub(super) async fn take(&self, fresh: bool, keep_starting: bool) -> Option<TicketResult> {
        {
            let mut state = self.lock();
            let upcoming = state.upcoming.count(fresh);
            *upcoming = upcoming.saturating_sub(1);
            *state.waiting.count(fresh) += 1;
            self.replenish_locked(&mut state, keep_starting);
        }
        loop {
            let mut available = pin!(self.available.notified());
            available.as_mut().enable();
            {
                let mut state = self.lock();
                if let Some(ticket) = state.take_idle(fresh) {
                    *state.waiting.count(fresh) -= 1;
                    if ticket.is_ok() {
                        state.running += 1;
                    }
                    self.replenish_locked(&mut state, keep_starting);
                    return Some(ticket);
                }
            }
            tokio::select! {
                () = available => {}
                () = self.cancellation.cancelled() => {
                    *self.lock().waiting.count(fresh) -= 1;
                    return None;
                }
            }
        }
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
                {
                    let mut state = self.lock();
                    state.creating -= 1;
                    if !state.closed {
                        state.idle.push_back(ticket);
                    }
                }
                self.available.notify_waiters();
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
    pub(super) fn session_ended(&self, keep_starting: bool) {
        let mut state = self.lock();
        state.alive -= 1;
        self.replenish_locked(&mut state, keep_starting);
    }

    /// Whether the session of a finished test should be reset: while tests (not needing a fresh
    /// session) are still to run, otherwise it quits. If so, it counts as a session on its way to
    /// the pool until [`Self::deliver_reset`] or [`Self::reset_failed`]. Claimed before the next
    /// test starts, so that the pool doesn't create a session for that test in the meantime.
    pub(super) fn claim_reset(&self, keep_starting: bool) -> bool {
        let mut state = self.lock();
        let wanted = keep_starting && !state.closed && state.waiting.any + state.upcoming.any > 0;
        if wanted {
            state.resetting += 1;
        }
        wanted
    }

    /// Offer a session reset after its test. The pool keeps it only while it has fewer sessions
    /// (running a test, ready or on their way) than tests that can run at the same time and
    /// spares need: otherwise it drops the ticket, and the session quits.
    pub(super) fn deliver_reset(&self, ticket: Ticket, keep_starting: bool) {
        {
            let mut state = self.lock();
            state.resetting -= 1;
            let spares = if keep_starting {
                self.spare.min(state.upcoming.total())
            } else {
                0
            };
            let running_at_once = self
                .max_concurrency
                .min(state.running + state.waiting.total() + state.upcoming.total());
            if state.closed || state.running + state.supply() >= running_at_once + spares {
                return;
            }
            state.idle.push_back(Ok(ticket));
        }
        self.available.notify_waiters();
    }

    /// A claimed reset failed, so the session quits: create another one if tests need it.
    pub(super) fn reset_failed(&self, keep_starting: bool) {
        let mut state = self.lock();
        state.resetting -= 1;
        self.replenish_locked(&mut state, keep_starting);
    }

    /// Create no further sessions and let idle ones quit.
    pub(super) fn close(&self) {
        {
            let mut state = self.lock();
            state.closed = true;
            state.idle.clear();
        }
        let _ = self.requests.send(None);
    }
}

#[cfg(test)]
mod tests {
    use assertr::prelude::*;

    use super::*;

    fn pool(upcoming: Upcoming, cancellation: &CancellationToken) -> SessionPool {
        let (requests, _) = mpsc::unbounded_channel();
        SessionPool::new(1, 0, upcoming, None, requests, cancellation.clone())
    }

    /// A pool running one test at a time with one spare, and its session requests.
    fn pool_with_spare(
        upcoming: Upcoming,
    ) -> (SessionPool, mpsc::UnboundedReceiver<Option<WorkerRequest>>) {
        let (requests, requested) = mpsc::unbounded_channel();
        let pool = SessionPool::new(1, 1, upcoming, None, requests, CancellationToken::new());
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
        pool.replenish(true);
        assert_that!(requested(&mut requests)).is_equal_to(1);
        pool.deliver(Delivery::Pool, Ok(ticket(created).0));

        // The first test takes the spare, and the pool creates the next one.
        assert_that!(pool.take(false, true).await.is_some()).is_true();
        assert_that!(requested(&mut requests)).is_equal_to(1);
        pool.deliver(Delivery::Pool, Ok(ticket(created).0));
        pool.test_finished();

        // While the first test's session quits, two browsers are open: the next test takes the
        // spare, and the pool creates no further one until the session ended.
        assert_that!(pool.take(false, true).await.is_some()).is_true();
        assert_that!(requested(&mut requests)).is_equal_to(0);
        pool.session_ended(true);
        assert_that!(requested(&mut requests)).is_equal_to(1);
    }

    #[tokio::test]
    async fn fresh_sessions_replace_idle_reset_ones_at_the_bound() {
        let (pool, mut requests) = pool_with_spare(Upcoming { any: 0, fresh: 1 });
        let (reset, reset_jobs) = ticket(SessionPreparation::Reset(std::time::Duration::ZERO));
        {
            let mut state = pool.lock();
            state.alive = 2;
            state.idle.push_back(Ok(reset));
        }

        let take = pin!(pool.take(true, true));
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
        assert_that!(pool.claim_reset(true)).is_true();
        let take = pool.take(false, true);
        cancellation.cancel();

        assert_that!(take.await.is_none()).is_true();
        assert_that!(pool.lock().waiting.total()).is_equal_to(0);
    }
}

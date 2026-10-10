//! The pool of sessions kept ready for starting tests.

use std::{
    collections::VecDeque,
    pin::pin,
    sync::{Mutex, PoisonError},
    time::Instant,
};

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
    ) -> Self {
        Self {
            state: Mutex::new(PoolState {
                idle: VecDeque::new(),
                creating: 0,
                resetting: 0,
                running: 0,
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
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PoolState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Create sessions for waiting tests (fresh ones for tests that need them) and, unless tests
    /// stopped starting, spares for upcoming ones.
    pub(super) fn replenish(&self, keep_starting: bool) {
        self.replenish_locked(&mut self.lock(), keep_starting);
    }

    fn replenish_locked(&self, state: &mut PoolState, keep_starting: bool) {
        let spares = if keep_starting {
            self.spare.min(state.upcoming.total())
        } else {
            0
        };
        while !state.closed
            && (state.supply() < state.waiting.total() + spares
                || state.fresh_supply() < state.waiting.fresh)
        {
            state.creating += 1;
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
    /// ready.
    pub(super) async fn take(&self, fresh: bool, keep_starting: bool) -> TicketResult {
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
                    return ticket;
                }
            }
            available.await;
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

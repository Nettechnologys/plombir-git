//! One ceiling on the git processes this server runs for its clients.
//!
//! Every `git-upload-pack` and `git-receive-pack` session — over Smart HTTP or
//! over SSH — runs a `git` child of its own (`pack-objects`, `index-pack`),
//! with the CPU and memory that costs. Nothing bounded how many ran at once: an
//! anonymous loop of fetches against a public repository started one process
//! per request (card_444288887c81), and one authenticated SSH connection could
//! multiplex hundreds of channels with a process behind each
//! (card_b14a241b1e18). A cheap request buying expensive work without a ceiling
//! is the same class as the Argon2 flood (card_94ed05581655), and it gets the
//! same answer: a process-wide bound, and a share per source inside it.
//!
//! Both transports draw on [`global`], so the bound is on the machine, not on
//! one door. There is no queue: a session that finds no place is refused at
//! once (HTTP `503` with `Retry-After`, SSH a failed command with the reason on
//! stderr) instead of holding a connection open while it waits.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

/// Who a git session is charged to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionSource {
    /// An authenticated account, whichever transport and address it uses.
    Account(i64),
    /// An anonymous (or deploy-key) client, by its address — aggregated with
    /// [`crate::net::abuse_source`], so an IPv6 host is its /64.
    Address(IpAddr),
}

impl SessionSource {
    /// The account when there is one, the client address otherwise.
    pub fn of(account: Option<i64>, address: Option<IpAddr>) -> Option<Self> {
        match (account, address) {
            (Some(id), _) => Some(Self::Account(id)),
            (None, Some(ip)) => Some(Self::Address(crate::net::abuse_source(ip))),
            (None, None) => None,
        }
    }
}

/// No place for another git session right now. Nothing was started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitSessionsSaturated {
    /// Every place on the server is taken.
    Server,
    /// This source already holds its share.
    Source,
}

impl GitSessionsSaturated {
    /// What the client is told. The same words on both transports.
    pub fn client_message(self) -> &'static str {
        match self {
            Self::Server => {
                "the server is running as many git operations as it can; try again shortly"
            }
            Self::Source => {
                "you already have as many git operations running on this server as one client may; \
                 try again when one finishes"
            }
        }
    }
}

impl std::fmt::Display for GitSessionsSaturated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.client_message())
    }
}

impl std::error::Error for GitSessionsSaturated {}

#[derive(Default)]
struct Running {
    total: usize,
    by_source: HashMap<SessionSource, usize>,
}

/// The bound itself. [`global`] is the one production uses.
pub struct GitSessionLimiter {
    capacity: usize,
    per_source: usize,
    running: Arc<Mutex<Running>>,
}

fn lock(running: &Mutex<Running>) -> MutexGuard<'_, Running> {
    // The critical sections only add and subtract; a panic cannot leave the
    // counts half-written, so a poisoned lock still holds the truth.
    running.lock().unwrap_or_else(PoisonError::into_inner)
}

impl GitSessionLimiter {
    /// `capacity` sessions in all, at most `per_source` of them for any one
    /// source. Both are at least one.
    pub fn new(capacity: usize, per_source: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            capacity,
            per_source: per_source.clamp(1, capacity),
            running: Arc::default(),
        }
    }

    /// Take a place for one session, or say at once why there is none.
    ///
    /// `source: None` (no account, no address — an in-process caller) is
    /// bound only by the total.
    pub fn try_acquire(
        &self,
        source: Option<SessionSource>,
    ) -> Result<GitSessionPermit, GitSessionsSaturated> {
        let mut running = lock(&self.running);
        if running.total >= self.capacity {
            return Err(GitSessionsSaturated::Server);
        }
        if let Some(source) = source {
            let held = running.by_source.entry(source).or_insert(0);
            if *held >= self.per_source {
                return Err(GitSessionsSaturated::Source);
            }
            *held += 1;
        }
        running.total += 1;
        Ok(GitSessionPermit {
            running: Arc::clone(&self.running),
            source,
        })
    }
}

/// One running git session. Dropping it gives the place back — hold it for as
/// long as the `git` child can run.
#[must_use = "the place is given back as soon as the permit is dropped"]
pub struct GitSessionPermit {
    running: Arc<Mutex<Running>>,
    source: Option<SessionSource>,
}

impl Drop for GitSessionPermit {
    fn drop(&mut self) {
        let mut running = lock(&self.running);
        running.total -= 1;
        if let Some(source) = self.source {
            if let Entry::Occupied(mut held) = running.by_source.entry(source) {
                *held.get_mut() -= 1;
                if *held.get() == 0 {
                    held.remove();
                }
            }
        }
    }
}

impl std::fmt::Debug for GitSessionPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitSessionPermit")
            .field("source", &self.source)
            .finish()
    }
}

/// Sessions per core the server runs at once. `pack-objects` is itself
/// multi-threaded and mostly waits on the client or the disk, so a couple per
/// core keeps the machine busy without letting the git children crowd out the
/// server that spawned them.
const SESSIONS_PER_CORE: usize = 2;

/// The floor, for one- and two-core hosts: a handful of clones must still be
/// able to run side by side.
const MIN_SESSIONS: usize = 8;

/// The process-wide limiter both git transports draw on.
///
/// One source may hold half of it, so a single client — an anonymous address
/// looping fetches, one account multiplexing SSH channels — always leaves the
/// other half to everybody else.
pub fn global() -> &'static GitSessionLimiter {
    static LIMITER: OnceLock<GitSessionLimiter> = OnceLock::new();
    LIMITER.get_or_init(|| {
        let cores = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
        let capacity = (cores * SESSIONS_PER_CORE).max(MIN_SESSIONS);
        GitSessionLimiter::new(capacity, capacity / 2)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(text: &str) -> Option<SessionSource> {
        SessionSource::of(None, Some(text.parse().unwrap()))
    }

    #[test]
    fn the_total_is_a_ceiling_and_a_drop_gives_the_place_back() {
        let limiter = GitSessionLimiter::new(2, 2);
        let first = limiter.try_acquire(None).unwrap();
        let _second = limiter.try_acquire(None).unwrap();
        assert_eq!(
            limiter.try_acquire(None).unwrap_err(),
            GitSessionsSaturated::Server
        );
        drop(first);
        assert!(limiter.try_acquire(None).is_ok());
    }

    #[test]
    fn one_source_cannot_take_more_than_its_share() {
        let limiter = GitSessionLimiter::new(4, 2);
        let flooding = address("203.0.113.9");
        let _a = limiter.try_acquire(flooding).unwrap();
        let _b = limiter.try_acquire(flooding).unwrap();
        assert_eq!(
            limiter.try_acquire(flooding).unwrap_err(),
            GitSessionsSaturated::Source
        );
        // Everyone else still gets in.
        assert!(limiter.try_acquire(address("198.51.100.1")).is_ok());
        assert!(limiter
            .try_acquire(SessionSource::of(Some(7), None))
            .is_ok());
    }

    #[test]
    fn an_ipv6_host_is_one_source_across_its_64() {
        let limiter = GitSessionLimiter::new(8, 1);
        let _held = limiter.try_acquire(address("2001:db8:1:2::1")).unwrap();
        assert_eq!(
            limiter
                .try_acquire(address("2001:db8:1:2::ffff"))
                .unwrap_err(),
            GitSessionsSaturated::Source
        );
    }

    #[test]
    fn an_account_is_charged_whatever_address_it_comes_from() {
        assert_eq!(
            SessionSource::of(Some(7), Some("203.0.113.9".parse().unwrap())),
            Some(SessionSource::Account(7))
        );
    }

    #[test]
    fn the_global_limiter_leaves_half_to_everyone_else() {
        let limiter = global();
        assert!(limiter.capacity >= MIN_SESSIONS);
        assert_eq!(limiter.per_source, limiter.capacity / 2);
    }
}

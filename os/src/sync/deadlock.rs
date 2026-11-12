//! Deadlock detection based on the banker's algorithm.
//!
//! Every mutex is a resource class with one instance and every semaphore a
//! class with as many instances as it was created with. Per thread the state
//! records how many instances of each class it holds (`allocation`) and which
//! instances it is blocked on (`need`).
//!
//! A lock/down that would block first asks [`DeadlockState::try_reserve`],
//! which runs the safety algorithm with that wait edge already in place. If
//! the state would be unsafe the request is refused, which turns a cyclic
//! wait into an error instead of a hang.

use alloc::{vec, vec::Vec};

/// Per-process deadlock detection state.
pub struct DeadlockState {
    enabled: bool,
    /// instances of each class nobody holds right now
    available: Vec<i32>,
    /// `allocation[thread][resource]`: instances held by a thread
    allocation: Vec<Vec<i32>>,
    /// `need[thread][resource]`: 1 where a thread is blocked on that class
    need: Vec<Vec<i32>>,
    /// resource class backing each mutex slot
    mutex_res: Vec<Option<usize>>,
    /// resource class backing each semaphore slot
    sem_res: Vec<Option<usize>>,
}

impl DeadlockState {
    /// Create a disabled, empty detection state.
    pub fn new() -> Self {
        Self {
            enabled: false,
            available: Vec::new(),
            allocation: Vec::new(),
            need: Vec::new(),
            mutex_res: Vec::new(),
            sem_res: Vec::new(),
        }
    }

    /// Turn detection on or off.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Whether detection is on.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Register mutex `id` as a one-instance resource class.
    pub fn register_mutex(&mut self, id: usize) {
        let res = self.ensure_resource(1);
        while self.mutex_res.len() <= id {
            self.mutex_res.push(None);
        }
        self.mutex_res[id] = Some(res);
    }

    /// Register semaphore `id` as a class of `capacity` instances.
    pub fn register_sem(&mut self, id: usize, capacity: i32) {
        let res = self.ensure_resource(capacity);
        while self.sem_res.len() <= id {
            self.sem_res.push(None);
        }
        self.sem_res[id] = Some(res);
    }

    /// Resource class of mutex `id`, if it has been registered.
    pub fn res_of_mutex(&self, id: usize) -> Option<usize> {
        self.mutex_res.get(id).copied().flatten()
    }

    /// Resource class of semaphore `id`, if it has been registered.
    pub fn res_of_sem(&self, id: usize) -> Option<usize> {
        self.sem_res.get(id).copied().flatten()
    }

    /// Note that `tid` no longer waits for anything.
    ///
    /// A thread only ever blocks on one resource, so taking any instance
    /// clears its whole `need` row.
    fn clear_need(&mut self, tid: usize) {
        for slot in self.need[tid].iter_mut() {
            *slot = 0;
        }
    }

    /// `tid` took a free instance of `res`, either straight from the pool or
    /// when it was woken with ownership of it.
    pub fn acquire(&mut self, tid: usize, res: usize, from_pool: bool) {
        self.ensure_thread(tid);
        self.allocation[tid][res] += 1;
        if from_pool {
            self.available[res] -= 1;
        }
        self.clear_need(tid);
    }

    /// `tid` released an instance it held.
    ///
    /// `to_pool` is false when the instance goes straight to a waiter, in
    /// which case the free count does not change.
    pub fn release(&mut self, tid: usize, res: usize, to_pool: bool) {
        self.ensure_thread(tid);
        self.allocation[tid][res] -= 1;
        if to_pool {
            self.available[res] += 1;
        }
    }

    /// An `up` created one instance of `res`.
    ///
    /// Unlike [`DeadlockState::release`] this does not touch the caller's
    /// allocation: producing a token is not giving one back.
    pub fn produce(&mut self, res: usize, to_pool: bool) {
        if to_pool {
            self.available[res] += 1;
        }
    }

    /// Record `tid` as blocked on `res`, keeping the safety verdict.
    ///
    /// Returns false — and leaves the state untouched — when waiting for
    /// `res` would make the state unsafe, i.e. when granting it could
    /// deadlock.
    pub fn try_reserve(&mut self, tid: usize, res: usize) -> bool {
        self.ensure_thread(tid);
        let previous = self.need[tid][res];
        self.need[tid][res] = previous + 1;
        if self.is_safe() {
            true
        } else {
            self.need[tid][res] = previous;
            false
        }
    }

    /// Grow the per-thread matrices so row `tid` exists.
    fn ensure_thread(&mut self, tid: usize) {
        while self.allocation.len() <= tid {
            self.allocation.push(vec![0; self.available.len()]);
            self.need.push(vec![0; self.available.len()]);
        }
    }

    /// Append a resource class with `capacity` instances and return its index.
    fn ensure_resource(&mut self, capacity: i32) -> usize {
        let res = self.available.len();
        self.available.push(capacity);
        for row in self.allocation.iter_mut() {
            row.push(0);
        }
        for row in self.need.iter_mut() {
            row.push(0);
        }
        res
    }

    /// The banker's safety algorithm: can every thread still finish?
    fn is_safe(&self) -> bool {
        let threads = self.need.len();
        let resources = self.available.len();
        let mut work = self.available.clone();
        let mut finish = vec![false; threads];
        loop {
            let mut progressed = false;
            for i in 0..threads {
                if finish[i] || (0..resources).any(|j| self.need[i][j] > work[j]) {
                    continue;
                }
                for j in 0..resources {
                    work[j] += self.allocation[i][j];
                }
                finish[i] = true;
                progressed = true;
            }
            if !progressed {
                break;
            }
        }
        finish.iter().all(|&done| done)
    }
}

impl Default for DeadlockState {
    fn default() -> Self {
        Self::new()
    }
}

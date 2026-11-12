//! Mutex (spin-like and blocking(sleep))

use super::UPSafeCell;
use crate::task::TaskControlBlock;
use crate::task::{block_current_and_run_next, current_task, suspend_current_and_run_next};
use crate::task::wakeup_task;
use alloc::{collections::VecDeque, sync::Arc};

/// Mutex trait
pub trait Mutex: Sync + Send {
    /// Lock the mutex
    fn lock(&self);
    /// Unlock the mutex
    fn unlock(&self);
    /// Take the mutex if it is free, otherwise switch away and retry.
    ///
    /// Returns whether a wait had to happen. The check and the enqueue share
    /// one critical section, so a wakeup can never be lost between them.
    fn take_or_wait(&self) -> bool {
        false
    }
    /// Whether an [`Mutex::unlock`] would hand the lock to a waiting task.
    fn hands_off(&self) -> bool {
        false
    }
    /// Whether somebody currently holds the mutex.
    fn is_locked(&self) -> bool {
        false
    }
}

/// Spinlock Mutex struct
pub struct MutexSpin {
    locked: UPSafeCell<bool>,
}

impl MutexSpin {
    /// Create a new spinlock mutex
    pub fn new() -> Self {
        Self {
            locked: unsafe { UPSafeCell::new(false) },
        }
    }
}

impl Mutex for MutexSpin {
    /// Lock the spinlock mutex
    fn lock(&self) {
        trace!("kernel: MutexSpin::lock");
        loop {
            let mut locked = self.locked.exclusive_access();
            if *locked {
                drop(locked);
                suspend_current_and_run_next();
                continue;
            } else {
                *locked = true;
                return;
            }
        }
    }

    fn unlock(&self) {
        trace!("kernel: MutexSpin::unlock");
        let mut locked = self.locked.exclusive_access();
        *locked = false;
    }

    fn take_or_wait(&self) -> bool {
        let mut waited = false;
        loop {
            let mut locked = self.locked.exclusive_access();
            if *locked {
                drop(locked);
                suspend_current_and_run_next();
                waited = true;
            } else {
                *locked = true;
                return waited;
            }
        }
    }

    fn is_locked(&self) -> bool {
        *self.locked.exclusive_access()
    }
}

/// Blocking Mutex struct
pub struct MutexBlocking {
    inner: UPSafeCell<MutexBlockingInner>,
}

pub struct MutexBlockingInner {
    locked: bool,
    wait_queue: VecDeque<Arc<TaskControlBlock>>,
}

impl MutexBlocking {
    /// Create a new blocking mutex
    pub fn new() -> Self {
        trace!("kernel: MutexBlocking::new");
        Self {
            inner: unsafe {
                UPSafeCell::new(MutexBlockingInner {
                    locked: false,
                    wait_queue: VecDeque::new(),
                })
            },
        }
    }
}

impl Mutex for MutexBlocking {
    /// lock the blocking mutex
    fn lock(&self) {
        trace!("kernel: MutexBlocking::lock");
        let mut mutex_inner = self.inner.exclusive_access();
        if mutex_inner.locked {
            mutex_inner.wait_queue.push_back(current_task().unwrap());
            drop(mutex_inner);
            block_current_and_run_next();
        } else {
            mutex_inner.locked = true;
        }
    }

    /// unlock the blocking mutex
    fn unlock(&self) {
        trace!("kernel: MutexBlocking::unlock");
        let mut mutex_inner = self.inner.exclusive_access();
        assert!(mutex_inner.locked);
        if let Some(waking_task) = mutex_inner.wait_queue.pop_front() {
            wakeup_task(waking_task);
        } else {
            mutex_inner.locked = false;
        }
    }

    fn take_or_wait(&self) -> bool {
        trace!("kernel: MutexBlocking::take_or_wait");
        let mut mutex_inner = self.inner.exclusive_access();
        if mutex_inner.locked {
            mutex_inner.wait_queue.push_back(current_task().unwrap());
            drop(mutex_inner);
            block_current_and_run_next();
            true
        } else {
            mutex_inner.locked = true;
            false
        }
    }

    fn hands_off(&self) -> bool {
        !self.inner.exclusive_access().wait_queue.is_empty()
    }

    fn is_locked(&self) -> bool {
        self.inner.exclusive_access().locked
    }
}

/// Enable or disable deadlock detection for the calling process.
///
/// Returns `-1` if `is_enable` is not 0 or 1, and 0 on success.
pub fn set_deadlock_detect(is_enable: i32) -> i32 {
    if is_enable != 0 && is_enable != 1 {
        return -1;
    }
    let process = current_task().unwrap().process.upgrade().unwrap();
    process
        .inner_exclusive_access()
        .deadlock
        .set_enabled(is_enable == 1);
    0
}

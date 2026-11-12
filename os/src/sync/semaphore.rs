//! Semaphore

use crate::sync::UPSafeCell;
use crate::task::{block_current_and_run_next, current_task, wakeup_task, TaskControlBlock};
use alloc::{collections::VecDeque, sync::Arc};

/// semaphore structure
pub struct Semaphore {
    /// semaphore inner
    pub inner: UPSafeCell<SemaphoreInner>,
}

pub struct SemaphoreInner {
    pub count: isize,
    pub wait_queue: VecDeque<Arc<TaskControlBlock>>,
}

impl Semaphore {
    /// Create a new semaphore
    pub fn new(res_count: usize) -> Self {
        trace!("kernel: Semaphore::new");
        Self {
            inner: unsafe {
                UPSafeCell::new(SemaphoreInner {
                    count: res_count as isize,
                    wait_queue: VecDeque::new(),
                })
            },
        }
    }

    /// up operation of semaphore
    pub fn up(&self) {
        trace!("kernel: Semaphore::up");
        let mut inner = self.inner.exclusive_access();
        inner.count += 1;
        if inner.count <= 0 {
            if let Some(task) = inner.wait_queue.pop_front() {
                wakeup_task(task);
            }
        }
    }

    /// down operation of semaphore
    pub fn down(&self) {
        trace!("kernel: Semaphore::down");
        let mut inner = self.inner.exclusive_access();
        inner.count -= 1;
        if inner.count < 0 {
            inner.wait_queue.push_back(current_task().unwrap());
            drop(inner);
            block_current_and_run_next();
        }
    }

    /// Take one instance if one is free, otherwise switch away and retry.
    ///
    /// Returns whether a wait had to happen. The check and the enqueue share
    /// one critical section, so a wakeup can never be lost between them.
    pub fn take_or_wait(&self) -> bool {
        let mut inner = self.inner.exclusive_access();
        if inner.count > 0 {
            inner.count -= 1;
            false
        } else {
            inner.count -= 1;
            inner.wait_queue.push_back(current_task().unwrap());
            drop(inner);
            block_current_and_run_next();
            true
        }
    }

    /// Whether a `down` right now would succeed without blocking.
    pub fn is_available(&self) -> bool {
        self.inner.exclusive_access().count > 0
    }

    /// Would the next [`Semaphore::up`] hand its instance to a waiter?
    pub fn will_hand_off(&self) -> bool {
        let inner = self.inner.exclusive_access();
        inner.count + 1 <= 0 && !inner.wait_queue.is_empty()
    }
}

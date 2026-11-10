//! Implementation of [`TaskManager`]
use super::TaskControlBlock;
use crate::sync::UPSafeCell;
use alloc::vec::Vec;
use alloc::sync::Arc;
use lazy_static::*;
/// A array of `TaskControlBlock` that is thread-safe
pub struct TaskManager {
    ready_queue: Vec<Arc<TaskControlBlock>>,
}

/// A stride scheduler.
impl TaskManager {
    /// Creat an empty TaskManager
    pub fn new() -> Self {
        Self {
            ready_queue: Vec::new(),
        }
    }
    /// Add process back to ready queue
    pub fn add(&mut self, task: Arc<TaskControlBlock>) {
        self.ready_queue.push(task);
    }
    /// Take the runnable process with the smallest stride out of the ready
    /// queue, charging it one pass of stride.
    ///
    /// Scanning the whole queue is `O(n)`, which is more than fast enough for
    /// the few processes this kernel runs and keeps the invariant obvious:
    /// the process that has received the least service so far runs next.
    pub fn fetch(&mut self) -> Option<Arc<TaskControlBlock>> {
        let mut min_stride = usize::MAX;
        let mut min_idx = None;
        for (idx, task) in self.ready_queue.iter().enumerate() {
            let stride = task.inner_exclusive_access().stride;
            if stride < min_stride {
                min_stride = stride;
                min_idx = Some(idx);
            }
        }
        let idx = min_idx?;
        let task = self.ready_queue.remove(idx);
        // the running process advances by its pass, so low-priority processes
        // (large pass) fall behind faster and get scheduled less often
        {
            let mut inner = task.inner_exclusive_access();
            inner.stride += inner.pass;
        }
        Some(task)
    }
}

lazy_static! {
    /// TASK_MANAGER instance through lazy_static!
    pub static ref TASK_MANAGER: UPSafeCell<TaskManager> =
        unsafe { UPSafeCell::new(TaskManager::new()) };
}

/// Add process to ready queue
pub fn add_task(task: Arc<TaskControlBlock>) {
    //trace!("kernel: TaskManager::add_task");
    TASK_MANAGER.exclusive_access().add(task);
}

/// Take a process out of the ready queue
pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
    //trace!("kernel: TaskManager::fetch_task");
    TASK_MANAGER.exclusive_access().fetch()
}

use crate::sync::{
    set_deadlock_detect, Condvar, DeadlockState, Mutex, MutexBlocking, MutexSpin, Semaphore,
};
use crate::task::{block_current_and_run_next, current_process, current_task};
use crate::timer::{add_timer, get_time_ms};
use alloc::sync::Arc;

/// Returned by `mutex_lock`/`semaphore_down` when the request would deadlock.
const DEADLOCK: isize = -0xDEAD;

/// Current thread id, used to ticket resources in the deadlock state.
fn gettid() -> usize {
    current_task()
        .unwrap()
        .inner_exclusive_access()
        .res
        .as_ref()
        .unwrap()
        .tid
}

/// Update the calling process's deadlock state in a short critical section.
///
/// Kept separate from the lock objects: their `take_or_wait` may block and
/// switch tasks, which must never happen while the process is borrowed.
fn set_state(update: impl FnOnce(&mut DeadlockState)) {
    let process = current_process();
    let mut inner = process.inner_exclusive_access();
    update(&mut inner.deadlock);
}

/// sleep syscall
pub fn sys_sleep(ms: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_sleep",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        gettid()
    );
    let expire_ms = get_time_ms() + ms;
    let task = current_task().unwrap();
    add_timer(expire_ms, task);
    block_current_and_run_next();
    0
}
/// mutex create syscall
pub fn sys_mutex_create(blocking: bool) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_mutex_create",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        gettid()
    );
    let process = current_process();
    let mutex: Option<Arc<dyn Mutex>> = if !blocking {
        Some(Arc::new(MutexSpin::new()))
    } else {
        Some(Arc::new(MutexBlocking::new()))
    };
    let mut process_inner = process.inner_exclusive_access();
    let id = if let Some(id) = process_inner
        .mutex_list
        .iter()
        .enumerate()
        .find(|(_, item)| item.is_none())
        .map(|(id, _)| id)
    {
        process_inner.mutex_list[id] = mutex;
        id
    } else {
        process_inner.mutex_list.push(mutex);
        process_inner.mutex_list.len() - 1
    };
    process_inner.deadlock.register_mutex(id);
    id as isize
}
/// mutex lock syscall
pub fn sys_mutex_lock(mutex_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_mutex_lock",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        gettid()
    );
    let tid = gettid();
    // the veto runs under the same borrow as the state read, and before the
    // caller can possibly block, so the wait edge is always recorded
    let (mutex, res) = {
        let process = current_process();
        let mut inner = process.inner_exclusive_access();
        let mutex = Arc::clone(inner.mutex_list[mutex_id].as_ref().unwrap());
        let res = inner.deadlock.res_of_mutex(mutex_id);
        let busy = mutex.is_locked();
        if busy && inner.deadlock.is_enabled() {
            if let Some(res) = res {
                if !inner.deadlock.try_reserve(tid, res) {
                    return DEADLOCK;
                }
            }
        }
        (mutex, res)
    };
    // `take_or_wait` decides and acts under one borrow: returning false means
    // it took the mutex out of the free pool, returning true means it slept
    // until the holder handed the mutex over to us.
    let waited = mutex.take_or_wait();
    if let Some(res) = res {
        set_state(|state| state.acquire(tid, res, !waited));
    }
    0
}
/// mutex unlock syscall
pub fn sys_mutex_unlock(mutex_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_mutex_unlock",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        gettid()
    );
    let tid = gettid();
    let process = current_process();
    let mut inner = process.inner_exclusive_access();
    let mutex = Arc::clone(inner.mutex_list[mutex_id].as_ref().unwrap());
    // `hands_off` reports what the unlock is about to do, so read it first
    let hands_off = mutex.hands_off();
    mutex.unlock();
    if inner.deadlock.is_enabled() {
        if let Some(res) = inner.deadlock.res_of_mutex(mutex_id) {
            inner.deadlock.release(tid, res, !hands_off);
        }
    }
    0
}
/// semaphore create syscall
pub fn sys_semaphore_create(res_count: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_semaphore_create",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        gettid()
    );
    let process = current_process();
    let mut process_inner = process.inner_exclusive_access();
    let id = if let Some(id) = process_inner
        .semaphore_list
        .iter()
        .enumerate()
        .find(|(_, item)| item.is_none())
        .map(|(id, _)| id)
    {
        process_inner.semaphore_list[id] = Some(Arc::new(Semaphore::new(res_count)));
        id
    } else {
        process_inner
            .semaphore_list
            .push(Some(Arc::new(Semaphore::new(res_count))));
        process_inner.semaphore_list.len() - 1
    };
    process_inner.deadlock.register_sem(id, res_count as i32);
    id as isize
}
/// semaphore up syscall
pub fn sys_semaphore_up(sem_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_semaphore_up",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        gettid()
    );
    let process = current_process();
    let mut inner = process.inner_exclusive_access();
    let sem = Arc::clone(inner.semaphore_list[sem_id].as_ref().unwrap());
    // read before `up`, since it reports what `up` is about to do
    let hands_off = sem.will_hand_off();
    sem.up();
    if inner.deadlock.is_enabled() {
        if let Some(res) = inner.deadlock.res_of_sem(sem_id) {
            inner.deadlock.produce(res, !hands_off);
        }
    }
    0
}
/// semaphore down syscall
pub fn sys_semaphore_down(sem_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_semaphore_down",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        gettid()
    );
    let tid = gettid();
    let (sem, res) = {
        let process = current_process();
        let mut inner = process.inner_exclusive_access();
        let sem = Arc::clone(inner.semaphore_list[sem_id].as_ref().unwrap());
        let res = inner.deadlock.res_of_sem(sem_id);
        let busy = !sem.is_available();
        if busy && inner.deadlock.is_enabled() {
            if let Some(res) = res {
                if !inner.deadlock.try_reserve(tid, res) {
                    return DEADLOCK;
                }
            }
        }
        (sem, res)
    };
    // see `sys_mutex_lock`: false means the instance came from the pool,
    // true means it was handed over while we slept
    let waited = sem.take_or_wait();
    if let Some(res) = res {
        set_state(|state| state.acquire(tid, res, !waited));
    }
    0
}
/// condvar create syscall
pub fn sys_condvar_create() -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_condvar_create",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        gettid()
    );
    let process = current_process();
    let mut process_inner = process.inner_exclusive_access();
    let id = if let Some(id) = process_inner
        .condvar_list
        .iter()
        .enumerate()
        .find(|(_, item)| item.is_none())
        .map(|(id, _)| id)
    {
        process_inner.condvar_list[id] = Some(Arc::new(Condvar::new()));
        id
    } else {
        process_inner
            .condvar_list
            .push(Some(Arc::new(Condvar::new())));
        process_inner.condvar_list.len() - 1
    };
    id as isize
}
/// condvar signal syscall
pub fn sys_condvar_signal(condvar_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_condvar_signal",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        gettid()
    );
    let process = current_process();
    let process_inner = process.inner_exclusive_access();
    let condvar = Arc::clone(process_inner.condvar_list[condvar_id].as_ref().unwrap());
    drop(process_inner);
    condvar.signal();
    0
}
/// condvar wait syscall
pub fn sys_condvar_wait(condvar_id: usize, mutex_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_condvar_wait",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        gettid()
    );
    let process = current_process();
    let process_inner = process.inner_exclusive_access();
    let condvar = Arc::clone(process_inner.condvar_list[condvar_id].as_ref().unwrap());
    let mutex = Arc::clone(process_inner.mutex_list[mutex_id].as_ref().unwrap());
    drop(process_inner);
    condvar.wait(mutex);
    0
}
/// enable deadlock detection syscall
pub fn sys_enable_deadlock_detect(enabled: usize) -> isize {
    trace!("kernel: sys_enable_deadlock_detect");
    set_deadlock_detect(enabled as i32) as isize
}

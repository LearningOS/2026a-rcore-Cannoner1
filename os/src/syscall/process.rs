//! Process management syscalls
use alloc::sync::Arc;

use crate::{
    loader::get_app_data_by_name,
    mm::{
        translated_byte_buffer, translated_refmut, translated_str, MapPermission, PageTable,
        PageTableEntry, PTEFlags, VirtAddr,
    },
    task::{
        add_task, current_task, current_user_token, exit_current_and_run_next, set_priority_current,
        spawn_current, suspend_current_and_run_next,
    },
    timer::get_time_us,
};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// Look up the page table entry that maps `addr` in the current task.
///
/// Returns `None` unless the page is present and marked `U`. The `U` test is
/// what makes this safe: kernel addresses are absent from a user page table,
/// and the trampoline and trap-context pages *are* present but lack `U`, so
/// without it a bogus pointer such as `usize::MAX` (which masks onto SV39's
/// top page) would pass as user memory. The upper half of the address space
/// is rejected outright for the same reason.
fn user_pte(addr: usize) -> Option<PageTableEntry> {
    if addr >= 1usize << 38 {
        return None;
    }
    let pte = PageTable::from_token(current_user_token()).translate(VirtAddr::from(addr).floor())?;
    if pte.is_valid() && (pte.flags() & PTEFlags::U) != PTEFlags::empty() {
        Some(pte)
    } else {
        None
    }
}

/// Is `[ptr, ptr + len)` writable by the current task?
fn user_writable(ptr: usize, len: usize) -> bool {
    check_user_range(ptr, len, |pte| pte.writable())
}

/// Walk every page touched by `[ptr, ptr + len)` and require `ok(pte)`.
///
/// The walk is page by page because the range may straddle a page boundary.
fn check_user_range(ptr: usize, len: usize, ok: impl Fn(&PageTableEntry) -> bool) -> bool {
    let end = match ptr.checked_add(len) {
        Some(end) => end,
        None => return false,
    };
    if len == 0 {
        return true;
    }
    let first = (ptr >> 12) << 12;
    let last = ((end - 1) >> 12) << 12;
    let mut page = first;
    loop {
        match user_pte(page) {
            Some(pte) if ok(&pte) => {}
            _ => return false,
        }
        if page == last {
            return true;
        }
        page += 4096;
    }
}

/// task exits and submit an exit code
pub fn sys_exit(exit_code: i32) -> ! {
    trace!("kernel:pid[{}] sys_exit", current_task().unwrap().pid.0);
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel:pid[{}] sys_yield", current_task().unwrap().pid.0);
    suspend_current_and_run_next();
    0
}

pub fn sys_getpid() -> isize {
    trace!("kernel: sys_getpid pid:{}", current_task().unwrap().pid.0);
    current_task().unwrap().pid.0 as isize
}

pub fn sys_fork() -> isize {
    trace!("kernel:pid[{}] sys_fork", current_task().unwrap().pid.0);
    let current_task = current_task().unwrap();
    let new_task = current_task.fork();
    let new_pid = new_task.pid.0;
    // modify trap context of new_task, because it returns immediately after switching
    let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
    // we do not have to move to next instruction since we have done it before
    // for child process, fork returns 0
    trap_cx.x[10] = 0;
    // add new task to scheduler
    add_task(new_task);
    new_pid as isize
}

pub fn sys_exec(path: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_exec", current_task().unwrap().pid.0);
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(data) = get_app_data_by_name(path.as_str()) {
        let task = current_task().unwrap();
        task.exec(data);
        0
    } else {
        -1
    }
}

/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
pub fn sys_waitpid(pid: isize, exit_code_ptr: *mut i32) -> isize {
    trace!("kernel::pid[{}] sys_waitpid [{}]", current_task().unwrap().pid.0, pid);
    let task = current_task().unwrap();
    // find a child process

    // ---- access current PCB exclusively
    let mut inner = task.inner_exclusive_access();
    if !inner
        .children
        .iter()
        .any(|p| pid == -1 || pid as usize == p.getpid())
    {
        return -1;
        // ---- release current PCB
    }
    let pair = inner.children.iter().enumerate().find(|(_, p)| {
        // ++++ temporarily access child PCB exclusively
        p.inner_exclusive_access().is_zombie() && (pid == -1 || pid as usize == p.getpid())
        // ++++ release child PCB
    });
    if let Some((idx, _)) = pair {
        let child = inner.children.remove(idx);
        // confirm that child will be deallocated after being removed from children list
        assert_eq!(Arc::strong_count(&child), 1);
        let found_pid = child.getpid();
        // ++++ temporarily access child PCB exclusively
        let exit_code = child.inner_exclusive_access().exit_code;
        // ++++ release child PCB
        *translated_refmut(inner.memory_set.token(), exit_code_ptr) = exit_code;
        found_pid as isize
    } else {
        -2
    }
    // ---- release current PCB automatically
}

/// get time with second and microsecond
///
/// `TimeVal` is written through the page table rather than through the raw
/// pointer: the kernel runs on its own address space and cannot dereference a
/// user virtual address directly, and the two `usize` fields may be split
/// across two pages.
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel:pid[{}] sys_get_time", current_task().unwrap().pid.0);
    let us = get_time_us();
    let time_val = TimeVal {
        sec: us / 1_000_000,
        usec: us % 1_000_000,
    };
    let src = unsafe {
        core::slice::from_raw_parts(
            &time_val as *const TimeVal as *const u8,
            core::mem::size_of::<TimeVal>(),
        )
    };
    if !user_writable(ts as usize, src.len()) {
        return -1;
    }
    let mut copied = 0;
    for page in translated_byte_buffer(current_user_token(), ts as *const u8, src.len()) {
        let n = page.len().min(src.len() - copied);
        page[..n].copy_from_slice(&src[copied..copied + n]);
        copied += n;
    }
    0
}

/// Map `len` bytes at `start` with permission bits `prot`.
///
/// `prot` bit 0 is readable, bit 1 writable, bit 2 executable. A zero `prot`,
/// any other bit set, or an unaligned `start` is rejected with `-1`.
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    trace!("kernel:pid[{}] sys_mmap", current_task().unwrap().pid.0);
    let task = current_task().unwrap();
    // `prot` may only contain the R/W/X bits, and at least one of them
    if prot == 0 || prot & !0b111 != 0 {
        return -1;
    }
    let start_va = VirtAddr::from(start);
    if !start_va.aligned() {
        return -1;
    }
    let mut perm = MapPermission::U;
    if prot & 0b001 != 0 {
        perm |= MapPermission::R;
    }
    if prot & 0b010 != 0 {
        perm |= MapPermission::W;
    }
    if prot & 0b100 != 0 {
        perm |= MapPermission::X;
    }
    let end_va = VirtAddr::from(start + len);
    if task
        .inner_exclusive_access()
        .memory_set
        .mmap(start_va, end_va, perm)
    {
        0
    } else {
        -1
    }
}

/// Unmap `len` bytes at `start`. Returns `-1` unless every page in the range
/// was mapped by this process.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    trace!("kernel:pid[{}] sys_munmap", current_task().unwrap().pid.0);
    let task = current_task().unwrap();
    let start_va = VirtAddr::from(start);
    if !start_va.aligned() {
        return -1;
    }
    let end_va = VirtAddr::from(start + len);
    if task
        .inner_exclusive_access()
        .memory_set
        .munmap(start_va, end_va)
    {
        0
    } else {
        -1
    }
}

/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel:pid[{}] sys_sbrk", current_task().unwrap().pid.0);
    if let Some(old_brk) = current_task().unwrap().change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}

/// Spawn a new process running the program named by the user string `path`.
///
/// Returns the child pid, or `-1` if no such program exists. Unlike
/// `fork + exec`, the child gets a brand new address space built directly from
/// the ELF image, so the parent's memory is never copied.
pub fn sys_spawn(path: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_spawn", current_task().unwrap().pid.0);
    let path = translated_str(current_user_token(), path);
    if let Some(data) = get_app_data_by_name(path.as_str()) {
        spawn_current(data)
    } else {
        -1
    }
}

/// Set the current process's scheduling priority.
///
/// `prio` must be `>= 2`; returns `prio` on success and `-1` otherwise. A
/// priority of 0 or 1 would make `pass` too large for the stride comparison
/// to stay meaningful, so both are rejected.
pub fn sys_set_priority(prio: isize) -> isize {
    trace!(
        "kernel:pid[{}] sys_set_priority",
        current_task().unwrap().pid.0
    );
    if prio < 2 {
        -1
    } else {
        set_priority_current(prio);
        prio
    }
}

//! Process management syscalls
//!
use alloc::sync::Arc;

use crate::{
    fs::{open_file, OpenFlags},
    mm::{
        translated_byte_buffer, translated_refmut, translated_str, MapPermission, PageTable,
        PageTableEntry, PTEFlags, VirtAddr,
    },
    task::{
        add_task, current_task, current_user_token, exit_current_and_run_next,
        suspend_current_and_run_next,
    },
    timer::get_time_us,
};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

pub fn sys_exit(exit_code: i32) -> ! {
    trace!("kernel:pid[{}] sys_exit", current_task().unwrap().pid.0);
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}

pub fn sys_yield() -> isize {
    //trace!("kernel: sys_yield");
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
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let all_data = app_inode.read_all();
        let task = current_task().unwrap();
        task.exec(all_data.as_slice());
        0
    } else {
        -1
    }
}

/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
pub fn sys_waitpid(pid: isize, exit_code_ptr: *mut i32) -> isize {
    //trace!("kernel: sys_waitpid");
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

/// get time with second and microsecond
///
/// `TimeVal` is written through the page table rather than through the raw
/// pointer: the kernel cannot dereference a user virtual address directly,
/// and the two `usize` fields may be split across two pages.
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

/// Map `len` bytes at `start` with the protection bits in `prot`
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    trace!("kernel:pid[{}] sys_mmap", current_task().unwrap().pid.0);
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
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    if inner.memory_set.mmap(start_va, end_va, perm) {
        0
    } else {
        -1
    }
}

/// Unmap `len` bytes starting at `start`
pub fn sys_munmap(start: usize, len: usize) -> isize {
    trace!("kernel:pid[{}] sys_munmap", current_task().unwrap().pid.0);
    let start_va = VirtAddr::from(start);
    if !start_va.aligned() {
        return -1;
    }
    let end_va = VirtAddr::from(start + len);
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    if inner.memory_set.munmap(start_va, end_va) {
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

/// Create a new process running the program at `path` and return its pid.
///
/// The child is forked from the caller (so it is linked to it as a child and
/// `wait`/`waitpid` can reap it) and then `exec`s the requested ELF, which
/// replaces the copied address space with a fresh one.
pub fn sys_spawn(path: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_spawn", current_task().unwrap().pid.0);
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let all_data = app_inode.read_all();
        let task = current_task().unwrap();
        let new_task = task.fork();
        new_task.exec(all_data.as_slice());
        let new_pid = new_task.getpid();
        // the child resumes as if returning from the syscall, i.e. spawn(2)
        // returns 0 there
        let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
        trap_cx.x[10] = 0;
        add_task(new_task);
        new_pid as isize
    } else {
        -1
    }
}

// YOUR JOB: Set task priority.
pub fn sys_set_priority(_prio: isize) -> isize {
    trace!(
        "kernel:pid[{}] sys_set_priority NOT IMPLEMENTED",
        current_task().unwrap().pid.0
    );
    -1
}

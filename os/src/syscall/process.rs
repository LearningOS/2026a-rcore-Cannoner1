//! Process management syscalls
use crate::mm::{translated_byte_buffer, PageTable, PageTableEntry, PTEFlags, VirtAddr};
use crate::task::{
    change_program_brk, current_user_token, exit_current_and_run_next, get_count,
    get_current_task, mmap_current, munmap_current, suspend_current_and_run_next,
};
use crate::timer::get_time_us;

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

/// Is `[ptr, ptr + len)` readable by the current task?
fn user_readable(ptr: usize, len: usize) -> bool {
    check_user_range(ptr, len, |pte| pte.readable())
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
pub fn sys_exit(_exit_code: i32) -> ! {
    trace!("kernel: sys_exit");
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// get time with second and microsecond
///
/// `TimeVal` is written through the page table rather than through the raw
/// pointer: the kernel cannot dereference a user virtual address directly,
/// and the two `usize` fields may be split across two pages.
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time");
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

/// Trace the memory of the current task, or count its syscalls.
///
/// * request 0: read the byte at `id`, `-1` if it is not readable
/// * request 1: write `data` as a byte at `id`, `-1` if it is not writable
/// * request 2: how many times has this task called syscall `id`
pub fn sys_trace(trace_request: usize, id: usize, data: usize) -> isize {
    trace!("kernel: sys_trace");
    match trace_request {
        // Read
        0 => {
            if !user_readable(id, 1) {
                return -1;
            }
            match translated_byte_buffer(current_user_token(), id as *const u8, 1).first_mut() {
                Some(page) => page[0] as isize,
                None => -1,
            }
        }
        // Write
        1 => {
            if !user_writable(id, 1) {
                return -1;
            }
            match translated_byte_buffer(current_user_token(), id as *const u8, 1).first_mut() {
                Some(page) => {
                    page[0] = data as u8;
                    0
                }
                None => -1,
            }
        }
        // Syscall count
        2 => get_count(get_current_task(), id),
        _ => -1,
    }
}

/// Map `len` bytes at `start` with permission bits `port`.
///
/// `port` uses a different encoding from the kernel's `MapPermission`:
/// bit 0 is readable, bit 1 writable, bit 2 executable. Any other bit set,
/// a zero `port`, or an unaligned `start` is rejected with `-1`.
pub fn sys_mmap(start: usize, len: usize, port: usize) -> isize {
    trace!("kernel: sys_mmap");
    mmap_current(start, len, port)
}

/// Unmap `len` bytes at `start`. `-1` if any page in the range is not mapped.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    trace!("kernel: sys_munmap");
    munmap_current(start, len)
}

/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel: sys_sbrk");
    if let Some(old_brk) = change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}

/// lazy allocation entry point (reserved for the ch4 问答练习).
pub fn sys_lazy(size: i32) -> isize {
    sys_sbrk(size)
}

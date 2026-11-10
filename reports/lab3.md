1. 在完成本次实验的过程（含此前学习的过程）中，我曾分别与 以下各位 就（与本次实验相关的）以下方面做过交流，还在代码中对应的位置以注释形式记录了具体的交流对象及内容：

《你交流的对象说明》

2. 此外，我也参考了 以下资料 ，还在代码中对应的位置以注释形式记录了具体的参考来源及内容：

《你参考的资料说明》

3. 我独立完成了本次实验除以上方面之外的所有工作，包括代码与文档。 我清楚地知道，从以上方面获得的信息在一定程度上降低了实验难度，可能会影响起评分。

4. 我从未使用过他人的代码，不管是原封不动地复制，还是经过了某些等价转换。 我未曾也不会向他人（含此后各届同学）复制或公开我的实验代码，我有义务妥善保管好它们。 我提交至本实验的评测系统的代码，均无意于破坏或妨碍任何计算机系统的正常运转。 我清楚地知道，以上情况均为本课程纪律所禁止，若违反，对应的实验成绩将按“-100”分计。

## 总结功能

本章在进程管理的基础上实现了 spawn 系统调用和 stride 调度。spawn 直接按文件名取 ELF 数据，用 MemorySet::from_elf 建立全新地址空间，不再复制父进程内存，并登记为当前进程的子进程以便 waitpid 回收。调度方面，TCB 新增 stride、pass、priority 字段，初始优先级 16、stride 为 0；就绪队列改为每次挑选 stride 最小的进程，取出时将其 stride 加上 pass。set_priority 拒绝小于 2 的优先级，其余返回 prio。此外把 ch4 的 sys_get_time、sys_mmap、sys_munmap 迁移到新的进程结构上，sys_trace 按要求不再维护。



### 1.1 实际情况是轮到 p1 执行吗？为什么？

不会轮到 p1.

原因是 `stride` 用 8 位无符号数存储.250+10会变成4.还是会执行p2.


### 1.2 为什么？尝试简单说明（不要求严格证明）。

因为优先级 `>= 2` 时 STRIDE_MAX=BigStride/2,STRIDE_MIN理论上可以非常接近0，但是不等于0，所以BigStride/2-STRIDE_MIN< BigStride/2


### 1.3. 



```rust
use core::cmp::Ordering;

struct Stride(u64);

impl PartialOrd for Stride {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        let diff = self.0.wrapping_sub(other.0);
        if diff == 0 {
            // 题目保证两个 Stride 永远不会相等
            None
        } else if diff < 1 << 63 {
            Some(Ordering::Greater)
        } else {
            Some(Ordering::Less)
        }
    }
}

impl PartialEq for Stride {
    fn eq(&self, other: &Self) -> bool {
        false
    }
}
```




mod yield_now;

use core::{future::Future, task::Poll};

use async_task::{Runnable, Task};
use atomic::Ordering;
use common::config::TASK_LIMIT;
use crossbeam_queue::ArrayQueue;
use klocks::Lazy;
pub use yield_now::yield_now;

use crate::SHUTDOWN;

static TASK_QUEUE: Lazy<TaskQueue> = Lazy::new(TaskQueue::new);

/// NOTE: 目前的实现中，并发的任务量是有硬上限 (`TASK_LIMIT`) 的，超过会直接 panic
struct TaskQueue {
    queue: ArrayQueue<Runnable>,
}

impl TaskQueue {
    fn new() -> Self {
        Self {
            queue: ArrayQueue::new(TASK_LIMIT),
        }
    }

    fn push_task(&self, runnable: Runnable) {
        self.queue.push(runnable).expect("Out of task limit");
    }

    fn fetch_task(&self) -> Option<Runnable> {
        self.queue.pop()
    }
}

pub fn spawn_with<F, A>(future: F, action: A) -> (Runnable, Task<F::Output>)
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
    A: Fn() + Send + Sync + 'static,
{
    // TODO: 现在这么操作用于让用户线程被调度时状态设为 `Ready`，其实可能可以有更好的方式
    async_task::spawn(future, move |runnable| {
        action();
        TASK_QUEUE.push_task(runnable);
    })
}

pub fn run_until_shutdown() {
    loop {
        while let Some(task) = TASK_QUEUE.fetch_task() {
            trace!("Schedule new task");
            task.run();
        }
        if SHUTDOWN.load(Ordering::SeqCst) {
            break;
        }
        sbi_rt::hart_suspend(sbi_rt::Retentive, 0, 0);
    }
}

pub fn block_on<T>(fut: impl Future<Output = T>) -> T {
    let waker = futures::task::noop_waker_ref();
    let mut cx = core::task::Context::from_waker(waker);

    let mut fut = core::pin::pin!(fut);
    loop {
        if let Poll::Ready(ret) = fut.as_mut().poll(&mut cx) {
            return ret;
        }
    }
}

//! One background thread for blocking file, JSON, and credential work.
//!
//! GPUI's event loop stays on the main thread. Jobs are FIFO so two settings
//! clicks cannot overwrite each other. The pool from
//! `background_executor` is unordered, so config writes do not go there.
//! Long waits (agent start, `/models`, browser sign-in) use their own
//! threads and must not be enqueued here.

use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;

fn sender() -> &'static Sender<Box<dyn FnOnce() + Send>> {
    static TX: OnceLock<Sender<Box<dyn FnOnce() + Send>>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Box<dyn FnOnce() + Send>>();
        std::thread::Builder::new()
            .name("apollo-ui-io".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    job();
                }
            })
            .expect("apollo-ui io thread");
        tx
    })
}

pub fn enqueue(job: impl FnOnce() + Send + 'static) {
    let _ = sender().send(Box::new(job));
}

/// Run `work` on the IO thread and apply the value on the UI thread.
/// The UI task only waits on the executor's timer; it does not block on IO.
pub fn off_ui<V, T, W, A>(cx: &mut gpui::Context<V>, work: W, apply: A)
where
    V: 'static,
    T: Send + 'static,
    W: FnOnce() -> T + Send + 'static,
    A: FnOnce(&mut V, T, &mut gpui::Context<V>) + 'static,
{
    let (tx, rx) = mpsc::channel();
    enqueue(move || {
        let _ = tx.send(work());
    });
    cx.spawn(async move |this, cx: &mut gpui::AsyncApp| loop {
        match rx.try_recv() {
            Ok(value) => {
                this.update(cx, |view, cx| apply(view, value, cx)).ok();
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(40))
                    .await;
            }
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
    })
    .detach();
}

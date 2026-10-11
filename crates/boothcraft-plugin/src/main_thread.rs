//! Proof of being on the main thread.
//!
//! VST3 and AudioUnit require most calls (instantiate, set up, state, editors) on the main thread.
//! Every such method takes a [`MainThread`] token, so the compiler checks thread affinity: the
//! token is `!Send` and `!Sync`, and only [`MainThread::init`] makes one.

use std::cell::Cell;
use std::marker::PhantomData;

thread_local! {
    static IS_MAIN: Cell<bool> = const { Cell::new(false) };
}

#[derive(Clone, Copy, Debug)]
pub struct MainThread(PhantomData<*const ()>);

impl MainThread {
    /// Claim the calling thread as the main thread. Call first thing in `main`; the app has one
    /// such thread, and the token cannot leave it.
    pub fn init() -> MainThread {
        IS_MAIN.with(|m| m.set(true));
        MainThread(PhantomData)
    }

    /// Whether the calling thread claimed to be the main thread (for debug assertions on
    /// shutdown paths, where something must not be dropped on the audio thread).
    pub fn is_current() -> bool {
        IS_MAIN.with(Cell::get)
    }
}

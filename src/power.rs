//! System sleep/wake notifications for the fan curve.
//!
//! On macOS a background thread registers with the root power domain
//! (`IORegisterForSystemPower`, the same public IOKit API `caffeinate` and
//! friends use) and records transitions in lock-free shared state. The
//! curve loop polls a [`Monitor`] between temperature reads, so it can turn
//! the fans off before the machine sleeps and reconnect/re-apply lighting
//! the moment it wakes — instead of discovering a stale device handle on
//! the next failed send.
//!
//! Sleep acknowledgements (`IOAllowPowerChange`) are sent from the callback
//! immediately; without them macOS would hold sleep up for 30 seconds.
//!
//! On Linux there is no watcher (that would need logind/D-Bus); [`start`]
//! returns `None` and the curve falls back to reconnect-on-error, as before.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// The system is about to sleep (fires before the machine goes down).
    Slept,
    /// The system finished waking up.
    Woke,
}

/// Shared, lock-free record of power events, written by the notification
/// callback and read by the curve loop.
#[derive(Debug, Default)]
pub struct State {
    asleep: AtomicBool,
    wakes: AtomicU64,
}

impl State {
    pub fn note_sleep(&self) {
        self.asleep.store(true, Ordering::SeqCst);
    }

    pub fn note_wake(&self) {
        self.asleep.store(false, Ordering::SeqCst);
        self.wakes.fetch_add(1, Ordering::SeqCst);
    }

    fn asleep(&self) -> bool {
        self.asleep.load(Ordering::SeqCst)
    }

    fn wakes(&self) -> u64 {
        self.wakes.load(Ordering::SeqCst)
    }
}

/// Consumer view of a [`State`]: yields each transition exactly once, in
/// order, even when a full sleep/wake cycle happened between polls.
pub struct Monitor {
    state: Arc<State>,
    seen_wakes: u64,
    reported_sleep: bool,
}

impl Monitor {
    pub fn new(state: Arc<State>) -> Self {
        let seen_wakes = state.wakes();
        Monitor {
            state,
            seen_wakes,
            reported_sleep: false,
        }
    }

    /// The next unreported transition, oldest first; `None` when caught up.
    pub fn poll(&mut self) -> Option<Transition> {
        if self.state.wakes() > self.seen_wakes {
            // At least one full sleep→wake cycle since the last poll.
            if !self.reported_sleep {
                self.reported_sleep = true;
                return Some(Transition::Slept);
            }
            // Collapse multiple cycles into a single wake.
            self.seen_wakes = self.state.wakes();
            self.reported_sleep = false;
            return Some(Transition::Woke);
        }
        if self.state.asleep() && !self.reported_sleep {
            self.reported_sleep = true;
            return Some(Transition::Slept);
        }
        None
    }

    /// Whether a transition is waiting, without consuming it. Lets wait
    /// loops cut their sleep short and handle a wake promptly.
    pub fn pending(&self) -> bool {
        self.state.wakes() > self.seen_wakes || (self.state.asleep() && !self.reported_sleep)
    }

    /// Whether the system is currently (about to be) asleep.
    pub fn is_asleep(&self) -> bool {
        self.state.asleep()
    }
}

/// Start watching for sleep/wake. Returns `None` where notifications are
/// unavailable (non-macOS, or registration failed); the caller should then
/// rely on reconnect-on-error alone.
pub fn start() -> Option<Monitor> {
    #[cfg(target_os = "macos")]
    {
        let state = Arc::new(State::default());
        if mac::spawn_watcher(state.clone()) {
            return Some(Monitor::new(state));
        }
        None
    }
    #[cfg(not(target_os = "macos"))]
    None
}

#[cfg(target_os = "macos")]
mod mac {
    //! IOKit registration and the CFRunLoop thread that services it.

    use std::ffi::c_void;
    use std::sync::Arc;
    use std::sync::mpsc;

    use super::State;

    #[repr(C)]
    struct Opaque {
        _private: [u8; 0],
    }
    type IoConnect = u32;
    type IoObject = u32;
    type IoService = u32;
    type IoNotificationPortRef = *mut Opaque;
    type CfRunLoopRef = *mut Opaque;
    type CfRunLoopSourceRef = *mut Opaque;
    type CfStringRef = *const c_void;

    type InterestCallback = extern "C" fn(
        refcon: *mut c_void,
        service: IoService,
        message_type: u32,
        argument: *mut c_void,
    );

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IORegisterForSystemPower(
            refcon: *mut c_void,
            the_port_ref: *mut IoNotificationPortRef,
            callback: InterestCallback,
            notifier: *mut IoObject,
        ) -> IoConnect;
        fn IOAllowPowerChange(root_domain_connect: IoConnect, notification_id: isize) -> i32;
        fn IONotificationPortGetRunLoopSource(port: IoNotificationPortRef) -> CfRunLoopSourceRef;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRunLoopGetCurrent() -> CfRunLoopRef;
        fn CFRunLoopAddSource(rl: CfRunLoopRef, source: CfRunLoopSourceRef, mode: CfStringRef);
        fn CFRunLoopRun();
        static kCFRunLoopDefaultMode: CfStringRef;
    }

    // IOMessage.h: iokit_common_msg(0x270 / 0x280 / 0x300).
    const MSG_CAN_SYSTEM_SLEEP: u32 = 0xE000_0270;
    const MSG_SYSTEM_WILL_SLEEP: u32 = 0xE000_0280;
    const MSG_SYSTEM_HAS_POWERED_ON: u32 = 0xE000_0300;

    /// Callback context; leaked once per process because the watcher thread
    /// and the registration live for the process lifetime.
    struct Ctx {
        state: Arc<State>,
        root_port: IoConnect,
    }

    extern "C" fn power_callback(
        refcon: *mut c_void,
        _service: IoService,
        message_type: u32,
        argument: *mut c_void,
    ) {
        // SAFETY: refcon is the leaked Ctx set up in spawn_watcher; the
        // callback only fires while its run loop (same thread) is running,
        // which starts after root_port is filled in.
        let ctx = unsafe { &*(refcon as *const Ctx) };
        match message_type {
            MSG_CAN_SYSTEM_SLEEP => {
                // Don't veto idle sleep — just acknowledge promptly.
                unsafe { IOAllowPowerChange(ctx.root_port, argument as isize) };
            }
            MSG_SYSTEM_WILL_SLEEP => {
                ctx.state.note_sleep();
                // Mandatory ack; sleep proceeds regardless, waiting up to
                // 30s for stragglers.
                unsafe { IOAllowPowerChange(ctx.root_port, argument as isize) };
            }
            MSG_SYSTEM_HAS_POWERED_ON => ctx.state.note_wake(),
            _ => {}
        }
    }

    /// Register for power notifications on a dedicated thread running a
    /// CFRunLoop. Returns false if registration failed.
    pub fn spawn_watcher(state: Arc<State>) -> bool {
        let (tx, rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("power-watch".into())
            .spawn(move || {
                let ctx = Box::into_raw(Box::new(Ctx {
                    state,
                    root_port: 0,
                }));
                let mut port: IoNotificationPortRef = std::ptr::null_mut();
                let mut notifier: IoObject = 0;
                let root_port = unsafe {
                    IORegisterForSystemPower(
                        ctx as *mut c_void,
                        &mut port,
                        power_callback,
                        &mut notifier,
                    )
                };
                if root_port == 0 || port.is_null() {
                    // Registration failed; reclaim the context and report.
                    drop(unsafe { Box::from_raw(ctx) });
                    let _ = tx.send(false);
                    return;
                }
                // The callback needs the root port to acknowledge sleep;
                // safe to fill in because callbacks only run inside
                // CFRunLoopRun below, on this same thread.
                unsafe { (*ctx).root_port = root_port };
                let _ = tx.send(true);
                unsafe {
                    let source = IONotificationPortGetRunLoopSource(port);
                    CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
                    CFRunLoopRun();
                }
            });
        match spawned {
            Ok(_) => rx.recv().unwrap_or(false),
            Err(e) => {
                log::warn!("could not start the sleep/wake watcher thread: {e}");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (Arc<State>, Monitor) {
        let state = Arc::new(State::default());
        (state.clone(), Monitor::new(state))
    }

    #[test]
    fn quiet_state_reports_nothing() {
        let (_state, mut mon) = pair();
        assert!(!mon.pending());
        assert_eq!(mon.poll(), None);
        assert!(!mon.is_asleep());
    }

    #[test]
    fn sleep_then_wake_reports_in_order() {
        let (state, mut mon) = pair();
        state.note_sleep();
        assert!(mon.pending());
        assert!(mon.is_asleep());
        assert_eq!(mon.poll(), Some(Transition::Slept));
        assert_eq!(mon.poll(), None); // reported once
        state.note_wake();
        assert!(mon.pending());
        assert_eq!(mon.poll(), Some(Transition::Woke));
        assert_eq!(mon.poll(), None);
        assert!(!mon.is_asleep());
    }

    #[test]
    fn full_cycle_between_polls_reports_both() {
        let (state, mut mon) = pair();
        state.note_sleep();
        state.note_wake();
        assert!(mon.pending());
        assert_eq!(mon.poll(), Some(Transition::Slept));
        assert_eq!(mon.poll(), Some(Transition::Woke));
        assert_eq!(mon.poll(), None);
        assert!(!mon.pending());
    }

    #[test]
    fn multiple_cycles_collapse_into_one() {
        let (state, mut mon) = pair();
        for _ in 0..3 {
            state.note_sleep();
            state.note_wake();
        }
        assert_eq!(mon.poll(), Some(Transition::Slept));
        assert_eq!(mon.poll(), Some(Transition::Woke));
        assert_eq!(mon.poll(), None);
    }

    #[test]
    fn wake_followed_by_new_sleep_reports_all_three() {
        let (state, mut mon) = pair();
        state.note_sleep();
        assert_eq!(mon.poll(), Some(Transition::Slept));
        state.note_wake();
        state.note_sleep(); // slept again before the loop caught up
        assert_eq!(mon.poll(), Some(Transition::Woke));
        assert_eq!(mon.poll(), Some(Transition::Slept));
        assert_eq!(mon.poll(), None);
        assert!(mon.is_asleep());
    }
}

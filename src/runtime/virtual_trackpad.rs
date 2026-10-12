// this file is basically copied and rearranged from arcnmx's GitHub example
// in the input-linux-rs repo (a translation of an example
// on the Linux kernel's uinput module, actually).
// The Rust example can be found here:
// https://github.com/arcnmx/input-linux-rs/blob/main/examples/mouse-movements.rs

use std::{
    fs::{File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    sync::{Arc, Mutex, PoisonError},
    thread, time::{self, Duration}
};
use futures_lite::future::FutureExt;
use async_io::Timer;
use tokio::sync::mpsc::Receiver;
use input_linux::{
    EventKind, EventTime,
    InputEvent, InputId,
    Key, KeyEvent, KeyState,
    RelativeAxis, RelativeEvent,
    SynchronizeEvent, SynchronizeKind,
    UInputHandle
};

use nix::libc::O_NONBLOCK;
use tracing::{debug, error, trace};

use crate::runtime::event_handler::ControlSignal::{self, *};


const ZERO: EventTime = EventTime::new(0, 0);


/// The uinput device itself, shared by every clone of `VirtualTrackpad`.
/// It is destroyed when the last clone is dropped.
struct Device {
    handle: UInputHandle<File>,

    // Whether the left button is currently held down. The lock is held
    // while the button event is written, so the drag end timer and the
    // gesture translator can't interleave a press and a release.
    mouse_is_down: Mutex<bool>
}

impl Device {
    fn write_button(&self, pressed: bool) -> Result<(), std::io::Error> {
        let events = [
            InputEvent::from(
                KeyEvent::new(
                    ZERO,
                    Key::ButtonLeft,
                    KeyState::pressed(pressed))
                ).into_raw(),
            InputEvent::from(
                SynchronizeEvent::new(
                    ZERO,
                    SynchronizeKind::Report,
                    0)
                ).into_raw(),
        ];
        self.handle.write(&events)?;
        Ok(())
    }
}

impl Drop for Device {
    /// Release the button (so nothing is left held down), and remove the
    /// device, since we don't need virtual devices cluttering the system.
    /// This runs on every way out of the program, including errors and
    /// panics, so it only logs failures.
    fn drop(&mut self) {
        let is_down = self.mouse_is_down
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);

        if *is_down {
            if let Err(e) = self.write_button(false) {
                error!("Could not release the mouse button on exit: {}", e);
            }
        }
        match self.handle.dev_destroy() {
            Ok(()) => debug!("Virtual trackpad destroyed."),
            Err(e) => error!("Could not destroy the virtual trackpad: {}", e)
        }
    }
}


/// Cloning is cheap, and all clones drive the same device and share its
/// mouse button state.
#[derive(Clone)]
pub struct VirtualTrackpad {
    device: Arc<Device>,

    // sub-pixel motion left over from previous moves, carried
    // into the next one (see `mouse_move_relative()`)
    rem_x: f64,
    rem_y: f64
}


pub fn start_handler() -> Result<VirtualTrackpad, std::io::Error> {
    let uinput_file_res = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(O_NONBLOCK)
        .open("/dev/uinput");

    let uinput_file = match uinput_file_res {
        Ok(file) => file,
        Err(e) => {
            error!(
                "You are not yet allowed to write to /dev/uinput.\n\
                Some things to try:\n\
                - Update the udev rules for uinput (see installation guide in README.md, step 3.1)\n\
                - Log out and log in again\n\
                - Restart your computer\n\
                - FOR ARCH: make sure the uinput kernel module is loaded on boot\n",
            );
            return Err(e);
        }
    };

    let uhandle = UInputHandle::new(uinput_file);

    uhandle.set_evbit(EventKind::Key)?;
    uhandle.set_keybit(input_linux::Key::ButtonLeft)?;

    uhandle.set_evbit(EventKind::Relative)?;
    uhandle.set_relbit(RelativeAxis::X)?;
    uhandle.set_relbit(RelativeAxis::Y)?;

    let input_id = InputId {
        bustype: input_linux::sys::BUS_USB,
        vendor: 0x1234,
        product: 0x5678,  // iykyk
        version: 0,
    };
    let device_name = b"Virtual trackpad (created by linux-3-finger-drag)";
    uhandle.create(&input_id, device_name, 0, &[])?;
    debug!("Virtual trackpad successfully created.");

    // may be needed to let the system catch up
    thread::sleep(time::Duration::from_millis(500));

    Ok(
        VirtualTrackpad {
            device: Arc::new(Device {
                handle: uhandle,
                mouse_is_down: Mutex::new(false)
            }),
            rem_x: 0.0,
            rem_y: 0.0
        }
    )

}


/// Start a timer for `delay`.
///
/// The messy return type is to match `listen_for_signal`. It is infallible.
async fn plain_timeout(delay: Duration) -> Option<ControlSignal>{
    trace!("Starting delay of {:?}", delay);
    Timer::after(delay).await;
    trace!("Delay completed fully");
    None
}

// dragEndDelay time should be cut short by a pointer or scoll gesture;
// this function listens on the channel for either a pointer button press,
// a scoll event, or a non-3-finger gesture, and exits when it gets one
async fn listen_for_signal(rx: &mut Receiver<ControlSignal>) -> Option<ControlSignal> {

    // function blocks until signal is received
    // since ControlSignal is the only thing
    // ever sent in the channel, there's no need to
    // check that that's what we received
    debug!("Listening for cancel signal...");
    trace!("Size of buffer currently: {}", rx.len());
    let sig = rx.recv().await?;
    debug!("Signal received: {:?}", sig);
    Some(sig)
}


impl VirtualTrackpad
{
    /// Press or release the left button, unless it's already in that
    /// state. Returns whether anything was written.
    fn set_button(&self, pressed: bool) -> Result<bool, std::io::Error> {
        let mut is_down = self.device.mouse_is_down
            .lock()
            .unwrap_or_else(PoisonError::into_inner);

        if *is_down == pressed {
            return Ok(false);
        }
        self.device.write_button(pressed)?;
        *is_down = pressed;
        Ok(true)
    }

    pub fn is_mouse_down(&self) -> bool {
        *self.device.mouse_is_down
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub fn mouse_down(&mut self) -> Result<(), std::io::Error> {
        // a new drag shouldn't inherit leftover motion from the last one
        self.rem_x = 0.0;
        self.rem_y = 0.0;

        self.set_button(true)?;
        Ok(())
    }

    pub fn mouse_up(&self) -> Result<(), std::io::Error> {
        if self.set_button(false)? {
            debug!("mouse_up written");
        }
        Ok(())
    }


    /// This is an infinite loop that listens for and processes signals
    /// for a delay to the end of the drag, like cancelation. This
    /// thread will not panic, and will not stop unless either it's
    /// sent a `ControlSignal::TerminateThread`, or an error was
    /// raised. So if it ends prematurely, it's because of an error.
    pub async fn handle_mouse_up_timeout(&self, mut rx: Receiver<ControlSignal>) -> Result<(), std::io::Error> {

        loop {
            trace!("awaiting signal in handle_mouse_up_timeout...");
            let ctl_sig = match rx.recv().await {
                Some(sig) => sig,
                None => break
            };
            debug!("sig recv'd in outer loop: {:?}", ctl_sig);

            // handle signals received during outer loop
            let delay = match ctl_sig {
                RestartTimer(delay) => delay,   // proceed to timer
                CancelMouseUp => continue,      // don't do anything this iteration
                TerminateThread => break
            };

            // handle signals received during timer loop
            // that can't be handled within that scope
            match self.run_timer(delay, &mut rx).await {
                Some(CancelMouseUp) => continue,
                Some(TerminateThread) => break,
                _ => {}     // the timer ran out (restarts are handled in run_timer)
            }

            trace!("Drag end delay ran out, releasing drag");
            self.mouse_up()?;
        }

        Ok(())
    }


    pub fn mouse_move_relative(&mut self, x_rel: f64, y_rel:f64) -> Result<(), std::io::Error> {

        // RelativeEvent::new() can only take integers, but libinput
        // gives us fractional deltas. Simply truncating each delta
        // throws away the fractional part on every event, so slow
        // drags (where each delta is under 1px) never move the cursor
        // at all. Instead, the fractional part is kept and added to the
        // next delta, so all motion eventually makes it to the cursor.
        // Truncating toward 0 (trunc()) keeps this unbiased in both
        // directions, so the cursor doesn't drift.
        self.rem_x += x_rel;
        self.rem_y += y_rel;

        let x_rel_whole = self.rem_x.trunc();
        let y_rel_whole = self.rem_y.trunc();

        self.rem_x -= x_rel_whole;
        self.rem_y -= y_rel_whole;

        // nothing to report until at least a whole pixel has built up
        if x_rel_whole == 0.0 && y_rel_whole == 0.0 {
            return Ok(());
        }

        let x_rel_int = x_rel_whole as i32;
        let y_rel_int = y_rel_whole as i32;

        let events = [
            InputEvent::from(
                RelativeEvent::new(
                    ZERO,
                    RelativeAxis::X,
                    x_rel_int)
                ).into_raw(),
            InputEvent::from(
                RelativeEvent::new(
                    ZERO,
                    RelativeAxis::Y,
                    y_rel_int)
                ).into_raw(),
            InputEvent::from(
                SynchronizeEvent::new(
                    ZERO,
                    SynchronizeKind::Report,
                    0)
                ).into_raw(),
        ];
        self.device.handle.write(&events)?;
        Ok(())
    }


    /// A timer that can be cancelled or reset via a signal in the channel. The return value
    /// is what signal was received, if any, except for `RestartTimer`, since it can be handled
    /// within the function (restarting with the delay it carries).
    async fn run_timer(&self, mut delay: Duration, rx: &mut Receiver<ControlSignal>) -> Option<ControlSignal> {
        loop {
            // catches the output of the function that returns first
            let signal = plain_timeout(delay)
                .or(listen_for_signal(rx))
                .await?;

            match signal {
                RestartTimer(new_delay) => {
                    delay = new_delay;
                    continue
                },
                // function exits, lets the outer loop handle the other signals
                _ => return Some(signal),
            }
        }
    }
}

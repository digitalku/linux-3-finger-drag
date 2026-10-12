// this file is basically copied and rearranged from arcnmx's GitHub example
// in the input-linux-rs repo (a translation of an example
// on the Linux kernel's uinput module, actually). 
// The Rust example can be found here: 
// https://github.com/arcnmx/input-linux-rs/blob/main/examples/mouse-movements.rs

use std::{
    fs::{File, OpenOptions}, 
    os::{fd::AsFd, unix::fs::OpenOptionsExt}, 
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


/// This struct is does not preserve `mouse_is_down` state between clones: 
/// that is copied during cloning, for simplicity. 
pub struct VirtualTrackpad {
    handle: UInputHandle<File>,
    pub mouse_is_down: bool,

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

    // I'm using unwraps here because this function is only called 
    // during the program's setup phase. I've also never had these 
    // functions below crash the program; if this `start_handler()`
    // ever crashes (from my experience), it's always an issue with
    // trying to read `/dev/uinput`. It's typically smooth sailing
    // in this function after that succeeds. 
    uhandle.set_evbit(EventKind::Key).unwrap();
    uhandle.set_keybit(input_linux::Key::ButtonLeft).unwrap();

    uhandle.set_evbit(EventKind::Relative).unwrap();
    uhandle.set_relbit(RelativeAxis::X).unwrap();
    uhandle.set_relbit(RelativeAxis::Y).unwrap();

    let input_id = InputId {
        bustype: input_linux::sys::BUS_USB,
        vendor: 0x1234,
        product: 0x5678,  // iykyk
        version: 0,
    };
    let device_name = b"Virtual trackpad (created by linux-3-finger-drag)";
    uhandle.create(&input_id, device_name, 0, &[]).unwrap();
    debug!("Virtual trackpad successfully created.");

    // may be needed to let the system catch up
    thread::sleep(time::Duration::from_millis(500));

    Ok(
        VirtualTrackpad { 
            handle: uhandle, 
            mouse_is_down: false,
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

impl Clone for VirtualTrackpad {
    /// This clone() can theoretically panic since there is an expect() in 
    /// its definition. This is because `try_cloned_to_owned`, from `std::io`,
    /// utilizes libc's `fnctl`, which can fail, but will only do so if 
    /// duplicating the file descriptor would exceed the maximum number of 
    /// file descriptors to be opened (or if the arguments to it are invalid; 
    /// the Rust method, however, takes no arguments except for a known-valid FD, 
    /// so those arguments are controlled by the `std` library).
    /// 
    /// This makes it as safe as any other file-system function to call, since 
    /// it only fails when there is a severe resource limitation issue (which 
    /// would be a rare and system-wide problem).
    /// 
    /// Note that the boolean `mouse_is_down` is *copied*, **not** passed by 
    /// reference, for simplicity. 
    fn clone(&self) -> Self {
        let uinput_fd = self.handle
            .as_fd()
            .try_clone_to_owned()
            .expect(
                "uinput file descriptor could not be duplicated, \
                likely do to hitting the maximum open file descriptors \
                for this OS."
        );

        VirtualTrackpad {
            handle: UInputHandle::new(File::from(uinput_fd)),
            mouse_is_down: self.mouse_is_down,
            rem_x: self.rem_x,
            rem_y: self.rem_y
        }
    }
}


impl VirtualTrackpad
{
    const ZERO: EventTime = EventTime::new(0, 0);

    pub fn mouse_down(&mut self) -> Result<(), std::io::Error> {
        let events = [
            InputEvent::from(
                KeyEvent::new(
                    VirtualTrackpad::ZERO, 
                    Key::ButtonLeft, 
                    KeyState::pressed(true))
                ).into_raw(),
            InputEvent::from(
                SynchronizeEvent::new(
                    VirtualTrackpad::ZERO, 
                    SynchronizeKind::Report, 
                    0)
                ).into_raw(),
        ];
        self.handle.write(&events)?;
        self.mouse_is_down = true;

        // a new drag shouldn't inherit leftover motion from the last one
        self.rem_x = 0.0;
        self.rem_y = 0.0;
        Ok(())
    }

    pub fn mouse_up(&mut self) -> Result<(), std::io::Error> {   

        let events = [
            InputEvent::from(
                KeyEvent::new(
                    VirtualTrackpad::ZERO, 
                    Key::ButtonLeft, 
                    KeyState::pressed(false))
                ).into_raw(),
            InputEvent::from(
                SynchronizeEvent::new(
                    VirtualTrackpad::ZERO, 
                    SynchronizeKind::Report, 
                    0)
                ).into_raw(),
        ];
        self.handle.write(&events)?;
        self.mouse_is_down = false;

        debug!("mouse_up written from simple mouse_up fn");

        Ok(())
    }


    /// This is an infinite loop that listens for and processes signals
    /// for a delay to the end of the drag, like cancelation. This 
    /// thread will not panic, and will not stop unless either it's 
    /// sent a `ControlSignal::TerminateThread`, or an error was 
    /// raised. So if it ends prematurely, it's because of an error.
    pub async fn handle_mouse_up_timeout(&mut self, mut rx: Receiver<ControlSignal>) -> Result<(), std::io::Error> {
        
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
                CancelTimer => {
                    trace!("Setting mouse up now");
                    self.mouse_up()?;
                    continue;
                },
                CancelMouseUp => continue,  // don't do anything this iteration
                TerminateThread => break
            };

            // handle signals received during timer loop
            // that can't be handled within that scope
            match self.run_timer(delay, &mut rx).await {
                Some(signal) => {
                    match signal {
                        CancelMouseUp => continue,
                        TerminateThread => break,
                        _ => {}                     // cancel/restart timer have already been handled
                    }
                },
                None => {}
            }

            self.mouse_up()?;
            debug!("mouse_up written from async mouse_up fn");
        }

        Ok(())
    }

    
    /// A simple, blocking mouse_up, but with a set, blocking, uncancellable delay. 
    /// `delay` is measured in milliseconds.
    pub fn mouse_up_delay_blocking(&mut self, delay: Duration) -> Result<(), std::io::Error> {
        
        std::thread::sleep(delay);

        let events = [
            InputEvent::from(
                KeyEvent::new(
                    VirtualTrackpad::ZERO,
                    Key::ButtonLeft, 
                    KeyState::pressed(false))
                ).into_raw(),
            InputEvent::from(
                SynchronizeEvent::new(
                    VirtualTrackpad::ZERO,
                    SynchronizeKind::Report, 
                    0)
                ).into_raw(),
        ];
        self.handle.write(&events)?;

        debug!("mouse_up written from mouse_up_delay_blocking");

        self.mouse_is_down = false;
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
                    VirtualTrackpad::ZERO, 
                    RelativeAxis::X, 
                    x_rel_int)
                ).into_raw(),
            InputEvent::from(
                RelativeEvent::new(
                    VirtualTrackpad::ZERO, 
                    RelativeAxis::Y, 
                    y_rel_int)
                ).into_raw(),
            InputEvent::from(
                SynchronizeEvent::new(
                    VirtualTrackpad::ZERO, 
                    SynchronizeKind::Report, 
                    0)
                ).into_raw(),
        ];
        self.handle.write(&events)?;
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
                // covers `CancelTimer` arm, since the behavior would be identical
                _ => return Some(signal), 
            }
        }
    }

    pub fn destruct(self) -> Result<(), std::io::Error> {
        self.handle.dev_destroy()
    }
}
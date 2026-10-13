use std::{
    path::PathBuf,
    sync::{
        Arc, atomic::{AtomicBool, Ordering}
    }, time::SystemTime
};
use tokio::sync::mpsc::{self, Receiver};
use signal_hook::{self, consts::{SIGINT, SIGTERM}, flag};
use tracing::{debug, error, info, trace};
use tracing_subscriber::fmt::time::ChronoLocal;

use linux_3_finger_drag::{
    init::{config, instance_lock, libinput_init},
    runtime::{
        event_handler::{ControlSignal, GestureTranslator, GtError}, 
        virtual_trackpad
    }
};


#[tokio::main]
async fn main() -> Result<(), GtError> {

    // The config file is optional, so neither a missing file nor an 
    // unknown config directory should stop the program (they only 
    // mean there's nothing to hot-reload).
    let cfg_file = config::get_config_file_path().ok(); 
    let cfg_last_modified = cfg_file
        .as_deref()
        .and_then(config::get_config_last_modified);

    let configs = config::init_cfg();

    match config::init_file_logger(configs.clone()) {
        Some(logger) => logger.init(), 
        None => {
            tracing_subscriber::fmt()
                .with_writer(std::io::stdout)
                .with_max_level(configs.log_level)
                .with_timer(ChronoLocal::rfc_3339())
                .init();
        }
    };
    println!("[PRE-LOG: INFO]: Logger initialized!"); 

    // held until main() returns; the kernel releases it on exit
    let _instance_lock = instance_lock::acquire()?;

    // handling SIGINT and SIGTERM
    let should_exit = Arc::new(AtomicBool::new(false));
    flag::register(SIGTERM, Arc::clone(&should_exit))?;
    flag::register(SIGINT,  Arc::clone(&should_exit))?;

    let (sender, recvr) = mpsc::channel::<ControlSignal>(3);
    let vtrackpad = virtual_trackpad::start_handler()?;

    info!("Searching for the trackpad on your device...");

    info!("end evdev search");
    // using a match case here instead of a `?` here so the program can log
    // its cleanup before it exits
    let main_result = match libinput_init::find_real_trackpads() {

        Ok(real_trackpad) => {

            let translator = GestureTranslator::new(
                vtrackpad.clone(), 
                configs.clone(),
                sender
            );
            run_main_event_loop(
                translator, 
                recvr, 
                &should_exit, 
                real_trackpad, 
                cfg_file,
                cfg_last_modified
            ).await
        },
        Err(e) => Err(GtError::from(e))
    };

    // the program arrives here if either a signal is received, 
    // or there was some issue during initialization
    // Dropping the last handle to the virtual trackpad releases the mouse
    // button (if it's still down) and destroys the device.
    info!("Cleaning up and exiting...");
    drop(vtrackpad);
    
    info!("Clean up successful.");
    main_result
}


// This function is placed in `main.rs` since it's essentially a 
// part of `main`, and I wanted to break it out so the `main` isn't
// too sprawling
async fn run_main_event_loop(
    mut translator: GestureTranslator,
    recvr: Receiver<ControlSignal>,
    should_exit: &Arc<AtomicBool>,
    mut real_trackpad: input::Libinput, 
    cfg_file: Option<PathBuf>,
    mut cfg_last_modified: Option<SystemTime>
    
) -> Result<(), GtError> {

    // spawn 1 separate thread to handle mouse_up_delay timeouts
    // (the delay itself is sent along with each `RestartTimer` signal,
    // so it stays in sync with the config when that is reloaded)
    debug!("Creating new thread to manage drag end timer");
    let vtp_clone = translator.vtp.clone();

    let fork_fn = async move {
        vtp_clone.handle_mouse_up_timeout(recvr)
            .await
            .map_err(GtError::from)
    };

    let mouse_up_listener = tokio::spawn(fork_fn);

    info!("linux-3-finger-drag started successfully!");

    loop {
        // this is to keep the infinite loop from filling out into
        // entire CPU core, which it will do even on no-ops.
        std::thread::sleep(translator.cfg.response_time);

        // check if the configuration was modified (or created), and if so, 
        // update configs in memory. If the file is missing, keep the current
        // configs: editors that save by replacing the file can make it 
        // disappear for a moment, and that shouldn't reset everything.
        // `!=` rather than `>`, since restoring an older copy of the file 
        // is a change too.
        if let Some(cfg_file) = &cfg_file {
            let cfg_last_modified_update = config::get_config_last_modified(cfg_file);

            if cfg_last_modified_update.is_some() 
                && cfg_last_modified_update != cfg_last_modified {

                translator.cfg = config::init_cfg();
                cfg_last_modified = cfg_last_modified_update;
            }
        }


        // handle interrupts
        if should_exit.load(Ordering::Relaxed) {
            break;
        }
        
        if let Err(e) = real_trackpad.dispatch() {
            error!("A {} error occured in reading device buffer: {}", e.kind(), e);
        }

        for event in &mut real_trackpad {

            trace!("Blocking in main()'s for loop");

            // do nothing on success (or ignored gesture)
            if let Err(e) = translator.translate_gesture(event).await { 
                error!("{:?}", e); 
            }

            // Without being a `ControlSignal::TerminateThread` being sent
            // into the channel, the other thread only finishes when
            // is an error is raised. it has been designed not to panic. 
            // the value the thread returns is a `Result`, so the this extracts 
            // the Result from the fork and returns it.
            if mouse_up_listener.is_finished() {
                let fork_result = mouse_up_listener.await?;
                error!("Drag end timer stopped unexpectedly: {:?}", fork_result);
                return fork_result;
            }
        }
    };

    debug!("Joining delay timer thread");
    translator.send_signal(ControlSignal::TerminateThread).await?;

    // awaiting a JoinHandle produces a Result
    // the generic for this JoinHandle, though, is itself a Result, 
    // so we can just return what the JoinHandle yields
    mouse_up_listener.await? 
}
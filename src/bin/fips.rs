use clap::Parser;
use fips::config::{resolve_identity, IdentitySource};
use fips::version;
use fips::{Config, Node};
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;
use tracing::{debug, error, info, warn};
use tracing_subscriber::{fmt, EnvFilter};

#[cfg(target_os = "macos")]
mod run_loop {
    use super::*;
    use std::ffi::c_void;

    static RUN_LOOP_ACTIVE: AtomicBool = AtomicBool::new(false);

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRunLoopRun();
        fn CFRunLoopStop(rl: *mut c_void);
        fn CFRunLoopGetMain() -> *mut c_void;
    }

    pub fn run() {
        if RUN_LOOP_ACTIVE.swap(true, Ordering::Relaxed) {
            debug!("macOS run loop already active");
            return;
        }

        info!("macOS: Running CFRunLoop on main thread for CoreBluetooth callbacks");
        while RUN_LOOP_ACTIVE.load(Ordering::Relaxed) {
            unsafe {
                CFRunLoopRun();
            }

            if RUN_LOOP_ACTIVE.load(Ordering::Relaxed) {
                warn!("macOS: CFRunLoop returned unexpectedly; restarting main run loop");
                thread::sleep(Duration::from_millis(10));
            }
        }

        info!("macOS: CFRunLoop exited");
    }

    pub fn is_active() -> bool {
        RUN_LOOP_ACTIVE.load(Ordering::Relaxed)
    }

    pub fn stop() {
        if !RUN_LOOP_ACTIVE.swap(false, Ordering::Relaxed) {
            return;
        }

        info!("macOS: Stopping CFRunLoop");
        unsafe {
            let main_loop = CFRunLoopGetMain();
            if !main_loop.is_null() {
                CFRunLoopStop(main_loop);
            }
        }

        info!("macOS: CFRunLoop stop requested");
    }
}

#[cfg(not(target_os = "macos"))]
mod run_loop {
    pub fn run() {}
    pub fn is_active() -> bool { true }
    pub fn stop() {}
}

#[derive(Parser, Debug)]
#[command(
    name = "fips",
    version = version::short_version(),
    long_version = version::long_version(),
    about
)]
struct Args {
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,
}

fn main() {
    let args = Args::parse();

    let (config, loaded_paths) = if let Some(config_path) = &args.config {
        match Config::load_file(config_path) {
            Ok(config) => (config, vec![config_path.clone()]),
            Err(e) => {
                eprintln!("Failed to load configuration from {}: {}", config_path.display(), e);
                process::exit(1);
            }
        }
    } else {
        match Config::load() {
            Ok(result) => result,
            Err(e) => {
                eprintln!("Failed to load configuration: {}", e);
                process::exit(1);
            }
        }
    };

    let log_level = config.node.log_level();
    let filter = EnvFilter::builder()
        .with_default_directive(log_level.into())
        .from_env_lossy();

    fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();

    info!("FIPS {} starting", version::short_version());

    if loaded_paths.is_empty() {
        info!("No config files found, using defaults");
    } else {
        for path in &loaded_paths {
            info!(path = %path.display(), "Loaded config file");
        }
    }

    let resolved = match resolve_identity(&config, &loaded_paths) {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to resolve identity: {}", e);
            process::exit(1);
        }
    };
    match &resolved.source {
        IdentitySource::Config => info!("Using identity from configuration"),
        IdentitySource::KeyFile(p) => info!(path = %p.display(), "Loaded persistent identity from key file"),
        IdentitySource::Generated(p) => info!(path = %p.display(), "Generated persistent identity, saved to key file"),
        IdentitySource::Ephemeral => info!("Using ephemeral identity (new keypair each start)"),
    }

    let mut config = config;
    config.node.identity.nsec = Some(resolved.nsec);

    #[cfg(target_os = "macos")]
    {
        let (exit_tx, exit_rx) = std::sync::mpsc::channel();

        thread::spawn(move || {
            while !run_loop::is_active() {
                thread::sleep(Duration::from_millis(10));
            }

            let exit_code = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| {
                    error!("Failed to build tokio runtime: {}", e);
                    1
                })
                .map(|rt| rt.block_on(run_node(config)))
                .unwrap_or(1);

            run_loop::stop();
            let _ = exit_tx.send(exit_code);
        });

        run_loop::run();

        let exit_code = exit_rx.recv().unwrap_or(1);
        if exit_code != 0 {
            process::exit(exit_code);
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let exit_code = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| {
                error!("Failed to build tokio runtime: {}", e);
                1
            })
            .map(|rt| rt.block_on(run_node(config)))
            .unwrap_or(1);

        if exit_code != 0 {
            process::exit(exit_code);
        }
    }
}

async fn run_node(config: Config) -> i32 {
    debug!("Creating node");
    let mut node = match Node::new(config) {
        Ok(node) => node,
        Err(e) => {
            error!("Failed to create node: {}", e);
            return 1;
        }
    };

    info!("Node created:");
    info!("      npub: {}", node.npub());
    info!("   node_addr: {}", hex::encode(node.node_addr().as_bytes()));
    info!("   address: {}", node.identity().address());
    info!("     state: {}", node.state());
    info!(" leaf_only: {}", node.is_leaf_only());

    if let Err(e) = node.start().await {
        error!("Failed to start node: {}", e);
        return 1;
    }

    info!("FIPS running, press Ctrl+C to exit");

    tokio::select! {
        result = node.run_rx_loop() => {
            match result {
                Ok(()) => info!("RX loop exited"),
                Err(e) => error!("RX loop error: {}", e),
            }
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Shutdown signal received");
        }
    }

    info!("FIPS shutting down");

    if let Err(e) = node.stop().await {
        warn!("Error during shutdown: {}", e);
    }

    info!("FIPS shutdown complete");
    0
}

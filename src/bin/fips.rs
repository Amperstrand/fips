//! FIPS daemon binary
//!
//! Loads configuration and creates the top-level node instance.

use clap::Parser;
use fips::config::{resolve_identity, IdentitySource};
use fips::version;
use fips::{Config, Node};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;
use tracing::{debug, error, info, warn};
use tracing_subscriber::{fmt, EnvFilter};

// macOS CFRunLoop support for CoreBluetooth NSStream callbacks
#[cfg(target_os = "macos")]
mod run_loop {
    use super::*;
    use std::ffi::c_void;
    
    static RUN_LOOP_ACTIVE: AtomicBool = AtomicBool::new(false);
    
    unsafe extern "C" {
        fn CFRunLoopRun();
        fn CFRunLoopStop(rl: *mut c_void);
        fn CFRunLoopGetMain() -> *mut c_void;
    }
    
    /// Start the main run loop in a background thread.
    /// Required for CoreBluetooth NSStream callbacks to fire.
    pub fn start() {
        if RUN_LOOP_ACTIVE.load(Ordering::Relaxed) {
            debug!("macOS run loop already active");
            return;
        }
        
        RUN_LOOP_ACTIVE.store(true, Ordering::Relaxed);
        
        thread::spawn(move || {
            info!("macOS: Starting CFRunLoop for CoreBluetooth NSStream callbacks");
            unsafe {
                // Run the main run loop - this blocks until stopped
                CFRunLoopRun();
            }
            info!("macOS: CFRunLoop exited");
            RUN_LOOP_ACTIVE.store(false, Ordering::Relaxed);
        });
        
        // Give the run loop thread a moment to start
        thread::sleep(Duration::from_millis(100));
        debug!("macOS: CFRunLoop thread started");
    }
    
    /// Stop the main run loop during shutdown.
    pub fn stop() {
        if !RUN_LOOP_ACTIVE.load(Ordering::Relaxed) {
            return;
        }
        
        info!("macOS: Stopping CFRunLoop");
        unsafe {
            let main_loop = CFRunLoopGetMain();
            if !main_loop.is_null() {
                CFRunLoopStop(main_loop);
            }
        }
        
        // Wait for run loop thread to exit
        thread::sleep(Duration::from_millis(100));
        info!("macOS: CFRunLoop stopped");
    }
}

#[cfg(not(target_os = "macos"))]
mod run_loop {
    pub fn start() {}
    pub fn stop() {}
}

/// FIPS mesh network daemon
#[derive(Parser, Debug)]
#[command(
    name = "fips",
    version = version::short_version(),
    long_version = version::long_version(),
    about
)]
struct Args {
    /// Path to configuration file (overrides default search paths)
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args = Args::parse();

    // Load configuration before initializing logging so we can use
    // the config's log_level as the tracing filter default.
    let (config, loaded_paths) = if let Some(config_path) = &args.config {
        match Config::load_file(config_path) {
            Ok(config) => (config, vec![config_path.clone()]),
            Err(e) => {
                eprintln!("Failed to load configuration from {}: {}", config_path.display(), e);
                std::process::exit(1);
            }
        }
    } else {
        match Config::load() {
            Ok(result) => result,
            Err(e) => {
                eprintln!("Failed to load configuration: {}", e);
                std::process::exit(1);
            }
        }
    };

    // Initialize logging: RUST_LOG env var overrides config if set
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

    // Identity provisioning: config nsec > key file > generate ephemeral
    let resolved = match resolve_identity(&config, &loaded_paths) {
        Ok(r) => r,
        Err(e) => {
            error!("Failed to resolve identity: {}", e);
            std::process::exit(1);
        }
    };
    match &resolved.source {
        IdentitySource::Config => info!("Using identity from configuration"),
        IdentitySource::KeyFile(p) => info!(path = %p.display(), "Loaded persistent identity from key file"),
        IdentitySource::Generated(p) => info!(path = %p.display(), "Generated persistent identity, saved to key file"),
        IdentitySource::Ephemeral => info!("Using ephemeral identity (new keypair each start)"),
    }

    // Create node with resolved identity
    let mut config = config;
    config.node.identity.nsec = Some(resolved.nsec);
    debug!("Creating node");
    let mut node = match Node::new(config) {
        Ok(node) => node,
        Err(e) => {
            error!("Failed to create node: {}", e);
            std::process::exit(1);
        }
    };

    // Log node information
    info!("Node created:");
    info!("      npub: {}", node.npub());
    info!("   node_addr: {}", hex::encode(node.node_addr().as_bytes()));
    info!("   address: {}", node.identity().address());
    info!("     state: {}", node.state());
    info!(" leaf_only: {}", node.is_leaf_only());

    // Start macOS CFRunLoop for CoreBluetooth NSStream callbacks
    // This MUST be started before node.start() so BLE can receive data
    run_loop::start();

    // Start the node (initializes TUN, spawns I/O threads)
    if let Err(e) = node.start().await {
        error!("Failed to start node: {}", e);
        std::process::exit(1);
    }

    info!("FIPS running, press Ctrl+C to exit");

    // Run the RX event loop until shutdown signal.
    // stop() drops the packet channel, causing run_rx_loop to exit.
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

    run_loop::stop();

    // Stop the node (shuts down transports, TUN, I/O threads)
    if let Err(e) = node.stop().await {
        warn!("Error during shutdown: {}", e);
    }

    info!("FIPS shutdown complete");
}

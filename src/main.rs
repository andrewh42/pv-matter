// The data-model handler chain is a deeply-nested tuple type (functional
// handlers + the root system clusters), so computing the `DataModel::run()`
// future's layout overflows the default query-depth limit.
#![recursion_limit = "512"]

//! Composition root for the pv-matter bridge.
//!
//! Two execution worlds: the tokio runtime hosts the MQTT ingest, and a
//! dedicated OS thread runs rs-matter via `block_on`. They communicate only
//! through channels — `DomainMsg` (ingest → here) folded into the state
//! accumulator, whose snapshots flow on to the matter thread.
//!
//! Startup bootstrap: the contract's topics are retained, so identity
//! (`info`) and current state normally arrive within milliseconds of the
//! first connect. We wait up to `PV_BOOTSTRAP_TIMEOUT` for `info` so Basic
//! Information can carry the real product name/serial, then start rs-matter
//! regardless — the device must come up for commissioning even against an
//! empty broker.

mod config;
mod domain;
mod ingest;
mod matter;
mod senml;

use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use rs_matter::dm::clusters::basic_info::BasicInfoConfig;
use rs_matter::dm::devices::test::{TEST_PID, TEST_VID};
use tokio::sync::mpsc;

use config::Config;
use domain::{DomainMsg, InverterInfo, State};

/// Stack for the rs-matter thread: the composed handler chain produces a very
/// large future.
const MATTER_THREAD_STACK: usize = 4 * 1024 * 1024;

/// ingest → main channel depth; retained replay is 3 messages, live traffic
/// is one pack per poll lap.
const MSG_CHANNEL_CAPACITY: usize = 64;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cfg = Config::from_env()?;
    std::fs::create_dir_all(&cfg.storage_path)
        .with_context(|| format!("creating storage dir {}", cfg.storage_path.display()))?;

    log::info!(
        "pv-matter {} starting (broker {}:{}, prefix {:?}, storage {})",
        env!("CARGO_PKG_VERSION"),
        cfg.mqtt_host,
        cfg.mqtt_port,
        cfg.topic_prefix,
        cfg.storage_path.display()
    );

    let (msg_tx, mut msg_rx) = mpsc::channel::<DomainMsg>(MSG_CHANNEL_CAPACITY);
    let mut ingest_task = tokio::spawn(ingest::run(cfg.clone(), msg_tx));

    // Bootstrap: fold messages until identity arrives (or the timeout).
    let mut state = State::default();
    let deadline = tokio::time::Instant::now() + cfg.bootstrap_timeout;
    while state.info.is_none() {
        match tokio::time::timeout_at(deadline, msg_rx.recv()).await {
            Ok(Some(msg)) => state.apply(msg),
            Ok(None) => break, // ingest gone; the select below will notice
            Err(_) => {
                log::warn!(
                    "no retained info within {:?}; starting with placeholder identity",
                    cfg.bootstrap_timeout
                );
                break;
            }
        }
    }
    if let Some(info) = &state.info {
        log::info!(
            "bootstrapped identity: serial {:?}, model {:?}, name {:?}",
            info.serial,
            info.device_model,
            info.name
        );
    }

    let dev_det = basic_info(state.info.as_ref());
    let (updates_tx, updates_rx) = async_channel::bounded::<domain::PvSnapshot>(16);
    let (shutdown_tx, shutdown_rx) = async_channel::bounded::<()>(1);

    // rs-matter on its own OS thread (block_on, !Send). The exit guard is
    // dropped on *any* exit — error, clean return, or panic — so the select
    // below notices a dead Matter side instead of running on headless.
    let storage_path = cfg.storage_path.clone();
    let matter_port = cfg.matter_port;
    let initial = state.snapshot.clone();
    let (matter_exited_tx, matter_exited_rx) = async_channel::bounded::<()>(1);
    let matter_thread: JoinHandle<()> = std::thread::Builder::new()
        .name("rs-matter".into())
        .stack_size(MATTER_THREAD_STACK)
        .spawn(move || {
            let _exit_guard = matter_exited_tx;
            if let Err(e) = matter::run(
                storage_path,
                dev_det,
                matter_port,
                initial,
                updates_rx,
                shutdown_rx,
            ) {
                log::error!("rs-matter thread exited with error: {e}");
            }
        })
        .context("spawning rs-matter thread")?;

    // Steady state: fold ingest messages, forward each resulting snapshot.
    let mut fatal = false;
    loop {
        tokio::select! {
            () = shutdown_signal() => {
                log::info!("shutdown requested");
                break;
            }
            _ = matter_exited_rx.recv() => {
                log::error!("rs-matter thread exited unexpectedly; shutting down");
                fatal = true;
                break;
            }
            res = &mut ingest_task => {
                log::error!("mqtt ingest task ended unexpectedly: {res:?}");
                fatal = true;
                break;
            }
            msg = msg_rx.recv() => {
                let Some(msg) = msg else { continue };
                state.apply(msg);
                // Bounded channel + a coalescing bridge: if the bridge is
                // waiting out its 1 s report floor, drop-oldest semantics via
                // try_send would lose the newest state — so block (briefly).
                if updates_tx.send(state.snapshot.clone()).await.is_err() {
                    log::error!("matter updates channel closed; shutting down");
                    fatal = true;
                    break;
                }
            }
        }
    }

    log::info!("stopping");
    let _ = shutdown_tx.send(()).await;
    join_matter_thread(matter_thread).await;
    ingest_task.abort();
    log::info!("stopped");
    anyhow::ensure!(!fatal, "a bridge component failed; see logs above");
    Ok(())
}

/// Basic Information from the bootstrap identity. Test VID/PID (real CSA
/// certification is out of scope); strings are leaked once for the process
/// lifetime because `Matter` borrows them for as long as it runs.
fn basic_info(info: Option<&InverterInfo>) -> &'static BasicInfoConfig<'static> {
    let product_name: &'static str = leak(
        info.and_then(|i| i.device_model.clone())
            .unwrap_or_else(|| "PV Inverter".into()),
    );
    let serial_no: &'static str = leak(
        info.and_then(|i| i.serial)
            .map(|s| s.to_string())
            .unwrap_or_default(),
    );
    Box::leak(Box::new(BasicInfoConfig {
        vid: TEST_VID,
        pid: TEST_PID,
        device_name: "PV Inverter",
        product_name,
        vendor_name: "pv-matter (unofficial)",
        serial_no,
        sw_ver_str: env!("CARGO_PKG_VERSION"),
        ..BasicInfoConfig::new()
    }))
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// Resolves on SIGINT (Ctrl-C) or SIGTERM.
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

/// Joins the rs-matter thread off the async runtime, with a timeout so a
/// stuck thread can't hang shutdown.
async fn join_matter_thread(handle: JoinHandle<()>) {
    let join = tokio::task::spawn_blocking(move || handle.join());
    match tokio::time::timeout(Duration::from_secs(5), join).await {
        Ok(Ok(Ok(()))) => log::info!("rs-matter thread stopped"),
        Ok(Ok(Err(_))) => log::warn!("rs-matter thread panicked during shutdown"),
        Ok(Err(e)) => log::warn!("failed to join rs-matter thread: {e}"),
        Err(_) => log::warn!("rs-matter thread did not stop within 5s; exiting anyway"),
    }
}

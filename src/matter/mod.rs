//! The rs-matter side of the bridge: builds the Solar Power node,
//! commissions it, and runs rs-matter's event loops on the calling thread.
//!
//! [`run`] is meant to be called via `futures_lite::future::block_on` on a
//! **dedicated OS thread**; the tokio side communicates only through the
//! `updates` channel (PV snapshots) and the `shutdown` channel.

mod bridge;
mod desc_tags;
mod eem;
mod epm;
mod node;
mod power_source;
mod power_topology;
mod temperature;

use core::cell::RefCell;
use core::pin::pin;

use std::net::{Ipv6Addr, SocketAddr, UdpSocket};
use std::path::PathBuf;

use embassy_futures::select::{select, select4};

use rs_matter::Matter;
use rs_matter::crypto::{Crypto, default_crypto};
use rs_matter::dm::clusters::basic_info::BasicInfoConfig;
use rs_matter::dm::devices::test::{DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_COMM};
use rs_matter::dm::networks::eth::EthNetwork;
use rs_matter::error::Error;
use rs_matter::im::{EthInteractionModelState, InteractionModel};
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::pairing::qr::QrTextType;
use rs_matter::persist::DirKvBlobStore;
use rs_matter::respond::DefaultResponder;
use rs_matter::sc::pase::MAX_COMM_WINDOW_TIMEOUT_SECS;
use rs_matter::transport::exchange::MatterBuffers;
use rs_matter::utils::init::InitMaybeUninit;
use rs_matter::utils::select::Coalesce;

use static_cell::StaticCell;

use crate::domain::PvSnapshot;

use bridge::run_bridge;
use node::{Handlers, dm_handler};

static MATTER: StaticCell<Matter> = StaticCell::new();
static BUFFERS: StaticCell<MatterBuffers<10>> = StaticCell::new();

/// Runs the composed Matter device to completion on the current thread.
///
/// - `storage_path` — persistent fabric storage (survives restarts).
/// - `dev_det` — Basic Information, built by the composition root from the
///   bootstrap `info` (product name/serial), `'static` because `Matter`
///   borrows it for the process lifetime.
/// - `initial` — snapshot seeded during bootstrap, so the first Matter read
///   reports real state instead of nulls.
/// - `updates` — PV snapshots from the MQTT side; the bridge turns them into
///   per-attribute change reports (and EEM events).
/// - `shutdown` — resolves (sent `()` or dropped sender) to return cleanly.
///
/// On first run (uncommissioned), prints the QR + manual pairing code.
pub fn run(
    storage_path: PathBuf,
    dev_det: &'static BasicInfoConfig<'static>,
    matter_port: u16,
    initial: PvSnapshot,
    updates: async_channel::Receiver<PvSnapshot>,
    shutdown: async_channel::Receiver<()>,
) -> Result<(), Error> {
    let matter = MATTER.uninit().init_with(Matter::init(
        dev_det,
        TEST_DEV_COMM,
        &TEST_DEV_ATT,
        matter_port,
    ));

    let kv = matter.kv(DirKvBlobStore::new(storage_path));
    futures_lite::future::block_on(matter.load_persist(&kv))?;

    log::info!(
        "matter fabric loaded (commissioned: {})",
        matter.is_commissioned()
    );

    let buffers = BUFFERS.uninit().init_with(MatterBuffers::init());

    let crypto = default_crypto(rand::thread_rng(), DAC_PRIVKEY);
    let mut rand = crypto.rand()?;

    // The shared snapshot the cluster handlers read and the bridge writes —
    // everything runs on this one executor, so a plain RefCell is sound.
    let state = RefCell::new(initial);
    let handlers = Handlers::new(&state, &mut rand);

    let mut im_state: EthInteractionModelState =
        EthInteractionModelState::new(EthNetwork::new_default());
    futures_lite::future::block_on(im_state.load_persist(&kv))?;

    let im = InteractionModel::new(
        matter,
        &crypto,
        &*buffers,
        dm_handler(rand, &handlers),
        &kv,
        &im_state,
    );

    let responder = DefaultResponder::new(&im);
    let mut respond = pin!(responder.run::<4, 4>());
    let mut dm_job = pin!(im.run());

    // Bind the same port we told Matter to advertise over mDNS (rs-matter's
    // MATTER_SOCKET_BIND_ADDR hardcodes 5540); the IPv6 wildcard matches it.
    let bind_addr = SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), matter_port);
    let socket = async_io::Async::<UdpSocket>::bind(bind_addr)?;
    let mut transport = pin!(matter.run(&crypto, &socket, &socket, &socket));

    // mDNS via Bonjour / DNS-SD (astro-dnssd) on both platforms: macOS links the
    // system mDNSResponder, Linux links libdns_sd from avahi-compat and talks to
    // the running avahi-daemon. Requires avahi-daemon up on the target box.
    let mut mdns_responder = rs_matter::transport::network::mdns::astro::AstroMdns::new();

    let mut mdns = pin!(mdns_responder.run(matter));

    // MQTT → Matter fan-out (per-attribute change reports + EEM events).
    let mut bridge = pin!(run_bridge(&state, &im, &updates));

    if !matter.is_commissioned() {
        matter.print_standard_qr_text(DiscoveryCapabilities::IP)?;
        matter.print_standard_qr_code(QrTextType::Unicode, DiscoveryCapabilities::IP)?;
        matter.open_basic_comm_window(MAX_COMM_WINDOW_TIMEOUT_SECS, &crypto, &())?;
    }

    let mut shutdown_signal = pin!(async {
        let _ = shutdown.recv().await;
        log::info!("matter shutdown signal");
        Ok::<(), Error>(())
    });

    let mut inner = pin!(select4(&mut transport, &mut mdns, &mut respond, &mut dm_job).coalesce());
    let mut core = pin!(select(&mut inner, &mut bridge).coalesce());
    let all = select(&mut core, &mut shutdown_signal).coalesce();
    futures_lite::future::block_on(all)
}

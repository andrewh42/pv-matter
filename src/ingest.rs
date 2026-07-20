//! MQTT ingest: an rumqttc v5 client subscribed to the sma-daemon topics,
//! decoding each retained/live publish into a [`DomainMsg`] and pushing it
//! onto the channel. Pure transport + topic routing — payload interpretation
//! lives in [`crate::domain`] / [`crate::senml`].
//!
//! The publisher's device id is self-assigned (`sma-<model>-<serial>`), so
//! unless `PV_DEVICE_ID` pins one we lock onto the first device id seen and
//! ignore (with one warning) anything else.
//!
//! Reconnects: `EventLoop::poll` returns the error and reconnects on the next
//! call; we re-subscribe on every ConnAck. All three topics are retained, so
//! state replays by itself after every (re)connect.

use std::time::Duration;

use rumqttc::v5::mqttbytes::QoS;
use rumqttc::v5::{AsyncClient, Event, Incoming, MqttOptions};
use tokio::sync::mpsc;

use crate::config::Config;
use crate::domain::{DomainMsg, InverterInfo, LinkStatus};
use crate::senml;

/// How long to back off after an event-loop error before polling again
/// (rumqttc reconnects on the next poll).
const RECONNECT_DELAY: Duration = Duration::from_secs(3);

/// Runs forever (or until the receiver side closes). Consumes the task.
pub async fn run(cfg: Config, tx: mpsc::Sender<DomainMsg>) {
    let client_id = format!("pv-matter-{}", std::process::id());
    let mut options = MqttOptions::new(client_id, cfg.mqtt_host.clone(), cfg.mqtt_port);
    options.set_keep_alive(Duration::from_secs(30));
    options.set_clean_start(true);
    if let (Some(user), Some(pass)) = (&cfg.mqtt_username, &cfg.mqtt_password) {
        options.set_credentials(user.clone(), pass.clone());
    }

    let (client, mut eventloop) = AsyncClient::new(options, 64);

    let mut router = Router::new(cfg.topic_prefix.clone(), cfg.device_id.clone());

    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                log::info!(
                    "mqtt connected to {}:{}; subscribing under {}/+/…",
                    cfg.mqtt_host,
                    cfg.mqtt_port,
                    cfg.topic_prefix
                );
                for leaf in ["info", "instantaneous", "status"] {
                    let filter = format!("{}/+/{leaf}", cfg.topic_prefix);
                    if let Err(e) = client.subscribe(filter.clone(), QoS::AtLeastOnce).await {
                        log::error!("mqtt subscribe {filter} failed: {e}");
                    }
                }
            }
            Ok(Event::Incoming(Incoming::Publish(publish))) => {
                let topic = String::from_utf8_lossy(&publish.topic).into_owned();
                if let Some(msg) = router.route(&topic, &publish.payload)
                    && tx.send(msg).await.is_err()
                {
                    // Consumer gone: the app is shutting down.
                    return;
                }
            }
            Ok(_) => {}
            Err(e) => {
                log::warn!("mqtt event loop error: {e}; retrying in {RECONNECT_DELAY:?}");
                tokio::time::sleep(RECONNECT_DELAY).await;
            }
        }
    }
}

/// Topic → [`DomainMsg`] routing with device-id lock-on. Separated from the
/// event loop so it is testable without a broker.
struct Router {
    prefix: String,
    /// `Some` once pinned (from config) or locked onto the first device seen.
    device_id: Option<String>,
    warned_other_device: bool,
}

impl Router {
    fn new(prefix: String, pinned_device: Option<String>) -> Self {
        Self {
            prefix,
            device_id: pinned_device,
            warned_other_device: false,
        }
    }

    fn route(&mut self, topic: &str, payload: &[u8]) -> Option<DomainMsg> {
        let rest = topic
            .strip_prefix(self.prefix.as_str())?
            .strip_prefix('/')?;
        let (device, leaf) = rest.split_once('/')?;
        if device.is_empty() || leaf.contains('/') {
            return None;
        }

        match &self.device_id {
            None => {
                log::info!("locked onto device id {device:?}");
                self.device_id = Some(device.to_owned());
            }
            Some(locked) if locked != device => {
                if !self.warned_other_device {
                    log::warn!(
                        "ignoring second device {device:?} (locked onto {locked:?}; \
                         set PV_DEVICE_ID to choose explicitly)"
                    );
                    self.warned_other_device = true;
                }
                return None;
            }
            Some(_) => {}
        }

        match leaf {
            "info" => match InverterInfo::parse(payload) {
                Ok(info) => Some(DomainMsg::Info(info)),
                Err(e) => {
                    log::warn!("unparseable info payload on {topic}: {e}");
                    None
                }
            },
            "instantaneous" => match senml::parse_pack(payload) {
                Ok(pack) => Some(DomainMsg::Pack(pack)),
                Err(e) => {
                    log::warn!("unparseable senml pack on {topic}: {e}");
                    None
                }
            },
            "status" => {
                let text = String::from_utf8_lossy(payload);
                match LinkStatus::parse(&text) {
                    Some(status) => Some(DomainMsg::Status(status)),
                    None => {
                        log::warn!("unknown status payload on {topic}: {text:?}");
                        None
                    }
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router() -> Router {
        Router::new("pv-inverter".into(), None)
    }

    #[test]
    fn routes_status_and_locks_onto_first_device() {
        let mut r = router();
        assert_eq!(
            r.route("pv-inverter/sma-sb5000tl-2130012345/status", b"online"),
            Some(DomainMsg::Status(LinkStatus::Online))
        );
        // A second device id is ignored once locked.
        assert_eq!(r.route("pv-inverter/sma-other-1/status", b"online"), None);
        // The locked device keeps flowing.
        assert_eq!(
            r.route("pv-inverter/sma-sb5000tl-2130012345/status", b"asleep"),
            Some(DomainMsg::Status(LinkStatus::Asleep))
        );
    }

    #[test]
    fn pinned_device_never_locks_elsewhere() {
        let mut r = Router::new("pv-inverter".into(), Some("sma-x-9".into()));
        assert_eq!(r.route("pv-inverter/sma-other-1/status", b"online"), None);
        assert_eq!(
            r.route("pv-inverter/sma-x-9/status", b"online"),
            Some(DomainMsg::Status(LinkStatus::Online))
        );
    }

    #[test]
    fn foreign_prefixes_and_shapes_are_ignored() {
        let mut r = router();
        assert_eq!(r.route("other/sma-x-9/status", b"online"), None);
        assert_eq!(r.route("pv-inverter/status", b"online"), None);
        assert_eq!(r.route("pv-inverter/sma-x-9/status/extra", b"online"), None);
        // Unknown leaf under a locked device: ignored.
        assert_eq!(r.route("pv-inverter/sma-x-9/unknown", b"x"), None);
    }

    #[test]
    fn routes_info_and_pack_payloads() {
        let mut r = router();
        let info = r.route(
            "pv-inverter/sma-x-9/info",
            br#"{"schema":1,"serial":42,"lines":1,"strings":2}"#,
        );
        assert!(matches!(info, Some(DomainMsg::Info(i)) if i.serial == Some(42)));

        let pack = r.route(
            "pv-inverter/sma-x-9/instantaneous",
            br#"[{"bn":"urn:dev:ser:42:","bt":1,"bu":"W"},{"n":"ac/total_power","v":10}]"#,
        );
        assert!(matches!(pack, Some(DomainMsg::Pack(_))));

        // Corrupt payloads are dropped, not crashed on.
        assert_eq!(r.route("pv-inverter/sma-x-9/info", b"{"), None);
        assert_eq!(r.route("pv-inverter/sma-x-9/instantaneous", b"nope"), None);
    }
}

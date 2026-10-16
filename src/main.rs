use clap::Parser as _;
use futures::StreamExt as _;
use homie5::client::Publish as HomiePublish;
use homie5::client::Subscription;
use homie5::device_description::DeviceDescriptionBuilder;
use homie5::device_description::HomieDeviceDescription;
use homie5::device_description::HomieNodeDescription;
use homie5::device_description::NodeDescriptionBuilder;
use homie5::device_description::PropertyDescriptionBuilder;
use homie5::{HOMIE_UNIT_DEGREE, HOMIE_UNIT_PERCENT};
use homie5::{Homie5DeviceProtocol, HomieDeviceStatus, HomieID};
use rumqttc::v5::MqttOptions;
use rumqttc::v5::mqttbytes::v5::LastWill;
use rumqttc::v5::mqttbytes::v5::Publish;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc, oneshot, watch};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

mod api;
mod schemas;

static BATTERY_NODE_ID: HomieID = HomieID::new_const("battery");
static BATTERY_LEVEL_PROP_ID: HomieID = HomieID::new_const("level");
static POSITION_NODE_ID: HomieID = HomieID::new_const("position");
static POSITION_LATLON_PROP_ID: HomieID = HomieID::new_const("latitude-longitude");
static MOWER_NODE_ID: HomieID = HomieID::new_const("mower");
static MOWER_MODE_PROP_ID: HomieID = HomieID::new_const("mode");
static MOWER_ACTIVITY_PROP_ID: HomieID = HomieID::new_const("activity");
static MOWER_INACTIVE_REASON_PROP_ID: HomieID = HomieID::new_const("inactive-reason");
static MOWER_STATE_PROP_ID: HomieID = HomieID::new_const("state");
static MOWER_ERROR_CODE_PROP_ID: HomieID = HomieID::new_const("error-code");
static MOWER_MOWING_PROP_ID: HomieID = HomieID::new_const("mowing");
static MOWER_PARKED_PROP_ID: HomieID = HomieID::new_const("parked");
static MOWER_PAUSED_PROP_ID: HomieID = HomieID::new_const("paused");
static MOWER_INACTIVE_PROP_ID: HomieID = HomieID::new_const("inactive");
static MOWER_WORK_AREA_ID_PROP_ID: HomieID = HomieID::new_const("work-area");
static PLANNER_NODE_ID: HomieID = HomieID::new_const("planner");
static PLANNER_NEXT_START_PROP_ID: HomieID = HomieID::new_const("next-start");
static PLANNER_OVERRIDE_PROP_ID: HomieID = HomieID::new_const("override-action");
static PLANNER_RESTRICTED_REASON_PROP_ID: HomieID = HomieID::new_const("restricted-reason");
static PLANNER_EXTERNAL_REASON_PROP_ID: HomieID = HomieID::new_const("external-reason");
static SETTINGS_NODE_ID: HomieID = HomieID::new_const("settings");
static SETTINGS_CUTTING_HEIGHT_PROP_ID: HomieID = HomieID::new_const("cutting-height");
static SETTINGS_HEADLIGHT_PROP_ID: HomieID = HomieID::new_const("headlight-mode");
static WORK_AREA_ID_PROP_ID: HomieID = HomieID::new_const("id");
static WORK_AREA_NAME_PROP_ID: HomieID = HomieID::new_const("name");
static WORK_AREA_TYPE_PROP_ID: HomieID = HomieID::new_const("type");
static WORK_AREA_CUTTING_HEIGHT_PROP_ID: HomieID = HomieID::new_const("cutting-height");
static WORK_AREA_ENABLED_PROP_ID: HomieID = HomieID::new_const("enabled");
static WORK_AREA_SCHEDULABLE_PROP_ID: HomieID = HomieID::new_const("schedulable");
static WORK_AREA_ORIENTATION_PROP_ID: HomieID = HomieID::new_const("orientation");
static WORK_AREA_ORIENTATION_SHIFT_PROP_ID: HomieID = HomieID::new_const("orientation-shift");
static WORK_AREA_CURRENT_ORIENTATION_PROP_ID: HomieID = HomieID::new_const("current-orientation");
static WORK_AREA_OVERLAP_PROP_ID: HomieID = HomieID::new_const("overlap");
static WORK_AREA_PROGRESS_PROP_ID: HomieID = HomieID::new_const("progress");
static WORK_AREA_LAST_TIME_COMPLETED_PROP_ID: HomieID = HomieID::new_const("last-time-completed");
static WORK_AREA_LAST_TIME_ABANDONED_PROP_ID: HomieID = HomieID::new_const("last-time-abandoned");
static WORK_AREA_USE_GLOBAL_CUTTING_HEIGHT_PROP_ID: HomieID =
    HomieID::new_const("use-global-cutting-height");
static WORK_AREA_CALENDAR_PROP_ID: HomieID = HomieID::new_const("calendar");
static STAY_OUT_ZONE_ID_PROP_ID: HomieID = HomieID::new_const("id");
static STAY_OUT_ZONE_NAME_PROP_ID: HomieID = HomieID::new_const("name");
static STAY_OUT_ZONE_ENABLED_PROP_ID: HomieID = HomieID::new_const("enabled");
static STAY_OUT_ZONE_DIRTY_PROP_ID: HomieID = HomieID::new_const("dirty");

const USER_AGENT: &str = concat!(
    env!("CARGO_PKG_NAME"),
    "/",
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("CARGO_PKG_REPOSITORY"),
    ")"
);
const CONNECT_TIMEOUT: Duration = Duration::new(10, 0);
const API_REQUEST_TIMEOUT: Duration = Duration::new(20, 0);
const PROPERTY_TARGET_TIMEOUT: Duration = Duration::from_secs(40);

#[derive(Clone, Debug)]
struct ExternalInhibit {
    name: HomieID,
    reason: i32,
    duration_minutes: u16,
}

impl std::str::FromStr for ExternalInhibit {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (name, rest) = value
            .split_once('=')
            .ok_or("expected NAME=REASON:DURATION")?;
        let (reason, duration) = rest
            .split_once(':')
            .ok_or("expected NAME=REASON:DURATION")?;
        let name = HomieID::from_str(&format!("external-inhibit-{name}"))
            .map_err(|_| "invalid inhibit name for a property")?;
        let reason = reason
            .parse::<i32>()
            .map_err(|_| "invalid external reason")?;
        if !(200_000..300_000).contains(&reason) {
            return Err("external reason must be in [200000, 300000)".into());
        }
        let duration_minutes = duration
            .parse::<u16>()
            .map_err(|_| "invalid inhibit duration")?;
        if !(1..=1500).contains(&duration_minutes) {
            return Err("duration must be in [1, 1500]".into());
        }
        Ok(Self {
            name,
            reason,
            duration_minutes,
        })
    }
}

/// Read the value stored in the specified register.
#[derive(clap::Parser)]
struct Args {
    /// How to connect to the MQTT broker.
    ///
    /// The value is expected to be provided as an URL, such as:
    /// `mqtt://location:1883?client_id=hostname` for plain text connection or
    /// `mqtts://location:1883?client_id=hostname` for TLS protected connection.
    #[clap(short = 'm', long)]
    mqtt_broker: String,

    /// To be provided together with `--mqtt-password` to use password based authentication
    /// with the broker.
    #[clap(short = 'u', long, requires = "mqtt_password")]
    mqtt_user: Option<String>,

    /// To be provided together with `--mqtt-user` to use password based authentication with
    /// the broker.
    #[clap(short = 'p', long, requires = "mqtt_user")]
    mqtt_password: Option<String>,

    #[clap(long, default_value = "automower2mqtt")]
    device_name: HomieID,

    #[clap(long, default_value = "info", env = "AUTOMOWER2MQTT_LOG")]
    log_filter: tracing_subscriber::filter::targets::Targets,

    /// Application key credential from your application in Husqvarna Developer API portal.
    #[clap(short = 'k', long, env = "AUTOMOWER_APP_KEY")]
    app_key: String,

    /// Application secret credential from your application in Husqvarna Developer API portal.
    #[clap(short = 's', long, env = "AUTOMOWER_APP_SECRET")]
    app_secret: String,

    /// Configure an external inhibit as `NAME=REASON:DURATION_MINUTES`.
    ///
    /// This will expose additional properties `planner/external-inhibit-NAME` which, when activated
    /// will have mower go back to the charging station for `DURATION_MINUTES`. This inhibit can be
    /// continuously reactivated to extend the duration.
    ///
    /// e.g. --external-inhibit rain=200101:1500 --external-inhibit dew=200102:60
    #[clap(long)]
    external_inhibit: Vec<ExternalInhibit>,
}

#[derive(thiserror::Error, Debug)]
enum Error {
    #[error("could not parse the `--mqtt-broker` argument")]
    ParseMqttBroker(#[source] rumqttc::v5::OptionError),
    #[error("mqtt connection error")]
    MqttConnection(#[source] rumqttc::v5::ConnectionError),
    #[error("could not publish init value to the state topic")]
    PublishInitState(#[source] rumqttc::v5::ClientError),
    #[error("could not construct device description message")]
    GenerateDescription(#[source] homie5::Homie5ProtocolError),
    #[error("could not publish the device description")]
    PublishDescription(#[source] rumqttc::v5::ClientError),
    #[error("could not publish ready value to the state topic")]
    PublishReadyState(#[source] rumqttc::v5::ClientError),
    #[error("could not construct the mqtt subscribtion message")]
    GenerateSubscribtions(#[source] homie5::Homie5ProtocolError),
    #[error("could not subscribe to the homie properties")]
    Subscribe(#[source] rumqttc::v5::ClientError),
    #[error(transparent)]
    Api(#[from] api::Error),
    #[error("could not publish value to `{1}/{2}`")]
    PublishValue(#[source] rumqttc::v5::ClientError, HomieID, HomieID),
    #[error("could not publish value to `{0}/{1}/$target`")]
    PublishTarget(#[source] rumqttc::v5::ClientError, HomieID, HomieID),
    #[error("could not join the monitor thread")]
    MonitorJoin(#[source] tokio::task::JoinError),
    #[error("could not join the API client task")]
    ApiJoin(#[source] tokio::task::JoinError),
    #[error("API client stopped unexpectedly")]
    ApiStopped,
    #[error("disconnected from the MQTT server")]
    MqttDisconnect,
    #[error("Homie property task `{0}/{1}/{2}` has stopped")]
    PropertyTaskStopped(String, String, String),
}

fn main() {
    let args = Args::parse();
    std::process::exit(match setup_and_run(args) {
        Ok(_) => 0,
        Err(e) => {
            print_error(&e);
            1
        }
    });
}

fn print_error(e: &dyn std::error::Error) {
    eprintln!("error: {e}");
    let mut cause = e.source();
    while let Some(e) = cause {
        eprintln!("  because: {e}");
        cause = e.source();
    }
}

#[expect(clippy::result_large_err)]
fn setup_and_run(args: Args) -> Result<(), Error> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(args.log_filter.clone())
        .init();
    tracing::debug!(filter = ?args.log_filter, message = "logging initiated");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("TODO");
    let client = reqwest::ClientBuilder::new()
        .user_agent(USER_AGENT)
        .referer(false)
        .connect_timeout(CONNECT_TIMEOUT)
        .connection_verbose(true)
        .timeout(API_REQUEST_TIMEOUT)
        .https_only(true)
        .build()
        .expect("TODO");
    let (protocol, lw) = Homie5DeviceProtocol::new(args.device_name, homie5::HomieDomain::Default);
    let mut mqtt_options =
        MqttOptions::parse_url(&args.mqtt_broker).map_err(Error::ParseMqttBroker)?;
    if let (Some(u), Some(p)) = (args.mqtt_user.as_ref(), args.mqtt_password.as_ref()) {
        mqtt_options.set_credentials(u, p);
    }
    let lw = LastWill::new(lw.topic, lw.message, convert_qos(lw.qos), lw.retain, None);
    mqtt_options.set_last_will(lw);
    let (mqtt, mqtt_loop) = rumqttc::v5::AsyncClient::new(mqtt_options, 100);
    let context = Arc::new(Context {
        api: Arc::new(api::Client::new(client, args.app_key, args.app_secret)),
        mowers: Default::default(),
        protocol,
        mqtt,
        external_inhibits: args.external_inhibit.into(),
        property_inputs: Default::default(),
    });
    runtime.block_on(context.run(mqtt_loop))
}

enum PropertyTaskMessage {
    Set(String),
    Publish(oneshot::Sender<Result<(), Error>>),
    Erase(oneshot::Sender<Result<(), Error>>),
}

type PropertyKey = (String, String, String);
type PropertyTaskSender = mpsc::UnboundedSender<PropertyTaskMessage>;
type PropertyInputs = Arc<Mutex<BTreeMap<PropertyKey, PropertyTaskSender>>>;

struct MowerContext {
    api: Arc<api::Mower>,
    api_client: Arc<api::Client>,
    external_inhibits: Arc<[ExternalInhibit]>,
    mqtt: rumqttc::v5::AsyncClient,
    protocol: Homie5DeviceProtocol,
    description: HomieDeviceDescription,
    status: HomieDeviceStatus,
    property_ready: watch::Sender<bool>,
    property_inputs: PropertyInputs,
}

/// Owns everything about a single Homie property: the deduplication of published values, the
/// `$target` lifecycle and the handling of the `set` commands.
struct PropertyTask {
    api: Arc<api::Mower>,
    api_client: Arc<api::Client>,
    external_inhibits: Arc<[ExternalInhibit]>,
    mqtt: rumqttc::v5::AsyncClient,
    protocol: Homie5DeviceProtocol,
    node_id: HomieID,
    prop_id: HomieID,
    settable: bool,
    retained: bool,
    inputs: mpsc::UnboundedReceiver<PropertyTaskMessage>,
    api_changes: watch::Receiver<u64>,
    ready: watch::Receiver<bool>,
    last_value: Option<Option<String>>,
    last_target: Option<Option<String>>,
    pending_target: Option<PendingTarget>,
}

/// A value that was requested through `set` and that the API is yet to report.
struct PendingTarget {
    /// The payload exactly as received, which is what `$target` has to be.
    requested: String,
    /// What the property is going to read once the API reflects the request.
    expected: String,
    deadline: tokio::time::Instant,
    /// Has the API poll for the change for as long as the target is pending.
    _wait: Option<api::ChangeWait>,
}

struct Context {
    api: Arc<api::Client>,
    mowers: Mutex<BTreeMap<String, Arc<Mutex<MowerContext>>>>,
    protocol: Homie5DeviceProtocol,
    mqtt: rumqttc::v5::AsyncClient,
    external_inhibits: Arc<[ExternalInhibit]>,
    property_inputs: PropertyInputs,
}

impl Context {
    async fn publish_root_device(self: &Arc<Self>) -> Result<(), Error> {
        let mut root_description = DeviceDescriptionBuilder::new().name("Husqvarna Automowers");
        {
            self.api.wait_initialized().await;
            let api_mowers = self.api.mowers().await;
            for mower in &api_mowers {
                let child_id =
                    HomieID::try_from(MowerContext::id_to_key(mower.id(), &self.protocol))
                        .expect("known valid child device ID");
                root_description = root_description.add_child(child_id);
            }
            // Stop the property tasks of the previous connection; they are bound to the previous
            // `MowerContext`s.
            self.property_inputs.lock().await.clear();
            let mut mowers = self.mowers.lock().await;
            for api_mower in api_mowers {
                let mower = MowerContext::new(
                    api_mower,
                    Arc::clone(&self.api),
                    self.mqtt.clone(),
                    &self.protocol,
                    Arc::clone(&self.external_inhibits),
                    Arc::clone(&self.property_inputs),
                )
                .await;
                let mower_id = mower.protocol.device_ref().device_id();
                mowers.insert(mower_id.to_string(), Arc::new(Mutex::new(mower)));
            }
        }
        let root_description = root_description.build();
        for step in homie5::homie_device_publish_steps() {
            match step {
                homie5::DevicePublishStep::DeviceStateInit => {
                    let p = self.protocol.publish_state(HomieDeviceStatus::Init);
                    self.mqtt
                        .homie_publish(p)
                        .await
                        .map_err(Error::PublishInitState)?;
                }
                homie5::DevicePublishStep::DeviceDescription => {
                    let p = self
                        .protocol
                        .publish_description(&root_description)
                        .map_err(Error::GenerateDescription)?;
                    self.mqtt
                        .homie_publish(p)
                        .await
                        .map_err(Error::PublishDescription)?;
                }
                homie5::DevicePublishStep::PropertyValues => {}
                homie5::DevicePublishStep::SubscribeProperties => {}
                homie5::DevicePublishStep::DeviceStateReady => {
                    let p = self.protocol.publish_state(HomieDeviceStatus::Ready);
                    self.mqtt
                        .homie_publish(p)
                        .await
                        .map_err(Error::PublishReadyState)?;
                }
            }
        }
        Ok(())
    }

    async fn run(self: Arc<Self>, mut mqtt_loop: rumqttc::v5::EventLoop) -> Result<(), Error> {
        let mut api_handle = tokio::spawn(Arc::clone(&self.api).run());
        let mut monitor_handle = None;
        loop {
            use rumqttc::Outgoing;
            use rumqttc::v5::Event;
            use rumqttc::v5::mqttbytes::v5::Packet;

            let result = tokio::select! {
                r = mqtt_loop.poll() => r,
                api_result = &mut api_handle => {
                    return match api_result {
                        Ok(Err(error)) => Err(Error::Api(error)),
                        Ok(Ok(())) => Err(Error::ApiStopped),
                        Err(error) => Err(Error::ApiJoin(error)),
                    };
                },
                join_result = async { monitor_handle.as_mut().unwrap().await }, if monitor_handle.is_some() => {
                    match join_result {
                        Ok(error) => return Err(error),
                        Err(join_error) => return Err(Error::MonitorJoin(join_error)),
                    }
                },
            };
            match result.map_err(Error::MqttConnection)? {
                Event::Incoming(Packet::ConnAck(_)) => {
                    tracing::debug!("connected to mqtt");
                    let this = Arc::clone(&self);
                    let joiner = tokio::spawn(async move {
                        if let Err(e) = this.publish_root_device().await {
                            return e;
                        }
                        let mowers = this
                            .mowers
                            .lock()
                            .await
                            .values()
                            .cloned()
                            .collect::<Vec<_>>();
                        for mower in &mowers {
                            let mut mower = mower.lock().await;
                            mower.ensure_property_tasks().await;
                            if let Err(error) = mower.publish_device().await {
                                return error;
                            }
                        }
                        let mut listeners = mowers
                            .into_iter()
                            .map(MowerContext::monitor_api_changes)
                            .collect::<futures::stream::FuturesUnordered<_>>();
                        while let Some(result) = listeners.next().await {
                            if let Err(error) = result {
                                return error;
                            }
                        }
                        Error::ApiStopped
                    });
                    if let Some(previous) = monitor_handle.replace(joiner) {
                        previous.abort();
                    }
                }
                Event::Incoming(Packet::Publish(msg)) => {
                    self.route_property_input(msg).await;
                }
                Event::Outgoing(Outgoing::Disconnect) => {
                    return Err(Error::MqttDisconnect);
                }
                event @ Event::Incoming(_) | event @ Event::Outgoing(_) => {
                    tracing::trace!(?event, "not handled in any way");
                }
            }
        }
    }

    async fn route_property_input(&self, msg: Publish) {
        let topic = match std::str::from_utf8(&msg.topic) {
            Ok(topic) => topic,
            Err(error) => {
                tracing::warn!(%error, "received an MQTT publish with a non-UTF8 topic");
                return;
            }
        };
        let (property, value) = match homie5::parse_mqtt_message(topic, &msg.payload) {
            Ok(homie5::Homie5Message::PropertySet {
                property,
                set_value,
            }) => (property, set_value),
            Ok(_) => {
                tracing::trace!(%topic, "ignoring non-set MQTT publish");
                return;
            }
            Err(error) => {
                tracing::warn!(%topic, %error, "ignoring invalid MQTT property publish");
                return;
            }
        };
        let key = (
            property.device_id().to_string(),
            property.node_id().to_string(),
            property.prop_id().to_string(),
        );
        let sender = self.property_inputs.lock().await.get(&key).cloned();
        match sender {
            Some(sender) if sender.send(PropertyTaskMessage::Set(value)).is_ok() => {}
            Some(_) => tracing::warn!(?key, "Homie property task has stopped"),
            None => tracing::warn!(?key, "MQTT set publish has no owning Homie property task"),
        }
    }
}

impl PropertyTask {
    async fn run(mut self) {
        tracing::debug!(node_id = %self.node_id, prop_id = %self.prop_id, "started Homie property task");
        loop {
            let deadline = self.pending_target.as_ref().map(|pending| pending.deadline);
            let result = tokio::select! {
                message = self.inputs.recv() => match message {
                    Some(message) => self.handle_message(message).await,
                    None => break,
                },
                changed = self.api_changes.changed() => {
                    if changed.is_err() { break; }
                    self.publish_api_state().await
                },
                ready = self.ready.changed() => {
                    if ready.is_err() { break; }
                    self.publish_api_state().await
                },
                _ = async {
                    match deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => futures::future::pending().await,
                    }
                } => self.expire_target().await,
            };
            if let Err(error) = result {
                tracing::error!(
                    error = &error as &dyn std::error::Error,
                    node_id = %self.node_id,
                    prop_id = %self.prop_id,
                    "error in Homie property task"
                );
            }
        }
        tracing::debug!(node_id = %self.node_id, prop_id = %self.prop_id, "stopped Homie property task");
    }

    async fn handle_message(&mut self, message: PropertyTaskMessage) -> Result<(), Error> {
        match message {
            PropertyTaskMessage::Set(value) => self.set(&value).await,
            PropertyTaskMessage::Publish(reply) => {
                let _ = reply.send(self.publish_all().await);
                Ok(())
            }
            PropertyTaskMessage::Erase(reply) => {
                let _ = reply.send(self.erase().await);
                Ok(())
            }
        }
    }

    async fn api_value(&self) -> Option<String> {
        let snapshot = self.api.snapshot().await;
        homie_property_value(
            &snapshot,
            self.node_id.as_str(),
            self.prop_id.as_str(),
            &self.external_inhibits,
        )
    }

    /// Forward the command to the API and start waiting for the API to report the new value.
    async fn set(&mut self, value: &str) -> Result<(), Error> {
        let Some(command) = parse_set(
            &self.external_inhibits,
            self.node_id.as_str(),
            self.prop_id.as_str(),
            value,
        ) else {
            return self.reject().await;
        };
        // The command is accepted, so `$target` goes first, then the request that starts the
        // transition is made.
        let previous = self.pending_target.replace(PendingTarget {
            requested: value.to_owned(),
            expected: command.expected.clone(),
            deadline: tokio::time::Instant::now() + PROPERTY_TARGET_TIMEOUT,
            _wait: None,
        });
        let sent = async {
            if *self.ready.borrow() {
                self.publish_target(Some(value.to_owned()), false).await?;
            }
            command
                .request
                .send(&self.api_client, self.api.id())
                .await
                .map_err(Error::from)
        }
        .await;
        if let Err(error) = sent {
            // Whatever was being waited for before is still the target.
            self.pending_target = previous;
            if let Err(error) = self.reject().await {
                tracing::error!(
                    error = &error as &dyn std::error::Error,
                    node_id = %self.node_id,
                    prop_id = %self.prop_id,
                    "could not publish the rejection of a command"
                );
            }
            return Err(error);
        }
        // Work areas and stay-out zones are not covered by the websocket, so have the API poll
        // for the change.
        let wait = is_polled_node(self.node_id.as_str()).then(|| {
            let (node_id, prop_id) = (self.node_id.clone(), self.prop_id.clone());
            let external_inhibits = Arc::clone(&self.external_inhibits);
            let expected = Some(command.expected);
            self.api.wait_for(move |data| {
                homie_property_value(data, node_id.as_str(), prop_id.as_str(), &external_inhibits)
                    == expected
            })
        });
        if let Some(pending) = &mut self.pending_target {
            pending.deadline = tokio::time::Instant::now() + PROPERTY_TARGET_TIMEOUT;
            pending._wait = wait;
        }
        Ok(())
    }

    /// Tell controllers that a command was not accepted by publishing the unchanged target again.
    async fn reject(&mut self) -> Result<(), Error> {
        if !*self.ready.borrow() {
            return Ok(());
        }
        let target = match &self.pending_target {
            Some(pending) => Some(pending.requested.clone()),
            None => self.api_value().await,
        };
        self.publish_target(target, true).await
    }

    /// Publish what the API currently reports, unless that is what is already published.
    async fn publish_api_state(&mut self) -> Result<(), Error> {
        if !*self.ready.borrow() {
            return Ok(());
        }
        let value = self.api_value().await;
        if self.settable {
            // `$target` is published ahead of the value it announces. Once the API reports what was
            // asked for, `$target` is left as it is: the value is only functionally equivalent
            // (e.g. `20.0` for `20`), which is fine for the value, but not a new target.
            // `None` means leaving `$target` as it is.
            let target = match &self.pending_target {
                Some(pending) if value.as_ref() == Some(&pending.expected) => {
                    self.pending_target = None;
                    None
                }
                Some(pending) => Some(Some(pending.requested.clone())),
                None if self.last_value.as_ref() != Some(&value) => Some(value.clone()),
                None => None,
            };
            if let Some(target) = target {
                self.publish_target(target, false).await?;
            }
        }
        self.publish_value(value, false).await
    }

    /// Give up waiting for the API to reach the requested target.
    async fn expire_target(&mut self) -> Result<(), Error> {
        self.pending_target = None;
        if !*self.ready.borrow() {
            return Ok(());
        }
        let value = self.api_value().await;
        self.publish_target(value.clone(), false).await?;
        self.publish_value(value, false).await
    }

    async fn publish_all(&mut self) -> Result<(), Error> {
        let value = self.api_value().await;
        if self.settable {
            let target = match (&self.pending_target, &self.last_value, &self.last_target) {
                (Some(pending), ..) => Some(pending.requested.clone()),
                // Nothing changed since the last time; keep the target the way it was.
                (None, Some(last_value), Some(last_target)) if *last_value == value => {
                    last_target.clone()
                }
                (None, ..) => value.clone(),
            };
            self.publish_target(target, false).await?;
        }
        self.publish_value(value, true).await
    }

    async fn erase(&mut self) -> Result<(), Error> {
        if self.settable {
            self.publish_target(None, false).await?;
        }
        self.publish_value(None, false).await
    }

    async fn publish_value(&mut self, value: Option<String>, force: bool) -> Result<(), Error> {
        if !force && self.last_value.as_ref() == Some(&value) {
            return Ok(());
        }
        let mut publish = self.protocol.publish_value(
            &self.node_id,
            &self.prop_id,
            value.clone().unwrap_or_default(),
            self.retained,
        );
        if value.is_none() {
            publish.payload.clear();
        }
        self.mqtt.homie_publish(publish).await.map_err(|error| {
            Error::PublishValue(error, self.node_id.clone(), self.prop_id.clone())
        })?;
        self.last_value = Some(value);
        Ok(())
    }

    async fn publish_target(&mut self, target: Option<String>, force: bool) -> Result<(), Error> {
        if !force && self.last_target.as_ref() == Some(&target) {
            return Ok(());
        }
        let clear = target.is_none();
        let mut publish = self.protocol.publish_target(
            &self.node_id,
            &self.prop_id,
            target.clone().unwrap_or_default(),
            clear || self.retained,
        );
        if clear {
            publish.payload.clear();
        }
        self.mqtt.homie_publish(publish).await.map_err(|error| {
            Error::PublishTarget(error, self.node_id.clone(), self.prop_id.clone())
        })?;
        self.last_target = Some(target);
        Ok(())
    }
}

/// A validated `set` command, ready to be sent to the API.
struct SetCommand {
    /// What the property is going to read once the API reflects the command.
    expected: String,
    request: SetRequest,
}

enum SetRequest {
    /// `POST /mowers/{id}/{path}`
    Post {
        path: String,
        body: serde_json::Value,
    },
    /// `PATCH /mowers/{id}/{path}`
    Patch {
        path: String,
        body: serde_json::Value,
    },
}

impl SetRequest {
    async fn send(self, api_client: &api::Client, mower_id: &str) -> Result<(), api::Error> {
        match self {
            SetRequest::Post { path, body } => api_client.post_mower(mower_id, &path, body).await,
            SetRequest::Patch { path, body } => api_client.patch_mower(mower_id, &path, body).await,
        }
    }
}

/// Validate a `set` command and work out the API request that carries it out.
///
/// Returns `None` if the command is invalid or not supported, in which case it must not be
/// accepted.
fn parse_set(
    external_inhibits: &[ExternalInhibit],
    node_id: &str,
    prop_id: &str,
    val: &str,
) -> Option<SetCommand> {
    if let Some(work_area_id) = parse_work_area_node_id(node_id) {
        if prop_id == "calendar" {
            let Ok(mut tasks) = serde_json::from_str::<Vec<schemas::automower::CalendarTask>>(val)
            else {
                tracing::error!(%node_id, %val, "work area calendar does not match the expected schema");
                return None;
            };
            if tasks
                .iter()
                .any(|task| task.start > 1439 || !(1..=1440).contains(&task.duration))
            {
                tracing::error!(%node_id, "work area calendar tasks contain invalid time ranges");
                return None;
            }
            // Same representation as `homie_property_value`, so that the target can be compared.
            for task in &mut tasks {
                task.work_area_id = None;
            }
            let expected = serde_json::to_string(&tasks).expect("calendar schema is serializable");
            for task in &mut tasks {
                task.work_area_id = Some(work_area_id);
            }
            return Some(SetCommand {
                expected,
                request: SetRequest::Post {
                    path: format!("workAreas/{work_area_id}/calendar"),
                    body: serde_json::json!({ "data": {
                        "type": "calendar",
                        "attributes": schemas::automower::CalendarWorkArea { tasks }
                    }}),
                },
            });
        }
        let (attributes, expected) = match prop_id {
            "name" => {
                if val.chars().count() > 32 {
                    tracing::error!(%node_id, "work area name exceeds 32 characters");
                    return None;
                }
                (serde_json::json!({ "name": val }), val.to_owned())
            }
            "cutting-height" => {
                let Ok(value) = val.parse::<u8>() else {
                    tracing::error!(%node_id, %val, "work area cutting height is not an integer");
                    return None;
                };
                if value > 100 {
                    tracing::error!(%node_id, %val, "work area cutting height must be in the range 0-100");
                    return None;
                }
                (
                    serde_json::json!({ "cuttingHeight": value }),
                    value.to_string(),
                )
            }
            "enabled" => {
                let Ok(value) = val.parse::<bool>() else {
                    tracing::error!(%node_id, %val, "work area enabled value is not a boolean");
                    return None;
                };
                (serde_json::json!({ "enable": value }), value.to_string())
            }
            "orientation" | "orientation-shift" => {
                let Ok(degrees) = val.parse::<f64>() else {
                    tracing::error!(%node_id, %prop_id, %val, "work area orientation is not a number");
                    return None;
                };
                if !(0.0..=180.0).contains(&degrees) {
                    tracing::error!(%node_id, %prop_id, %val, "work area orientation must be in the range 0-180 degrees");
                    return None;
                }
                let tenths = (degrees * 10.0).round() as u16;
                let key = if prop_id == "orientation" {
                    "orientation"
                } else {
                    "orientationShift"
                };
                (
                    serde_json::json!({ key: tenths }),
                    mower_degrees_tenths(tenths),
                )
            }
            _ => unreachable!("only the settable work area properties are subscribed to"),
        };
        return Some(SetCommand {
            expected,
            request: SetRequest::Patch {
                path: format!("workAreas/{work_area_id}"),
                body: serde_json::json!({ "data": {
                    "id": work_area_id,
                    "type": "workArea",
                    "attributes": attributes
                }}),
            },
        });
    }
    if let Some(zone_id) = parse_stay_out_zone_node_id(node_id) {
        if prop_id != "enabled" {
            unreachable!("only the settable stay-out zone properties are subscribed to");
        }
        let Ok(enable) = val.parse::<bool>() else {
            tracing::error!(%node_id, %val, "stay-out zone enabled value is not a boolean");
            return None;
        };
        return Some(SetCommand {
            expected: enable.to_string(),
            request: SetRequest::Patch {
                path: format!("stayOutZones/{zone_id}"),
                body: serde_json::json!({ "data": {
                    "id": zone_id,
                    "type": "stayOutZone",
                    "attributes": { "enable": enable }
                }}),
            },
        });
    }
    let (path, body, expected) = match (node_id, prop_id) {
        ("settings", "cutting-height") => {
            let Ok(height) = val.parse::<i64>() else {
                tracing::error!(%node_id, %prop_id, %val, "cutting height value is not a number");
                return None;
            };
            let body = serde_json::json!({ "data": {
                "type": "settings",
                "attributes": { "cuttingHeight": height }
            }});
            ("settings", body, height.to_string())
        }
        ("settings", "headlight-mode") => {
            let body = serde_json::json!({ "data": {
                "type": "settings",
                "attributes": { "headlight": { "mode": val } }
            }});
            ("settings", body, val.to_owned())
        }
        ("mower", "mowing" | "parked" | "paused") => {
            let action = match (prop_id, val) {
                ("mowing", "yes") | ("parked", "no") | ("paused", "false") => "ResumeSchedule",
                ("mowing", "no") | ("paused", "true") => "Pause",
                ("parked", "yes") => "ParkUntilFurtherNotice",
                ("parked", "until-next-schedule") => "ParkUntilNextSchedule",
                ("mowing", "forced-for-duration" | "forced-in-workarea")
                | ("parked", "for-duration") => {
                    // TODO: figure out how to pass parameters.
                    tracing::warn!(%node_id, %prop_id, %val, "value not supported yet");
                    return None;
                }
                _ => {
                    tracing::error!(%node_id, %prop_id, %val, "invalid value");
                    return None;
                }
            };
            let body = serde_json::json!({ "data": { "type": action } });
            ("actions", body, val.to_owned())
        }
        ("planner", prop_id) => {
            let inhibit = external_inhibits
                .iter()
                .find(|inhibit| inhibit.name.as_str() == prop_id)
                .expect("only the configured external inhibits are subscribed to");
            let duration = match val {
                "enable" => inhibit.duration_minutes,
                "disable" => 1,
                _ => {
                    tracing::error!(%node_id, %prop_id, %val, "invalid external inhibit command");
                    return None;
                }
            };
            let body = serde_json::json!({ "data": {
                "type": "Park",
                "attributes": {
                    "duration": duration,
                    "externalReason": inhibit.reason
                }
            }});
            ("actions", body, val.to_owned())
        }
        _ => unreachable!("only the settable properties are subscribed to"),
    };
    Some(SetCommand {
        expected,
        request: SetRequest::Post {
            path: path.to_owned(),
            body,
        },
    })
}

impl MowerContext {
    async fn monitor_api_changes(mower: Arc<Mutex<Self>>) -> Result<(), Error> {
        let api = Arc::clone(&mower.lock().await.api);
        let mut changes = api.subscribe();
        loop {
            mower.lock().await.synchronize_from_api().await?;
            if changes.changed().await.is_err() {
                return Ok(());
            }
        }
    }

    async fn synchronize_from_api(&mut self) -> Result<(), Error> {
        let data = self.api.snapshot().await;
        self.update_dynamic_nodes(dynamic_nodes(&data)).await
    }

    async fn send_property_message(
        &self,
        node_id: &HomieID,
        prop_id: &HomieID,
        message: impl FnOnce(oneshot::Sender<Result<(), Error>>) -> PropertyTaskMessage,
    ) -> Result<(), Error> {
        let key = (
            self.protocol.device_ref().device_id().to_string(),
            node_id.to_string(),
            prop_id.to_string(),
        );
        let sender = self.property_inputs.lock().await.get(&key).cloned();
        let Some(sender) = sender else {
            return Err(Error::PropertyTaskStopped(key.0, key.1, key.2));
        };
        let (reply, result) = oneshot::channel();
        sender
            .send(message(reply))
            .map_err(|_| Error::PropertyTaskStopped(key.0.clone(), key.1.clone(), key.2.clone()))?;
        result
            .await
            .map_err(|_| Error::PropertyTaskStopped(key.0, key.1, key.2))?
    }

    async fn publish_all_properties(&self) -> Result<(), Error> {
        let properties = self
            .description
            .iter()
            .map(|(node, _, property, _)| (node.clone(), property.clone()))
            .collect::<Vec<_>>();
        for (node, property) in properties {
            self.send_property_message(&node, &property, PropertyTaskMessage::Publish)
                .await?;
        }
        Ok(())
    }

    async fn erase_property(&self, node_id: &HomieID, prop_id: &HomieID) -> Result<(), Error> {
        self.send_property_message(node_id, prop_id, PropertyTaskMessage::Erase)
            .await?;
        self.property_inputs.lock().await.remove(&(
            self.protocol.device_ref().device_id().to_string(),
            node_id.to_string(),
            prop_id.to_string(),
        ));
        Ok(())
    }

    /// Make the set of the nodes for work areas and stay-out zones match `wanted`.
    ///
    /// Per the Homie convention, a change to the description requires the device to go through
    /// `init` before it becomes `ready` again, so the description gets republished from scratch.
    async fn update_dynamic_nodes(
        &mut self,
        wanted: BTreeMap<HomieID, HomieNodeDescription>,
    ) -> Result<(), Error> {
        let old_nodes = self
            .description
            .nodes
            .iter()
            .filter(|(id, _)| is_polled_node(id.as_str()))
            .collect::<BTreeMap<_, _>>();
        if old_nodes.keys().copied().eq(wanted.keys()) {
            return Ok(());
        }
        let removed = old_nodes
            .iter()
            .filter(|(id, _)| !wanted.contains_key(**id))
            .map(|(id, node)| ((*id).clone(), node.properties.keys().cloned().collect()))
            .collect::<Vec<(HomieID, Vec<HomieID>)>>();
        let old_description = self.description.clone();
        let mut description = old_description.clone();
        description
            .nodes
            .retain(|id, _| !is_polled_node(id.as_str()));
        description.nodes.extend(wanted);
        description.update_version();
        let old_status = self.status;
        if old_status == HomieDeviceStatus::Ready {
            self.status = HomieDeviceStatus::Init;
            self.property_ready.send_replace(false);
        }
        self.description = description;
        let result = self.republish(removed).await;
        if result.is_err() {
            self.description = old_description;
            self.status = old_status;
            self.property_ready
                .send_replace(old_status == HomieDeviceStatus::Ready);
        }
        result
    }

    async fn republish(&mut self, removed: Vec<(HomieID, Vec<HomieID>)>) -> Result<(), Error> {
        self.publish_device_state(self.status).await?;
        for (node_id, prop_ids) in removed {
            for prop_id in prop_ids {
                self.erase_property(&node_id, &prop_id).await?;
            }
        }
        self.ensure_property_tasks().await;
        self.publish_device().await
    }

    fn id_to_key(id: &str, root_protocol: &Homie5DeviceProtocol) -> String {
        let root_id = root_protocol.device_ref().device_id();
        format!("{}-{}", root_id, id)
    }

    async fn new(
        api: Arc<api::Mower>,
        api_client: Arc<api::Client>,
        mqtt: rumqttc::v5::AsyncClient,
        root_protocol: &Homie5DeviceProtocol,
        external_inhibits: Arc<[ExternalInhibit]>,
        property_inputs: PropertyInputs,
    ) -> Self {
        let api_id = api.id().to_owned();
        let data = api.snapshot().await;
        let (property_ready, _) = watch::channel(false);
        let root_id = root_protocol.device_ref().device_id();
        let child_device_id = HomieID::try_from(Self::id_to_key(&api_id, root_protocol)).unwrap();
        let protocol = root_protocol.clone_for_child(child_device_id);
        let initial_state = if data.metadata.connected {
            HomieDeviceStatus::Init
        } else {
            HomieDeviceStatus::Disconnected
        };
        let dynamic_nodes = dynamic_nodes(&data);
        let mut description = DeviceDescriptionBuilder::new()
            .name(data.system.name.clone())
            .root(root_id.clone())
            .parent(root_id.clone());
        let battery_level_prop = PropertyDescriptionBuilder::integer()
            .unit(HOMIE_UNIT_PERCENT)
            .range(0..=100)
            .build();
        let battery_node = NodeDescriptionBuilder::new()
            .add_property(BATTERY_LEVEL_PROP_ID.clone(), battery_level_prop)
            .build();
        let latlon_prop = PropertyDescriptionBuilder::string().build();
        let position_node = NodeDescriptionBuilder::new()
            .add_property(POSITION_LATLON_PROP_ID.clone(), latlon_prop)
            .build();
        let mode_prop = PropertyDescriptionBuilder::enumeration([
            "MAIN_AREA",
            "SECONDARY_AREA",
            "HOME",
            "DEMO",
            "UNKNOWN",
            "POI",
        ])
        .unwrap()
        .build();
        let activity_prop = PropertyDescriptionBuilder::enumeration([
            "UNKNOWN",
            "NOT_APPLICABLE",
            "MOWING",
            "GOING_HOME",
            "CHARGING",
            "LEAVING",
            "PARKED_IN_CS",
            "STOPPED_IN_GARDEN",
        ])
        .unwrap()
        .build();
        let inactive_prop = PropertyDescriptionBuilder::enumeration([
            "NONE",
            "PLANNING",
            "SEARCHING_FOR_SATELLITES",
        ])
        .unwrap()
        .build();
        let state_prop = PropertyDescriptionBuilder::enumeration([
            "UNKNOWN",
            "PAUSED",
            "IN_OPERATION",
            "WAIT_UPDATING",
            "WAIT_POWER_UP",
            "RESTRICTED",
            "OFF",
            "STOPPED",
            "ERROR",
            "FATAL_ERROR",
            "ERROR_AT_POWER_UP",
        ])
        .unwrap()
        .build();
        let error_code_prop = PropertyDescriptionBuilder::integer().range(0..=724).build();
        let work_area_prop = PropertyDescriptionBuilder::integer().build();
        let mowing_prop = PropertyDescriptionBuilder::enumeration([
            "no",
            "yes",
            "forced-for-duration",
            "forced-in-workarea",
            "commuting",
        ])
        .unwrap()
        .settable(true)
        .build();
        let parked_prop = PropertyDescriptionBuilder::enumeration([
            "no",
            "yes",
            "until-next-schedule",
            "for-duration",
        ])
        .unwrap()
        .settable(true)
        .build();
        let paused_prop = PropertyDescriptionBuilder::boolean().settable(true).build();
        let inactive_summary_prop =
            PropertyDescriptionBuilder::enumeration(["no", "planning", "searching-for-satellites"])
                .unwrap()
                .build();
        let mower_node = NodeDescriptionBuilder::new()
            .add_property(MOWER_MODE_PROP_ID.clone(), mode_prop)
            .add_property(MOWER_ACTIVITY_PROP_ID.clone(), activity_prop)
            .add_property(MOWER_INACTIVE_REASON_PROP_ID.clone(), inactive_prop)
            .add_property(MOWER_STATE_PROP_ID.clone(), state_prop)
            .add_property(MOWER_ERROR_CODE_PROP_ID.clone(), error_code_prop)
            .add_property(MOWER_WORK_AREA_ID_PROP_ID.clone(), work_area_prop)
            .add_property(MOWER_MOWING_PROP_ID.clone(), mowing_prop)
            .add_property(MOWER_PARKED_PROP_ID.clone(), parked_prop)
            .add_property(MOWER_PAUSED_PROP_ID.clone(), paused_prop)
            .add_property(MOWER_INACTIVE_PROP_ID.clone(), inactive_summary_prop)
            .build();
        let restricted_reason_prop = PropertyDescriptionBuilder::enumeration([
            "NONE",
            "WEEK_SCHEDULE",
            "PARK_OVERRIDE",
            "SENSOR",
            "DAILY_LIMIT",
            "FOTA",
            "FROST",
            "ALL_WORK_AREAS_COMPLETED",
            "EXTERNAL",
            "WORK_AREA_ABANDONED",
        ])
        .unwrap()
        .build();
        let override_prop =
            PropertyDescriptionBuilder::enumeration(["NOT_ACTIVE", "FORCE_PARK", "FORCE_MOW"])
                .unwrap()
                .build();
        let next_start_prop = PropertyDescriptionBuilder::datetime().build();
        let external_reason_prop = PropertyDescriptionBuilder::integer()
            .range(1000..=299_999)
            .build();
        let mut planner_node = NodeDescriptionBuilder::new()
            .add_property(PLANNER_OVERRIDE_PROP_ID.clone(), override_prop)
            .add_property(PLANNER_NEXT_START_PROP_ID.clone(), next_start_prop)
            .add_property(
                PLANNER_EXTERNAL_REASON_PROP_ID.clone(),
                external_reason_prop,
            )
            .add_property(
                PLANNER_RESTRICTED_REASON_PROP_ID.clone(),
                restricted_reason_prop,
            );
        for inhibit in external_inhibits.iter() {
            let prop = PropertyDescriptionBuilder::enumeration(["enable", "disable"])
                .unwrap()
                .settable(true)
                .retained(false)
                .build();
            planner_node = planner_node.add_property(inhibit.name.clone(), prop);
        }
        let planner_node = planner_node.build();

        let cutting_height_prop = PropertyDescriptionBuilder::integer()
            .unit(HOMIE_UNIT_PERCENT)
            .range(0..=100)
            .settable(true)
            .build();
        let headlight_mode_prop = PropertyDescriptionBuilder::enumeration([
            "ALWAYS_ON",
            "ALWAYS_OFF",
            "EVENING_ONLY",
            "EVENING_AND_NIGHT",
        ])
        .unwrap()
        .settable(true)
        .build();
        let settings_node = NodeDescriptionBuilder::new()
            .add_property(SETTINGS_HEADLIGHT_PROP_ID.clone(), headlight_mode_prop)
            .add_property(SETTINGS_CUTTING_HEIGHT_PROP_ID.clone(), cutting_height_prop)
            .build();
        description = description.add_node(BATTERY_NODE_ID.clone(), battery_node);
        description = description.add_node(POSITION_NODE_ID.clone(), position_node);
        for (id, node) in dynamic_nodes {
            description = description.add_node(id, node);
        }
        description = description.add_node(MOWER_NODE_ID.clone(), mower_node);
        description = description.add_node(PLANNER_NODE_ID.clone(), planner_node);
        description = description.add_node(SETTINGS_NODE_ID.clone(), settings_node);
        let description = description.build();
        Self {
            api,
            api_client,
            external_inhibits,
            mqtt,
            protocol,
            description,
            property_inputs,
            property_ready,
            status: initial_state,
        }
    }

    async fn ensure_property_tasks(&self) {
        let device_id = self.protocol.device_ref().device_id().to_string();
        let mut property_inputs = self.property_inputs.lock().await;
        for (node_id, _, prop_id, prop) in self.description.iter() {
            let key = (device_id.clone(), node_id.to_string(), prop_id.to_string());
            if property_inputs
                .get(&key)
                .is_some_and(|sender| !sender.is_closed())
            {
                continue;
            }
            let (sender, inputs) = mpsc::unbounded_channel();
            property_inputs.insert(key, sender);
            tokio::spawn(
                PropertyTask {
                    api: Arc::clone(&self.api),
                    api_client: Arc::clone(&self.api_client),
                    external_inhibits: Arc::clone(&self.external_inhibits),
                    mqtt: self.mqtt.clone(),
                    protocol: self.protocol.clone(),
                    node_id: node_id.clone(),
                    prop_id: prop_id.clone(),
                    settable: prop.settable,
                    retained: prop.retained,
                    inputs,
                    api_changes: self.api.subscribe(),
                    ready: self.property_ready.subscribe(),
                    last_value: None,
                    last_target: None,
                    pending_target: None,
                }
                .run(),
            );
        }
    }

    async fn publish_device(&mut self) -> Result<(), Error> {
        let initial_state = self.status;
        for step in homie5::homie_device_publish_steps() {
            match step {
                homie5::DevicePublishStep::DeviceStateInit => {
                    let publish = self.protocol.publish_state(initial_state);
                    self.mqtt
                        .homie_publish(publish)
                        .await
                        .map_err(Error::PublishInitState)?;
                }
                homie5::DevicePublishStep::DeviceDescription => {
                    let publish = self
                        .protocol
                        .publish_description(&self.description)
                        .map_err(Error::GenerateDescription)?;
                    self.mqtt
                        .homie_publish(publish)
                        .await
                        .map_err(Error::PublishDescription)?;
                }
                homie5::DevicePublishStep::PropertyValues => {
                    if initial_state == HomieDeviceStatus::Disconnected {
                        continue;
                    }
                    self.publish_all_properties().await?;
                }
                homie5::DevicePublishStep::SubscribeProperties => {
                    if initial_state == HomieDeviceStatus::Disconnected {
                        continue;
                    }
                    let subscriptions = self
                        .protocol
                        .subscribe_props(&self.description)
                        .map_err(Error::GenerateSubscribtions)?;
                    self.mqtt
                        .homie_subscribe(subscriptions)
                        .await
                        .map_err(Error::Subscribe)?;
                }
                homie5::DevicePublishStep::DeviceStateReady => {
                    if initial_state == HomieDeviceStatus::Disconnected {
                        continue;
                    }
                    let publish = self.protocol.publish_state(HomieDeviceStatus::Ready);
                    self.mqtt
                        .homie_publish(publish)
                        .await
                        .map_err(Error::PublishReadyState)?;
                    self.status = HomieDeviceStatus::Ready;
                    self.property_ready.send_replace(true);
                }
            }
        }
        Ok(())
    }

    async fn publish_device_state(&self, status: HomieDeviceStatus) -> Result<(), Error> {
        let publish = self.protocol.publish_state(status);
        self.mqtt
            .homie_publish(publish)
            .await
            .map_err(Error::PublishInitState)
    }
}

fn homie_property_value(
    data: &schemas::automower::MowerData,
    node_id: &str,
    prop_id: &str,
    external_inhibits: &[ExternalInhibit],
) -> Option<String> {
    if let Some(zone_id) = parse_stay_out_zone_node_id(node_id) {
        let zones = data.stay_out_zones.as_ref()?;
        let zone = zones.zones.as_ref()?.iter().find(|zone| {
            zone.id.as_ref().map(|id| id.to_lowercase()).as_deref() == Some(zone_id)
        })?;
        return match prop_id {
            "id" => Some(zone_id.to_owned()),
            "name" => zone.name.clone(),
            "enabled" => zone.enabled.map(|value| value.to_string()),
            "dirty" => zones.dirty.map(|value| value.to_string()),
            _ => None,
        };
    }
    if let Some(id) = parse_work_area_node_id(node_id) {
        if prop_id == "id" {
            return Some(id.to_string());
        }
        let area = data
            .work_areas
            .as_ref()?
            .iter()
            .find(|area| area.work_area_id == Some(id))?;
        return match prop_id {
            "name" => area.name.clone(),
            "type" => area.r#type.clone(),
            "cutting-height" => area.cutting_height.map(|value| value.to_string()),
            "enabled" => area.enabled.map(|value| value.to_string()),
            "schedulable" => area.schedulable.map(|value| value.to_string()),
            "orientation" => area.orientation.map(mower_degrees_tenths),
            "orientation-shift" => area.orientation_shift.map(mower_degrees_tenths),
            "current-orientation" => area.current_orientation.map(mower_degrees_tenths),
            "overlap" => area.overlap.map(|value| value.to_string()),
            "progress" => area.progress.map(|value| value.to_string()),
            "last-time-completed" => area
                .last_time_completed
                .map(|value| mower_datetime_seconds(value).to_string()),
            "last-time-abandoned" => area
                .last_time_abandoned
                .map(|value| mower_datetime_seconds(value).to_string()),
            "use-global-cutting-height" => area
                .use_global_cutting_height
                .map(|value| value.to_string()),
            "calendar" => area.calendar.as_ref().map(|calendar| {
                let mut tasks = calendar.tasks.clone();
                for task in &mut tasks {
                    task.work_area_id = None;
                }
                serde_json::to_string(&tasks).expect("calendar schema is serializable")
            }),
            _ => None,
        };
    }

    match (node_id, prop_id) {
        ("battery", "level") => Some(data.battery.battery_percent.to_string()),
        ("position", "latitude-longitude") => {
            let position = data.positions.as_ref()?.last()?;
            Some(format!("{},{}", position.latitude?, position.longitude?))
        }
        ("mower", "mode") => Some(data.mower.mode.clone()),
        ("mower", "activity") => Some(data.mower.activity.clone()),
        ("mower", "inactive-reason") => Some(
            data.mower
                .inactive_reason
                .clone()
                .unwrap_or_else(|| "NONE".to_owned()),
        ),
        ("mower", "state") => Some(data.mower.state.clone()),
        ("mower", "error-code") => data.mower.error_code.map(|value| value.to_string()),
        ("mower", "work-area") => data.mower.work_area_id.map(|value| value.to_string()),
        ("mower", "mowing") => Some(derive_mowing_api(data).to_owned()),
        ("mower", "parked") => Some(derive_parked_api(data).to_owned()),
        ("mower", "paused") => Some((data.mower.state == "PAUSED").to_string()),
        ("mower", "inactive") => Some(derive_inactive_api(data).to_owned()),
        ("planner", "next-start") => Some(if data.planner.next_start_timestamp == 0 {
            jiff::Timestamp::now().to_string()
        } else {
            mower_datetime(data.planner.next_start_timestamp).to_string()
        }),
        ("planner", "override-action") => data
            .planner
            .r#override
            .as_ref()
            .map(|value| value.action.clone()),
        ("planner", "restricted-reason") => Some(data.planner.restricted_reason.clone()),
        ("planner", "external-reason") => {
            data.planner.external_reason.map(|value| value.to_string())
        }
        ("planner", prop_id) if prop_id.starts_with("external-inhibit-") => {
            let inhibit = external_inhibits
                .iter()
                .find(|inhibit| inhibit.name.as_str() == prop_id)?;
            Some(if data.planner.external_reason == Some(inhibit.reason) {
                "enable".to_owned()
            } else {
                "disable".to_owned()
            })
        }
        ("settings", "cutting-height") => data
            .settings
            .as_ref()?
            .cutting_height
            .map(|value| value.to_string()),
        ("settings", "headlight-mode") => data
            .settings
            .as_ref()?
            .headlight
            .as_ref()
            .map(|value| value.mode.clone()),
        _ => None,
    }
}

fn derive_mowing_api(data: &schemas::automower::MowerData) -> &'static str {
    match data.mower.activity.as_str() {
        "GOING_HOME" | "LEAVING" => "commuting",
        "MOWING"
            if data
                .planner
                .r#override
                .as_ref()
                .map(|value| value.action.as_str())
                == Some("FORCE_MOW") =>
        {
            if data.mower.work_area_id.unwrap_or(0) == 0 {
                "forced-for-duration"
            } else {
                "forced-in-workarea"
            }
        }
        "MOWING" => "yes",
        _ => "no",
    }
}

fn derive_parked_api(data: &schemas::automower::MowerData) -> &'static str {
    if matches!(data.mower.activity.as_str(), "CHARGING" | "PARKED_IN_CS") {
        if data
            .planner
            .r#override
            .as_ref()
            .map(|value| value.action.as_str())
            == Some("FORCE_PARK")
        {
            "for-duration"
        } else {
            "yes"
        }
    } else {
        "no"
    }
}

fn derive_inactive_api(data: &schemas::automower::MowerData) -> &'static str {
    match data.mower.inactive_reason.as_deref() {
        Some("PLANNING") => "planning",
        Some("SEARCHING_FOR_SATELLITES") => "searching-for-satellites",
        _ => "no",
    }
}

fn work_area_node_id(id: i64) -> HomieID {
    HomieID::try_from(format!("work-area-{id}")).expect("work area IDs are valid Homie IDs")
}

fn parse_work_area_node_id(node_id: &str) -> Option<i64> {
    node_id
        .strip_prefix("work-area-")?
        .parse::<i64>()
        .ok()
        .filter(|id| *id >= 0)
}

const STAY_OUT_ZONE_NODE_PREFIX: &str = "stay-out-zone-";

fn stay_out_zone_node_id(zone_id: &str) -> Option<HomieID> {
    let node_id = format!("{STAY_OUT_ZONE_NODE_PREFIX}{}", zone_id.to_lowercase());
    parse_stay_out_zone_node_id(&node_id)?;
    HomieID::try_from(node_id).ok()
}

/// The (lowercase) ID of the stay-out zone that the node is for.
fn parse_stay_out_zone_node_id(node_id: &str) -> Option<&str> {
    let zone_id = node_id.strip_prefix(STAY_OUT_ZONE_NODE_PREFIX)?;
    let is_uuid = zone_id.len() == 36
        && zone_id
            .chars()
            .all(|c| c == '-' || c.is_ascii_digit() || ('a'..='f').contains(&c));
    is_uuid.then_some(zone_id)
}

/// Whether the node is for something that the API has to be polled for: a work area or a
/// stay-out zone.
fn is_polled_node(node_id: &str) -> bool {
    parse_work_area_node_id(node_id).is_some() || parse_stay_out_zone_node_id(node_id).is_some()
}

/// The nodes that come and go with the work areas and the stay-out zones of the mower.
fn dynamic_nodes(data: &schemas::automower::MowerData) -> BTreeMap<HomieID, HomieNodeDescription> {
    let work_areas = data
        .work_areas
        .iter()
        .flatten()
        .filter_map(|area| area.work_area_id)
        .map(|id| (work_area_node_id(id), work_area_node_description()));
    let stay_out_zones = data
        .stay_out_zones
        .iter()
        .filter_map(|zones| zones.zones.as_ref())
        .flatten()
        .filter_map(|zone| stay_out_zone_node_id(zone.id.as_deref()?))
        .map(|id| (id, stay_out_zone_node_description()));
    work_areas.chain(stay_out_zones).collect()
}

fn stay_out_zone_node_description() -> HomieNodeDescription {
    NodeDescriptionBuilder::new()
        .add_property(
            STAY_OUT_ZONE_ID_PROP_ID.clone(),
            PropertyDescriptionBuilder::string().build(),
        )
        .add_property(
            STAY_OUT_ZONE_NAME_PROP_ID.clone(),
            PropertyDescriptionBuilder::string().build(),
        )
        .add_property(
            STAY_OUT_ZONE_ENABLED_PROP_ID.clone(),
            PropertyDescriptionBuilder::boolean().settable(true).build(),
        )
        // While the zones are not synchronized with the cloud, they cannot be enabled or disabled.
        .add_property(
            STAY_OUT_ZONE_DIRTY_PROP_ID.clone(),
            PropertyDescriptionBuilder::boolean().build(),
        )
        .build()
}

fn work_area_node_description() -> HomieNodeDescription {
    let integer = PropertyDescriptionBuilder::integer().build();
    let percentage = PropertyDescriptionBuilder::integer()
        .unit(HOMIE_UNIT_PERCENT)
        .range(0..=100)
        .settable(true)
        .build();
    let boolean = PropertyDescriptionBuilder::boolean().build();
    let settable_boolean = PropertyDescriptionBuilder::boolean().settable(true).build();
    let string = PropertyDescriptionBuilder::string().settable(true).build();
    let orientation = PropertyDescriptionBuilder::float()
        .unit(HOMIE_UNIT_DEGREE)
        .range(0.0..=180.0)
        .settable(true)
        .build();
    let current_orientation = PropertyDescriptionBuilder::float()
        .unit(HOMIE_UNIT_DEGREE)
        .range(0.0..=180.0)
        .build();
    let work_area_type =
        PropertyDescriptionBuilder::enumeration(["BOUNDARY", "RANDOM", "SYSTEMATIC"])
            .expect("work area type enum is valid")
            .build();
    let json = PropertyDescriptionBuilder::json().settable(true).build();
    NodeDescriptionBuilder::new()
        .add_property(WORK_AREA_ID_PROP_ID.clone(), integer.clone())
        .add_property(WORK_AREA_NAME_PROP_ID.clone(), string.clone())
        .add_property(WORK_AREA_TYPE_PROP_ID.clone(), work_area_type)
        .add_property(WORK_AREA_CUTTING_HEIGHT_PROP_ID.clone(), percentage.clone())
        .add_property(WORK_AREA_ENABLED_PROP_ID.clone(), settable_boolean)
        .add_property(WORK_AREA_SCHEDULABLE_PROP_ID.clone(), boolean.clone())
        .add_property(WORK_AREA_ORIENTATION_PROP_ID.clone(), orientation.clone())
        .add_property(WORK_AREA_ORIENTATION_SHIFT_PROP_ID.clone(), orientation)
        .add_property(
            WORK_AREA_CURRENT_ORIENTATION_PROP_ID.clone(),
            current_orientation,
        )
        .add_property(WORK_AREA_OVERLAP_PROP_ID.clone(), integer.clone())
        .add_property(WORK_AREA_PROGRESS_PROP_ID.clone(), percentage)
        .add_property(
            WORK_AREA_LAST_TIME_COMPLETED_PROP_ID.clone(),
            PropertyDescriptionBuilder::datetime().build(),
        )
        .add_property(
            WORK_AREA_LAST_TIME_ABANDONED_PROP_ID.clone(),
            PropertyDescriptionBuilder::datetime().build(),
        )
        .add_property(WORK_AREA_USE_GLOBAL_CUTTING_HEIGHT_PROP_ID.clone(), boolean)
        .add_property(WORK_AREA_CALENDAR_PROP_ID.clone(), json)
        .build()
}

trait MqttClientExt {
    type PublishError;
    type SubscribeError;
    async fn homie_publish(&self, p: HomiePublish) -> Result<(), Self::PublishError>;
    async fn homie_subscribe(
        &self,
        subs: impl Iterator<Item = Subscription> + Send,
    ) -> Result<(), Self::SubscribeError>;
}

impl MqttClientExt for rumqttc::v5::AsyncClient {
    type PublishError = rumqttc::v5::ClientError;
    type SubscribeError = rumqttc::v5::ClientError;
    async fn homie_publish(&self, p: HomiePublish) -> Result<(), Self::PublishError> {
        self.publish(p.topic, convert_qos(p.qos), p.retain, p.payload)
            .await
    }

    async fn homie_subscribe(
        &self,
        subs: impl Iterator<Item = Subscription> + Send,
    ) -> Result<(), Self::SubscribeError> {
        let subs = subs
            .map(|sub| rumqttc::v5::mqttbytes::v5::Filter::new(sub.topic, convert_qos(sub.qos)))
            .collect::<Vec<_>>();
        if subs.is_empty() {
            return Ok(());
        }
        self.subscribe_many(subs).await
    }
}

fn convert_qos(homie: homie5::client::QoS) -> rumqttc::v5::mqttbytes::QoS {
    use homie5::client::QoS::*;
    match homie {
        AtMostOnce => rumqttc::v5::mqttbytes::QoS::AtMostOnce,
        AtLeastOnce => rumqttc::v5::mqttbytes::QoS::AtLeastOnce,
        ExactlyOnce => rumqttc::v5::mqttbytes::QoS::ExactlyOnce,
    }
}

fn mower_datetime(since_mower_epoch: u64) -> jiff::civil::DateTime {
    const EPOCH: jiff::civil::DateTime = jiff::civil::DateTime::constant(1970, 1, 1, 0, 0, 0, 0);
    let duration = std::time::Duration::from_millis(since_mower_epoch);
    EPOCH
        .checked_add(duration)
        .expect("adding milliseconds to epoch should never overflow?")
}

fn mower_datetime_seconds(since_mower_epoch: i64) -> jiff::civil::DateTime {
    const EPOCH: jiff::civil::DateTime = jiff::civil::DateTime::constant(1970, 1, 1, 0, 0, 0, 0);
    EPOCH
        .checked_add(jiff::Span::new().seconds(since_mower_epoch))
        .expect("adding seconds to epoch should never overflow")
}

fn mower_degrees_tenths(value: u16) -> String {
    format!("{:.1}", f64::from(value) / 10.0)
}

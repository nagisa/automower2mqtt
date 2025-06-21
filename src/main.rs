use clap::Parser as _;
use futures::{SinkExt as _, StreamExt as _};
use homie5::HOMIE_UNIT_PERCENT;
use homie5::HomieDataType;
use homie5::client::Publish as HomiePublish;
use homie5::client::Subscription;
use homie5::device_description::DeviceDescriptionBuilder;
use homie5::device_description::HomieDeviceDescription;
use homie5::device_description::NodeDescriptionBuilder;
use homie5::device_description::PropertyDescriptionBuilder;
use homie5::{Homie5DeviceProtocol, HomieDeviceStatus, HomieID};
use rumqttc::v5::MqttOptions;
use rumqttc::v5::mqttbytes::v5::LastWill;
use schemas::automower::MowerData;
use std::collections::BTreeMap;
use std::collections::btree_map;
use std::error::Error as _;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::Bytes;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tracing::debug;
use tracing::warn;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

mod schemas;

static AUTOMOWER_CONNECT_API_BASE: &str = "https://api.amc.husqvarna.dev/v1";
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
static MOWER_WORK_AREA_ID_PROP_ID: HomieID = HomieID::new_const("work-area");
static PLANNER_NODE_ID: HomieID = HomieID::new_const("planner");
static PLANNER_NEXT_START_PROP_ID: HomieID = HomieID::new_const("next-start");
static PLANNER_OVERRIDE_PROP_ID: HomieID = HomieID::new_const("override-action");
static PLANNER_RESTRICTED_REASON_PROP_ID: HomieID = HomieID::new_const("restricted-reason");
static PLANNER_EXTERNAL_REASON_PROP_ID: HomieID = HomieID::new_const("external-reason");
static SETTINGS_NODE_ID: HomieID = HomieID::new_const("settings");
static SETTINGS_CUTTING_HEIGHT_PROP_ID: HomieID = HomieID::new_const("cutting-height");
static SETTINGS_HEADLIGHT_PROP_ID: HomieID = HomieID::new_const("headlight-mode");

const USER_AGENT: &str = concat!(
    env!("CARGO_PKG_NAME"),
    "/",
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("CARGO_PKG_REPOSITORY"),
    ")"
);
const CONNECT_TIMEOUT: Duration = Duration::new(3, 0);
const API_REQUEST_TIMEOUT: Duration = Duration::new(10, 0);
const TOKEN_EXPIRATION_HEADROOM: Duration = Duration::new(30, 0);

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
    #[error("could not make a request to the `{1}` API endpoint")]
    GetApi(#[source] reqwest::Error, &'static str),
    #[error("could not read the API response for `{1}`")]
    ReadResponse(#[source] reqwest::Error, &'static str),
    #[error("could not publish value to `{0}/{1}`")]
    PublishValue(
        #[source] rumqttc::v5::ClientError,
        &'static HomieID,
        &'static HomieID,
    ),
    #[error("an error in websocket")]
    Websocket(#[source] tungstenite::Error),
    #[error("could not join the monitor thread")]
    MonitorJoin(#[source] tokio::task::JoinError),
    #[error("disconnected from the MQTT server")]
    MqttDisconnect,
}

fn main() {
    let args = Args::parse();
    std::process::exit(match setup_and_run(args) {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            let mut cause = e.source();
            while let Some(e) = cause {
                eprintln!("  because: {e}");
                cause = e.source();
            }
            1
        }
    });
}

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
    let state = State {
        token: Default::default(),
        token_expiration: boot_time::Instant::now(),
        mowers: Default::default(),
    };
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
        client,
        state: Mutex::new(state),
        protocol,
        mqtt,
        app_key: args.app_key,
        app_secret: args.app_secret,
    });
    runtime.block_on(context.run(mqtt_loop))
}

struct MowerState {
    homie_status: HomieDeviceStatus,
    battery_percent: u8,
    latitude: Option<f64>,
    longitude: Option<f64>,
    mower_mode: String,
    mower_activity: String,
    mower_inactive_reason: Option<String>,
    mower_state: String,
    work_area_id: Option<i64>,
    error_code: Option<u16>,
    error_code_timestamp: Option<u64>,
    is_error_confirmable: Option<bool>,
    planner_external_reason: Option<i32>,
    planner_restricted_reason: String,
    planner_next_start: u64,
    planner_override: Option<String>,
    cutting_height: Option<u8>,
    headlight_mode: Option<String>,
}

struct MowerContext {
    _client: reqwest::Client,
    mqtt: rumqttc::v5::AsyncClient,
    protocol: Homie5DeviceProtocol,
    description: HomieDeviceDescription,
    _root_id: HomieID,
    api_id: String,
    data: Arc<Mutex<MowerState>>,
}

struct State {
    token: Arc<str>,
    token_expiration: boot_time::Instant,
    mowers: BTreeMap<String, MowerContext>,
}

struct Context {
    client: reqwest::Client,
    state: Mutex<State>,
    protocol: Homie5DeviceProtocol,
    mqtt: rumqttc::v5::AsyncClient,
    app_key: String,
    app_secret: String,
}

impl Context {
    async fn get_token(&self) -> Result<Arc<str>, Error> {
        let now = boot_time::Instant::now();
        {
            let state = self.state.lock().await;
            if state.token_expiration > now {
                return Ok(Arc::clone(&state.token));
            }
        }
        let request = self
            .client
            .post("https://api.authentication.husqvarnagroup.dev/v1/oauth2/token")
            .header("accept", "application/json")
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", &self.app_key),
                ("client_secret", &self.app_secret),
            ])
            .build()
            .expect("TODO");
        let response = self
            .client
            .execute(request)
            .await
            .expect("TODO")
            .json::<schemas::oauth::TokenResponse>()
            .await
            .expect("TODO");

        let mut state = self.state.lock().await;
        state.token = response.access_token.into();
        let token_duration =
            Duration::from_secs_f32(response.expires_in).saturating_sub(TOKEN_EXPIRATION_HEADROOM);
        state.token_expiration = boot_time::Instant::now()
            .checked_add(token_duration)
            .expect("TODO");
        return Ok(Arc::clone(&state.token));
    }

    async fn update_mower_data(&self) -> Result<Vec<MowerContext>, Error> {
        let token = self.get_token().await?;
        let response = self
            .client
            .get(format!("{AUTOMOWER_CONNECT_API_BASE}/mowers"))
            .header("accept", "application/vnd.api+json")
            .header("x-api-key", &self.app_key)
            .header("authorization", format!("Bearer {token}"))
            .header("authorization-provider", "husqvarna")
            .send()
            .await
            .map_err(|e| Error::GetApi(e, "GET /mowers"))?;
        let response = response
            .json::<schemas::automower::JsonApiDataListDocument>()
            .await
            .map_err(|e| Error::ReadResponse(e, "GET /mowers"))?;
        tracing::debug!(?response, uri = "/mowers", method = "GET");
        let mut result = Vec::with_capacity(response.data.len());
        for datum in response.data {
            if "mower" != datum.r#type {
                continue;
            }
            result.push(
                MowerContext::new(
                    datum.id,
                    datum.attributes,
                    self.client.clone(),
                    self.mqtt.clone(),
                    &self.protocol,
                )
                .await,
            );
        }
        Ok(result)
    }

    async fn monitor(self: Arc<Self>) -> Error {
        'reconnect: loop {
            let token = match self.get_token().await {
                Ok(t) => t,
                Err(e) => break 'reconnect e,
            };
            let mut request = format!("wss://ws.openapi.husqvarna.dev/v1")
                .into_client_request()
                .expect("TODO");
            let headers = request.headers_mut();
            headers.insert("x-api-key", self.app_key.parse().expect("TODO"));
            headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
            headers.insert("authorization-provider", "husqvarna".parse().unwrap());
            let (mut stream, _response) = tokio_tungstenite::connect_async(request).await.unwrap();
            debug!(response = ?_response, "connected");
            let mut ping_timer = tokio::time::interval(Duration::from_secs(10));
            loop {
                tokio::select! {
                    result = stream.next() => {
                        let Some(result) = result else {
                            continue 'reconnect;
                        };
                        debug!(websocket_message=?result, "received a websocket message");
                        let message: Result<schemas::websocket::Event ,_> = match result {
                            Ok(tungstenite::Message::Text(text)) => {
                                serde_json::from_str(&text)
                            },
                            Ok(tungstenite::Message::Binary(bytes)) => {
                                serde_json::from_slice(&bytes)
                            },
                            Ok(tungstenite::Message::Ping(payload)) => {
                                stream.feed(
                                    tungstenite::Message::Pong(payload)
                                ).await.expect("TODO");
                                continue;
                            },
                            Ok(tungstenite::Message::Pong(_)) => {
                                debug!("got a pong!");
                                continue;
                            },
                            Ok(tungstenite::Message::Close(_)) => continue 'reconnect,
                            Err(tungstenite::Error::Protocol(_)) => continue 'reconnect,
                            Ok(tungstenite::Message::Frame(_)) => unreachable!(),
                            Err(err) => break 'reconnect Error::Websocket(err),
                        };
                        let event = match message {
                            Ok(event) => event,
                            Err(e) => {
                                warn!(
                                    error = &e as &dyn std::error::Error,
                                    "could not deserialize a websocket message"
                                );
                                continue;
                            }
                        };
                        let guard = self.state.lock().await;
                        let id = match &event {
                            schemas::websocket::Event::BatteryEventV2 { id, .. } |
                            schemas::websocket::Event::CalendarEventV2 { id, .. } |
                            schemas::websocket::Event::CuttingHeightEventV2 { id, .. } |
                            schemas::websocket::Event::HeadlightsEventV2 { id, .. } |
                            schemas::websocket::Event::MessageEventV2 { id, .. } |
                            schemas::websocket::Event::MowerEventV2 { id, .. } |
                            schemas::websocket::Event::PlannerEventV2 { id, .. } |
                            schemas::websocket::Event::PositionEventV2 { id, .. } => id,
                        };
                        let key = MowerContext::id_to_key(id, &self.protocol);
                        let Some(mower) = guard.mowers.get(&key) else {
                            warn!(id, "received event for mower we don't know about, consider restarting?");
                            continue;
                        };
                        if let Err(e) = mower.record_event(event).await {
                            break 'reconnect e;
                        }
                    },
                    _ = ping_timer.tick() => {
                        debug!("sending ping");
                        stream.feed(
                            tungstenite::Message::Ping(
                                Bytes::from_static(b"would be very nice of you to pong, thank you very much")
                            )
                        ).await.expect("TODO");
                    },
                }
            }
        }
    }

    async fn publish_root_device(&self) -> Result<(), Error> {
        let mut root_description = DeviceDescriptionBuilder::new().name("Husqvarna Automowers");
        {
            let mower_data = self.update_mower_data().await?;
            let mut state = self.state.lock().await;
            for mower in mower_data {
                let mower_id = mower.protocol.device_ref().device_id();
                root_description = root_description.add_child(mower_id.clone());
                let entry = state.mowers.entry(mower_id.to_string());
                match entry {
                    btree_map::Entry::Vacant(vacant) => vacant.insert(mower),
                    btree_map::Entry::Occupied(_) => {
                        todo!()
                    }
                };
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
        let mut handle = None;
        loop {
            use rumqttc::Outgoing;
            use rumqttc::v5::Event;
            use rumqttc::v5::mqttbytes::v5::Packet;

            let result = tokio::select! {
                r = mqtt_loop.poll() => r,
                join_result = async { handle.as_mut().unwrap().await }, if handle.is_some() => {
                    match join_result {
                        Ok(error) => return Err(error),
                        Err(join_error) => return Err(Error::MonitorJoin(join_error)),
                    }
                }
            };
            match result.map_err(Error::MqttConnection)? {
                Event::Incoming(Packet::ConnAck(_)) => {
                    tracing::debug!("connected to mqtt");
                    let this = Arc::clone(&self);
                    let joiner = tokio::spawn(async move {
                        if let Err(e) = this.publish_root_device().await {
                            return e;
                        }
                        {
                            let state = this.state.lock().await;
                            for mower in state.mowers.values() {
                                if let Err(e) = mower.publish_device().await {
                                    return e;
                                };
                            }
                        }
                        this.monitor().await
                    });
                    handle = Some(joiner);
                }
                Event::Incoming(Packet::Publish(_publish)) => {
                    todo!("incoming publish");
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
}

impl MowerContext {
    fn id_to_key(id: &str, root_protocol: &Homie5DeviceProtocol) -> String {
        let root_id = root_protocol.device_ref().device_id();
        format!("{}-{}", root_id, id)
    }

    async fn new(
        api_id: String,
        data: MowerData,
        client: reqwest::Client,
        mqtt: rumqttc::v5::AsyncClient,
        root_protocol: &Homie5DeviceProtocol,
    ) -> Self {
        let root_id = root_protocol.device_ref().device_id();
        let child_device_id = HomieID::try_from(Self::id_to_key(&api_id, root_protocol)).unwrap();
        let protocol = root_protocol.clone_for_child(child_device_id);
        let initial_state = if data.metadata.connected {
            HomieDeviceStatus::Init
        } else {
            HomieDeviceStatus::Disconnected
        };
        let mut description = DeviceDescriptionBuilder::new()
            .name(data.system.name.clone())
            .root(root_id.clone())
            .parent(root_id.clone());
        let battery_level_prop = PropertyDescriptionBuilder::new(HomieDataType::Integer)
            .unit(HOMIE_UNIT_PERCENT)
            .format(0..=100)
            .build();
        let battery_node = NodeDescriptionBuilder::new()
            .add_property(BATTERY_LEVEL_PROP_ID.clone(), battery_level_prop)
            .build();
        let latlon_prop = PropertyDescriptionBuilder::new(HomieDataType::String).build();
        let position_node = NodeDescriptionBuilder::new()
            .add_property(POSITION_LATLON_PROP_ID.clone(), latlon_prop)
            .build();
        // FIXME: specify the format for enums...
        let mode_prop = PropertyDescriptionBuilder::new(HomieDataType::Enum).build();
        let activity_prop = PropertyDescriptionBuilder::new(HomieDataType::Enum).build();
        let inactive_prop = PropertyDescriptionBuilder::new(HomieDataType::Enum).build();
        let state_prop = PropertyDescriptionBuilder::new(HomieDataType::Enum).build();
        let error_code_prop = PropertyDescriptionBuilder::new(HomieDataType::Integer)
            .format(0..=724)
            .build();
        let work_area_prop = PropertyDescriptionBuilder::new(HomieDataType::Integer).build();
        let mower_node = NodeDescriptionBuilder::new()
            .add_property(MOWER_MODE_PROP_ID.clone(), mode_prop)
            .add_property(MOWER_ACTIVITY_PROP_ID.clone(), activity_prop)
            .add_property(MOWER_INACTIVE_REASON_PROP_ID.clone(), inactive_prop)
            .add_property(MOWER_STATE_PROP_ID.clone(), state_prop)
            .add_property(MOWER_ERROR_CODE_PROP_ID.clone(), error_code_prop)
            .add_property(MOWER_WORK_AREA_ID_PROP_ID.clone(), work_area_prop)
            .build();
        let restricted_reason_prop = PropertyDescriptionBuilder::new(HomieDataType::Enum).build();
        let override_prop = PropertyDescriptionBuilder::new(HomieDataType::Enum).build();
        let next_start_prop = PropertyDescriptionBuilder::new(HomieDataType::Datetime).build();
        let external_reason_prop = PropertyDescriptionBuilder::new(HomieDataType::Integer)
            .format(1000..=299_999)
            .build();
        let planner_node = NodeDescriptionBuilder::new()
            .add_property(PLANNER_OVERRIDE_PROP_ID.clone(), override_prop)
            .add_property(PLANNER_NEXT_START_PROP_ID.clone(), next_start_prop)
            .add_property(
                PLANNER_EXTERNAL_REASON_PROP_ID.clone(),
                external_reason_prop,
            )
            .add_property(
                PLANNER_RESTRICTED_REASON_PROP_ID.clone(),
                restricted_reason_prop,
            )
            .build();
        let cutting_height_prop = PropertyDescriptionBuilder::new(HomieDataType::Integer)
            .format(0..=9)
            .build();
        let headlight_mode_prop = PropertyDescriptionBuilder::new(HomieDataType::Enum).build();
        let settings_node = NodeDescriptionBuilder::new()
            .add_property(SETTINGS_HEADLIGHT_PROP_ID.clone(), headlight_mode_prop)
            .add_property(SETTINGS_CUTTING_HEIGHT_PROP_ID.clone(), cutting_height_prop)
            .build();
        description = description.add_node(BATTERY_NODE_ID.clone(), battery_node);
        description = description.add_node(POSITION_NODE_ID.clone(), position_node);
        description = description.add_node(MOWER_NODE_ID.clone(), mower_node);
        description = description.add_node(PLANNER_NODE_ID.clone(), planner_node);
        description = description.add_node(SETTINGS_NODE_ID.clone(), settings_node);
        let description = description.build();
        Self {
            _client: client,
            mqtt,
            protocol,
            description,
            _root_id: root_id.clone(),
            api_id,
            data: Arc::new(Mutex::new(MowerState {
                homie_status: initial_state,
                battery_percent: data.battery.battery_percent,
                latitude: data
                    .positions
                    .as_ref()
                    .and_then(|ps| ps.last())
                    .and_then(|p| p.latitude),
                longitude: data
                    .positions
                    .as_ref()
                    .and_then(|ps| ps.last())
                    .and_then(|p| p.longitude),
                mower_mode: data.mower.mode,
                mower_activity: data.mower.activity,
                mower_inactive_reason: data.mower.inactive_reason,
                mower_state: data.mower.state,
                work_area_id: data.mower.work_area_id,
                error_code: data.mower.error_code,
                error_code_timestamp: data.mower.error_code_timestamp,
                is_error_confirmable: data.mower.is_error_confirmable,
                planner_external_reason: data.planner.external_reason,
                planner_restricted_reason: data.planner.restricted_reason,
                planner_next_start: data.planner.next_start_timestamp,
                planner_override: data.planner.r#override.map(|o| o.action),
                cutting_height: data.settings.as_ref().and_then(|s| s.cutting_height),
                headlight_mode: data
                    .settings
                    .as_ref()
                    .and_then(|s| s.headlight.as_ref())
                    .map(|h| h.mode.clone()),
            })),
        }
    }

    fn is_retained(&self, node_id: &HomieID, prop_id: &HomieID) -> bool {
        self.description
            .nodes
            .get(node_id)
            .unwrap_or_else(|| panic!("attempting to publish unkonwn automower node `{node_id}`"))
            .properties
            .get(prop_id)
            .unwrap_or_else(|| {
                panic!("attempting to publish unkonwn automower property `{node_id}/{prop_id}`")
            })
            .retained
    }

    async fn record_event(&self, event: schemas::websocket::Event) -> Result<(), Error> {
        let events = match event {
            schemas::websocket::Event::BatteryEventV2 { attributes: a, .. } => {
                let mut data = self.data.lock().await;
                data.battery_percent = a
                    .battery
                    .and_then(|b| b.battery_percent)
                    .unwrap_or(data.battery_percent);
                let pct = data.battery_percent.to_string();
                vec![(&BATTERY_NODE_ID, &BATTERY_LEVEL_PROP_ID, pct)]
            }
            schemas::websocket::Event::CalendarEventV2 { attributes: _a, .. } => return Ok(()),
            schemas::websocket::Event::CuttingHeightEventV2 { attributes: a, .. } => {
                let mut data = self.data.lock().await;
                data.cutting_height = a
                    .cutting_height
                    .and_then(|c| c.height)
                    .or(data.cutting_height);
                let Some(ch) = data.cutting_height else {
                    return Ok(());
                };
                let ch = ch.to_string();
                vec![(&SETTINGS_NODE_ID, &SETTINGS_CUTTING_HEIGHT_PROP_ID, ch)]
            }
            schemas::websocket::Event::HeadlightsEventV2 { attributes: a, .. } => {
                let mut data = self.data.lock().await;
                data.headlight_mode = a.headlight.map(|c| c.mode).or(data.headlight_mode.clone());
                let Some(mode) = data.headlight_mode.clone() else {
                    return Ok(());
                };
                vec![(&SETTINGS_NODE_ID, &SETTINGS_HEADLIGHT_PROP_ID, mode)]
            }
            schemas::websocket::Event::MessageEventV2 { attributes: _a, .. } => return Ok(()),
            schemas::websocket::Event::MowerEventV2 { attributes: a, .. } => {
                let mut evts = vec![];
                let Some(mower) = a.mower else { return Ok(()) };
                if let Some(mode) = mower.mode {
                    evts.push((&MOWER_NODE_ID, &MOWER_MODE_PROP_ID, mode));
                }
                if let Some(activity) = mower.activity {
                    evts.push((&MOWER_NODE_ID, &MOWER_ACTIVITY_PROP_ID, activity));
                }
                if let Some(reason) = mower.inactive_reason {
                    evts.push((&MOWER_NODE_ID, &MOWER_INACTIVE_REASON_PROP_ID, reason));
                }
                if let Some(state) = mower.state {
                    evts.push((&MOWER_NODE_ID, &MOWER_STATE_PROP_ID, state));
                }
                if let Some(ec) = mower.error_code {
                    evts.push((&MOWER_NODE_ID, &MOWER_ERROR_CODE_PROP_ID, ec.to_string()));
                }
                if let Some(id) = mower.work_area_id {
                    evts.push((&MOWER_NODE_ID, &MOWER_WORK_AREA_ID_PROP_ID, id.to_string()));
                }
                evts
            }
            schemas::websocket::Event::PlannerEventV2 { attributes: a, .. } => {
                let mut evts = vec![];
                let Some(planner) = a.planner else {
                    return Ok(());
                };
                if let Some(t) = planner.next_start_timestamp {
                    let dt = if t == 0 {
                        jiff::Timestamp::now().to_string()
                    } else {
                        mower_datetime(t).to_string()
                    };
                    evts.push((&PLANNER_NODE_ID, &PLANNER_NEXT_START_PROP_ID, dt));
                }
                if let Some(o) = planner.r#override {
                    evts.push((&PLANNER_NODE_ID, &PLANNER_OVERRIDE_PROP_ID, o.action));
                }
                if let Some(r) = planner.restricted_reason {
                    evts.push((&PLANNER_NODE_ID, &PLANNER_RESTRICTED_REASON_PROP_ID, r));
                }
                if let Some(r) = planner.external_reason {
                    let r = r.to_string();
                    evts.push((&PLANNER_NODE_ID, &PLANNER_EXTERNAL_REASON_PROP_ID, r));
                }
                evts
            }
            schemas::websocket::Event::PositionEventV2 { attributes: a, .. } => {
                let mut data = self.data.lock().await;
                let pos = a.position.as_ref();
                data.latitude = pos.and_then(|p| p.latitude).or(data.latitude);
                data.longitude = pos.and_then(|p| p.longitude).or(data.longitude);
                let (Some(lat), Some(lon)) = (data.latitude, data.longitude) else {
                    return Ok(());
                };
                let latlon = format!("{},{}", lat, lon);
                vec![(&POSITION_NODE_ID, &POSITION_LATLON_PROP_ID, latlon)]
            }
        };
        for (node_id, prop_id, value) in events {
            let retained = Self::is_retained(&self, node_id, prop_id);
            let p = self
                .protocol
                .publish_value(node_id, prop_id, value, retained);
            self.mqtt
                .homie_publish(p)
                .await
                .map_err(|e| Error::PublishValue(e, node_id, prop_id))?;
        }
        Ok(())
    }

    async fn publish_device(&self) -> Result<(), Error> {
        let initial_state = self.data.lock().await.homie_status;
        for step in homie5::homie_device_publish_steps() {
            match step {
                homie5::DevicePublishStep::DeviceStateInit => {
                    let p = self.protocol.publish_state(initial_state);
                    self.mqtt
                        .homie_publish(p)
                        .await
                        .map_err(Error::PublishInitState)?;
                }
                homie5::DevicePublishStep::DeviceDescription => {
                    let p = self
                        .protocol
                        .publish_description(&self.description)
                        .map_err(Error::GenerateDescription)?;
                    self.mqtt
                        .homie_publish(p)
                        .await
                        .map_err(Error::PublishDescription)?;
                }
                homie5::DevicePublishStep::PropertyValues => {
                    use schemas::websocket::*;
                    if initial_state == HomieDeviceStatus::Disconnected {
                        continue;
                    }
                    let evs = {
                        let data = self.data.lock().await;
                        [
                            Event::BatteryEventV2 {
                                id: self.api_id.clone(),
                                attributes: Default::default(),
                            },
                            Event::PositionEventV2 {
                                id: self.api_id.clone(),
                                attributes: Default::default(),
                            },
                            Event::MowerEventV2 {
                                id: self.api_id.clone(),
                                attributes: MowerAttributes {
                                    mower: Some(Mower {
                                        mode: Some(data.mower_mode.clone()),
                                        activity: Some(data.mower_activity.clone()),
                                        inactive_reason: data
                                            .mower_inactive_reason
                                            .clone()
                                            .or_else(|| Some("NONE".into())),
                                        state: Some(data.mower_state.clone()),
                                        work_area_id: data.work_area_id,
                                        error_code: data.error_code,
                                        error_code_timestamp: data.error_code_timestamp,
                                        is_error_confirmable: data.is_error_confirmable,
                                    }),
                                },
                            },
                            Event::PlannerEventV2 {
                                id: self.api_id.clone(),
                                attributes: PlannerAttributes {
                                    planner: Some(Planner {
                                        next_start_timestamp: Some(data.planner_next_start),
                                        r#override: data.planner_override.clone().map(|o| {
                                            schemas::automower::PlannerOverride { action: o }
                                        }),
                                        restricted_reason: Some(
                                            data.planner_restricted_reason.clone(),
                                        ),
                                        external_reason: data.planner_external_reason,
                                    }),
                                },
                            },
                            Event::HeadlightsEventV2 {
                                id: self.api_id.clone(),
                                attributes: Default::default(),
                            },
                            Event::CuttingHeightEventV2 {
                                id: self.api_id.clone(),
                                attributes: Default::default(),
                            },
                        ]
                    };
                    for ev in evs {
                        self.record_event(ev).await?;
                    }
                }
                homie5::DevicePublishStep::SubscribeProperties => {
                    if initial_state == HomieDeviceStatus::Disconnected {
                        continue;
                    }
                    let p = self
                        .protocol
                        .subscribe_props(&self.description)
                        .map_err(Error::GenerateSubscribtions)?;
                    self.mqtt
                        .homie_subscribe(p)
                        .await
                        .map_err(Error::Subscribe)?;
                }
                homie5::DevicePublishStep::DeviceStateReady => {
                    if initial_state == HomieDeviceStatus::Disconnected {
                        continue;
                    }
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

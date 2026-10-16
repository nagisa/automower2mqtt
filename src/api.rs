use crate::schemas;
use futures::SinkExt as _;
use futures::StreamExt as _;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify, watch};
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::Bytes;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const API_BASE: &str = "https://api.amc.husqvarna.dev/v1";
const TOKEN_URL: &str = "https://api.authentication.husqvarnagroup.dev/v1/oauth2/token";
const TOKEN_EXPIRATION_HEADROOM: Duration = Duration::from_secs(30);
const WORK_AREAS_POLL_INTERVAL: Duration = Duration::from_secs(15 * 60);
const WORK_AREA_REFRESH_TIMEOUT: Duration = Duration::from_secs(30);
/// Delays between the successive reads of the work areas while somebody is waiting for a change
/// to show up. Once these run out, the regular poll interval applies again.
const WORK_AREA_WAIT_POLL_DELAYS: [Duration; 5] = [
    Duration::from_secs(2),
    Duration::from_secs(3),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(20),
];

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("could not get an access token")]
    GetToken(#[source] reqwest::Error),
    #[error("could not make a request to the `GET {1}` API endpoint")]
    Get(#[source] reqwest::Error, String),
    #[error("could not make a request to update the `{1}` API endpoint")]
    Write(#[source] reqwest::Error, String),
    #[error("API request to `{0}` was rejected with status {1}: {2}")]
    Rejected(String, reqwest::StatusCode, String),
    #[error("could not read the API response for `GET {1}`")]
    ReadGetResponse(#[source] reqwest::Error, String),
    #[error("could not read the API response for `{1}`")]
    ReadWriteResponse(#[source] reqwest::Error, String),
    #[error("an error in websocket")]
    Websocket(#[source] tungstenite::Error),
}

struct Token {
    value: Arc<str>,
    expires_at: boot_time::Instant,
}

pub(crate) struct Client {
    http: reqwest::Client,
    app_key: String,
    app_secret: String,
    token: Mutex<Token>,
    mowers: Mutex<std::collections::BTreeMap<String, Arc<Mower>>>,
    initialized: watch::Sender<bool>,
}

impl Client {
    pub(crate) fn new(http: reqwest::Client, app_key: String, app_secret: String) -> Self {
        let (initialized, _) = watch::channel(false);
        Self {
            http,
            app_key,
            app_secret,
            token: Mutex::new(Token {
                value: Arc::from(""),
                expires_at: boot_time::Instant::now(),
            }),
            mowers: Mutex::new(std::collections::BTreeMap::new()),
            initialized,
        }
    }

    pub(crate) async fn mowers(&self) -> Vec<Arc<Mower>> {
        self.mowers.lock().await.values().cloned().collect()
    }

    pub(crate) async fn wait_initialized(&self) {
        let mut initialized = self.initialized.subscribe();
        while !*initialized.borrow() {
            if initialized.changed().await.is_err() {
                return;
            }
        }
    }

    pub(crate) async fn run(self: Arc<Self>) -> Result<(), Error> {
        self.load_mowers().await?;
        let mowers = self.mowers().await;
        self.initialized.send_replace(true);
        for mower in mowers {
            let client = Arc::clone(&self);
            tokio::spawn(async move { client.refresh_work_areas_loop(mower).await });
        }
        self.monitor_websocket().await
    }

    async fn load_mowers(&self) -> Result<(), Error> {
        let response = self
            .get::<schemas::automower::JsonApiDataListDocument>("/mowers")
            .await?;
        for datum in response.data {
            if datum.r#type != "mower" {
                continue;
            }
            let mower = Arc::new(Mower::new(datum.id, datum.attributes));
            if let Err(error) = self.refresh_work_areas(&mower).await {
                tracing::warn!(%error, mower_id=%mower.id(), "could not load work areas; keeping summary data");
            }
            self.mowers
                .lock()
                .await
                .insert(mower.id().to_owned(), mower);
        }
        Ok(())
    }

    async fn monitor_websocket(self: Arc<Self>) -> Result<(), Error> {
        'reconnect: loop {
            let token = self.token().await?;
            let mut request = "wss://ws.openapi.husqvarna.dev/v1"
                .into_client_request()
                .expect("valid websocket endpoint");
            let headers = request.headers_mut();
            headers.insert(
                "x-api-key",
                self.app_key.parse().expect("valid API key header"),
            );
            headers.insert(
                "authorization",
                format!("Bearer {token}")
                    .parse()
                    .expect("valid authorization header"),
            );
            headers.insert(
                "authorization-provider",
                "husqvarna".parse().expect("valid provider header"),
            );
            let (mut stream, response) = match tokio_tungstenite::connect_async(request).await {
                Ok(connection) => connection,
                Err(error) => {
                    tracing::warn!(%error, "could not connect to the Husqvarna websocket; retrying");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue 'reconnect;
                }
            };
            tracing::debug!(?response, "connected to Husqvarna websocket");
            let mut ping_timer = tokio::time::interval(Duration::from_secs(10));
            loop {
                tokio::select! {
                    result = stream.next() => {
                        let Some(result) = result else { continue 'reconnect; };
                        tracing::debug!(websocket_message=?result, "received a websocket message");
                        let message: Result<schemas::websocket::Event, _> = match result {
                            Ok(tungstenite::Message::Text(text)) => {
                                let event = serde_json::from_str(&text);
                                if event.is_err() && serde_json::from_str::<schemas::websocket::ConnectionEvent>(&text).is_ok() { continue; }
                                event
                            }
                            Ok(tungstenite::Message::Binary(bytes)) => serde_json::from_slice(&bytes),
                            Ok(tungstenite::Message::Ping(payload)) => { stream.feed(tungstenite::Message::Pong(payload)).await.expect("websocket pong send"); continue; }
                            Ok(tungstenite::Message::Pong(_)) => { tracing::debug!("got a pong"); continue; }
                            Ok(tungstenite::Message::Close(_)) | Err(tungstenite::Error::Protocol(_)) => continue 'reconnect,
                            Ok(tungstenite::Message::Frame(_)) => unreachable!(),
                            Err(error) => break 'reconnect Err(Error::Websocket(error)),
                        };
                        let event = match message {
                            Ok(event) => event,
                            Err(error) => { tracing::warn!(?error, "could not deserialize websocket message"); continue; }
                        };
                        let id = event_mower_id(&event);
                        let mower = self.mowers.lock().await.get(id).cloned();
                        if let Some(mower) = mower {
                            if mower.apply_event(&event).await {
                                mower.request_work_area_refresh();
                            }
                        } else {
                            tracing::warn!(%id, "received event for an unknown mower");
                        }
                    }
                    _ = ping_timer.tick() => {
                        stream.feed(tungstenite::Message::Ping(Bytes::from_static(b"would be very nice of you to pong, thank you very much"))).await.expect("websocket ping send");
                    }
                }
            }
        }
    }

    /// Keeps the work areas up to date, as they are not covered by the websocket.
    ///
    /// Reads them periodically, whenever the mower asks for it, and at a quicker pace for as long
    /// as somebody is waiting for a change to show up (see `Mower::wait_for`). Every read
    /// postpones the next periodic one.
    async fn refresh_work_areas_loop(self: Arc<Self>, mower: Arc<Mower>) {
        use tokio::time::Instant;
        let mut periodic = Instant::now();
        let mut quick: Option<Instant> = None;
        let mut quick_attempt = 0;
        loop {
            let wake = quick.map_or(periodic, |quick| quick.min(periodic));
            tokio::select! {
                _ = tokio::time::sleep_until(wake) => {}
                _ = mower.refresh_requested.notified() => {}
                _ = mower.waiters_changed.notified() => {
                    if mower.has_waiters() {
                        let soonest = Instant::now() + WORK_AREA_WAIT_POLL_DELAYS[0];
                        quick = Some(quick.map_or(soonest, |quick| quick.min(soonest)));
                        quick_attempt = 0;
                    } else {
                        quick = None;
                    }
                    continue;
                }
            }
            let result =
                tokio::time::timeout(WORK_AREA_REFRESH_TIMEOUT, self.refresh_work_areas(&mower))
                    .await;
            match result {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    tracing::warn!(%error, mower_id=%mower.id(), "could not refresh work areas")
                }
                Err(_) => tracing::warn!(mower_id=%mower.id(), "work area refresh timed out"),
            }
            periodic = Instant::now() + WORK_AREAS_POLL_INTERVAL;
            mower.notify_waiters().await;
            quick = match WORK_AREA_WAIT_POLL_DELAYS.get(quick_attempt) {
                Some(delay) if mower.has_waiters() => {
                    quick_attempt += 1;
                    Some(Instant::now() + *delay)
                }
                _ => None,
            };
        }
    }

    pub(crate) async fn post_mower(
        &self,
        mower_id: &str,
        command: &str,
        body: serde_json::Value,
    ) -> Result<(), Error> {
        self.write(
            reqwest::Method::POST,
            &format!("/mowers/{mower_id}/{command}"),
            body,
        )
        .await
    }

    pub(crate) async fn patch_work_area(
        &self,
        mower_id: &str,
        work_area_id: i64,
        attributes: serde_json::Value,
    ) -> Result<(), Error> {
        self.write(
            reqwest::Method::PATCH,
            &format!("/mowers/{mower_id}/workAreas/{work_area_id}"),
            serde_json::json!({ "data": {
                "id": work_area_id,
                "type": "workArea",
                "attributes": attributes
            }}),
        )
        .await
    }

    pub(crate) async fn token(&self) -> Result<Arc<str>, Error> {
        let now = boot_time::Instant::now();
        {
            let token = self.token.lock().await;
            if token.expires_at > now {
                return Ok(Arc::clone(&token.value));
            }
        }

        let response = self
            .http
            .post(TOKEN_URL)
            .header("accept", "application/json")
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", &self.app_key),
                ("client_secret", &self.app_secret),
            ])
            .send()
            .await
            .map_err(Error::GetToken)?
            .json::<schemas::oauth::TokenResponse>()
            .await
            .map_err(Error::GetToken)?;

        let mut token = self.token.lock().await;
        token.value = response.access_token.into();
        let duration =
            Duration::from_secs_f32(response.expires_in).saturating_sub(TOKEN_EXPIRATION_HEADROOM);
        token.expires_at = boot_time::Instant::now()
            .checked_add(duration)
            .expect("token expiry is within range");
        Ok(Arc::clone(&token.value))
    }

    pub(crate) async fn get<T>(&self, uri: &str) -> Result<T, Error>
    where
        T: serde::de::DeserializeOwned,
    {
        let token = self.token().await?;
        tracing::debug!(%uri, method = "GET");
        let response = self
            .http
            .get(format!("{API_BASE}{uri}"))
            .header("accept", "application/vnd.api+json")
            .header("x-api-key", &self.app_key)
            .header("authorization", format!("Bearer {token}"))
            .header("authorization-provider", "husqvarna")
            .send()
            .await
            .map_err(|error| Error::Get(error, uri.to_owned()))?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .map_err(|error| Error::ReadGetResponse(error, uri.to_owned()))?;
            tracing::error!(?body, %uri, method = "GET");
            return Err(Error::Rejected(format!("GET {uri}"), status, body));
        }
        tracing::debug!(?response, %uri, method = "GET");
        response
            .json::<T>()
            .await
            .map_err(|error| Error::ReadGetResponse(error, uri.to_owned()))
    }

    pub(crate) async fn write(
        &self,
        method: reqwest::Method,
        uri: &str,
        body: serde_json::Value,
    ) -> Result<(), Error> {
        let token = self.token().await?;
        let response = self
            .http
            .request(method.clone(), format!("{API_BASE}{uri}"))
            .header("accept", "application/vnd.api+json")
            .header("content-type", "application/vnd.api+json")
            .header("x-api-key", &self.app_key)
            .header("authorization", format!("Bearer {token}"))
            .header("authorization-provider", "husqvarna")
            .json(&body)
            .send()
            .await
            .map_err(|error| Error::Write(error, format!("{method} {uri}")))?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .map_err(|error| Error::ReadWriteResponse(error, format!("{method} {uri}")))?;
            tracing::error!(?body, %uri, %method);
            return Err(Error::Rejected(format!("{method} {uri}"), status, body));
        }
        let response = response
            .json::<serde_json::Value>()
            .await
            .map_err(|error| Error::ReadWriteResponse(error, format!("{method} {uri}")))?;
        tracing::debug!(?response, %uri, %method);
        Ok(())
    }

    /// Read all work areas of the mower with a single request and store them.
    pub(crate) async fn refresh_work_areas(
        &self,
        mower: &Mower,
    ) -> Result<std::collections::BTreeMap<i64, schemas::automower::WorkArea>, Error> {
        let uri = format!("/mowers/{}/workAreas", mower.id());
        let document = self
            .get::<schemas::automower::JsonApiDataListDocumentWorkAreas>(&uri)
            .await?;
        let work_areas = document
            .data
            .into_iter()
            .map(|mut area| {
                area.attributes.work_area_id = Some(area.id);
                (area.id, area.attributes)
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        mower
            .replace_work_areas(work_areas.values().cloned().collect())
            .await;
        Ok(work_areas)
    }
}

type Waiter = Box<dyn Fn(&schemas::automower::MowerData) -> bool + Send>;

pub(crate) struct Mower {
    id: String,
    data: Mutex<schemas::automower::MowerData>,
    changed: watch::Sender<u64>,
    refresh_requested: Notify,
    waiters_changed: Notify,
    waiters: std::sync::Mutex<std::collections::BTreeMap<u64, Waiter>>,
    next_waiter: std::sync::atomic::AtomicU64,
}

/// Returned by `Mower::wait_for`. The waiting is over when this is dropped.
pub(crate) struct WorkAreaWait {
    mower: Arc<Mower>,
    id: u64,
}

impl Drop for WorkAreaWait {
    fn drop(&mut self) {
        self.mower.waiters.lock().unwrap().remove(&self.id);
        self.mower.waiters_changed.notify_one();
    }
}

impl Mower {
    pub(crate) fn new(id: String, data: schemas::automower::MowerData) -> Self {
        let (changed, _) = watch::channel(0);
        Self {
            id,
            data: Mutex::new(data),
            changed,
            refresh_requested: Notify::new(),
            waiters_changed: Notify::new(),
            waiters: Default::default(),
            next_waiter: Default::default(),
        }
    }

    /// Have the work areas read now.
    pub(crate) fn request_work_area_refresh(&self) {
        self.refresh_requested.notify_one();
    }

    /// Have the work areas read at a quicker pace until `done` is satisfied with the mower data
    /// or the returned handle is dropped, whichever comes first.
    ///
    /// Writes to the mower take a while to be reflected by the API. The caller knows what it is
    /// waiting for, so it decides when to stop. All waiters of the mower share the same reads.
    pub(crate) fn wait_for(
        self: &Arc<Self>,
        done: impl Fn(&schemas::automower::MowerData) -> bool + Send + 'static,
    ) -> WorkAreaWait {
        let id = self
            .next_waiter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.waiters.lock().unwrap().insert(id, Box::new(done));
        self.waiters_changed.notify_one();
        WorkAreaWait {
            mower: Arc::clone(self),
            id,
        }
    }

    fn has_waiters(&self) -> bool {
        !self.waiters.lock().unwrap().is_empty()
    }

    /// Forget about the waiters that are satisfied by the current data.
    async fn notify_waiters(&self) {
        let data = self.snapshot().await;
        self.waiters.lock().unwrap().retain(|_, done| !done(&data));
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) async fn snapshot(&self) -> schemas::automower::MowerData {
        self.data.lock().await.clone()
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    fn notify_changed(&self) {
        self.changed.send_modify(|revision| *revision += 1);
    }

    pub(crate) async fn replace_work_areas(&self, work_areas: Vec<schemas::automower::WorkArea>) {
        let changed = {
            let mut data = self.data.lock().await;
            let differs = data.work_areas.as_ref() != Some(&work_areas);
            if differs {
                data.work_areas = Some(work_areas);
            }
            differs
        };
        if changed {
            self.notify_changed();
        }
    }

    pub(crate) async fn apply_event(&self, event: &schemas::websocket::Event) -> bool {
        use schemas::websocket::Event;
        let (changed, mower_state_changed) = {
            let mut data = self.data.lock().await;
            let mut changed = false;
            let mut mower_state_changed = false;
            match event {
                Event::BatteryEventV2 { attributes, .. } => {
                    if let Some(value) = attributes
                        .battery
                        .as_ref()
                        .and_then(|battery| battery.battery_percent)
                    {
                        changed |= replace(&mut data.battery.battery_percent, value);
                    }
                }
                Event::CalendarEventV2 { attributes, .. } => {
                    if let (Some(calendar), Some(work_areas)) =
                        (attributes.calendar.as_ref(), data.work_areas.as_mut())
                    {
                        let has_unassigned = calendar
                            .tasks
                            .iter()
                            .any(|task| task.work_area_id.is_none());
                        for area in work_areas {
                            let tasks = calendar
                                .tasks
                                .iter()
                                .filter(|task| task.work_area_id == area.work_area_id)
                                .cloned()
                                .collect::<Vec<_>>();
                            if !has_unassigned || !tasks.is_empty() {
                                let calendar = schemas::automower::CalendarWorkArea { tasks };
                                if area.calendar.as_ref() != Some(&calendar) {
                                    area.calendar = Some(calendar);
                                    changed = true;
                                }
                            }
                        }
                    }
                }
                Event::CuttingHeightEventV2 { attributes, .. } => {
                    if let Some(value) = attributes
                        .cutting_height
                        .as_ref()
                        .and_then(|height| height.height)
                    {
                        let settings = data.settings.get_or_insert_with(empty_settings);
                        changed |= replace(&mut settings.cutting_height, Some(value));
                    }
                }
                Event::HeadlightsEventV2 { attributes, .. } => {
                    if let Some(value) = attributes.headlight.as_ref() {
                        let settings = data.settings.get_or_insert_with(empty_settings);
                        if settings.headlight.as_ref() != Some(value) {
                            settings.headlight = Some(value.clone());
                            changed = true;
                        }
                    }
                }
                Event::MowerEventV2 { attributes, .. } => {
                    if let Some(mower) = attributes.mower.as_ref() {
                        let previous_state = (
                            data.mower.mode.clone(),
                            data.mower.activity.clone(),
                            data.mower.state.clone(),
                            data.mower.work_area_id,
                        );
                        changed |= replace_some(&mut data.mower.mode, mower.mode.as_ref());
                        changed |= replace_some(&mut data.mower.activity, mower.activity.as_ref());
                        changed |= replace_optional(
                            &mut data.mower.inactive_reason,
                            mower.inactive_reason.as_ref(),
                        );
                        changed |= replace_some(&mut data.mower.state, mower.state.as_ref());
                        changed |= replace_optional(
                            &mut data.mower.work_area_id,
                            mower.work_area_id.as_ref(),
                        );
                        mower_state_changed = previous_state
                            != (
                                data.mower.mode.clone(),
                                data.mower.activity.clone(),
                                data.mower.state.clone(),
                                data.mower.work_area_id,
                            );
                        changed |=
                            replace_optional(&mut data.mower.error_code, mower.error_code.as_ref());
                        changed |= replace_optional(
                            &mut data.mower.error_code_timestamp,
                            mower.error_code_timestamp.as_ref(),
                        );
                        changed |= replace_optional(
                            &mut data.mower.is_error_confirmable,
                            mower.is_error_confirmable.as_ref(),
                        );
                    }
                }
                Event::PlannerEventV2 { attributes, .. } => {
                    if let Some(planner) = attributes.planner.as_ref() {
                        changed |= replace_some(
                            &mut data.planner.next_start_timestamp,
                            planner.next_start_timestamp.as_ref(),
                        );
                        if let Some(value) = planner.r#override.as_ref()
                            && data.planner.r#override.as_ref().map(|item| &item.action)
                                != Some(&value.action)
                        {
                            data.planner.r#override = Some(schemas::automower::PlannerOverride {
                                action: value.action.clone(),
                            });
                            changed = true;
                        }
                        changed |= replace_some(
                            &mut data.planner.restricted_reason,
                            planner.restricted_reason.as_ref(),
                        );
                        changed |= replace_optional(
                            &mut data.planner.external_reason,
                            planner.external_reason.as_ref(),
                        );
                    }
                }
                Event::PositionEventV2 { attributes, .. } => {
                    if let Some(position) = attributes.position.as_ref() {
                        let positions = data.positions.get_or_insert_with(Vec::new);
                        if positions.last().map(|last| (last.latitude, last.longitude))
                            != Some((position.latitude, position.longitude))
                        {
                            positions.push(position.clone());
                            changed = true;
                        }
                    }
                }
                Event::MessageEventV2 { .. } => {}
            }
            (changed, mower_state_changed)
        };
        if changed {
            self.notify_changed();
        }
        mower_state_changed
    }
}

fn event_mower_id(event: &schemas::websocket::Event) -> &str {
    use schemas::websocket::Event::*;
    match event {
        BatteryEventV2 { id, .. }
        | CalendarEventV2 { id, .. }
        | CuttingHeightEventV2 { id, .. }
        | HeadlightsEventV2 { id, .. }
        | MessageEventV2 { id, .. }
        | MowerEventV2 { id, .. }
        | PlannerEventV2 { id, .. }
        | PositionEventV2 { id, .. } => id,
    }
}

fn replace<T: PartialEq>(current: &mut T, new: T) -> bool {
    if *current == new {
        false
    } else {
        *current = new;
        true
    }
}

fn replace_some<T: Clone + PartialEq>(current: &mut T, new: Option<&T>) -> bool {
    new.is_some_and(|new| replace(current, new.clone()))
}

fn replace_optional<T: Clone + PartialEq>(current: &mut Option<T>, new: Option<&T>) -> bool {
    new.is_some_and(|new| replace(current, Some(new.clone())))
}

fn empty_settings() -> schemas::automower::Settings {
    schemas::automower::Settings {
        cutting_height: None,
        headlight: None,
        timer: None,
    }
}

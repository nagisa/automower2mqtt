#![expect(dead_code)]

pub(crate) mod oauth {
    #[derive(Debug, serde::Deserialize)]
    pub(crate) struct TokenResponse {
        pub(crate) access_token: String,
        pub(crate) expires_in: f32,
    }
}

pub(crate) mod automower {
    use serde::Deserialize;

    #[derive(Debug, serde::Deserialize)]
    pub(crate) struct JsonApiDataListDocument {
        pub(crate) data: Vec<JsonApiData>,
    }

    #[derive(Debug, serde::Deserialize)]
    pub(crate) struct JsonApiData {
        pub(crate) r#type: String,
        pub(crate) id: String,
        pub(crate) attributes: MowerData,
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct MowerData {
        pub(crate) system: System,
        pub(crate) battery: Battery,
        pub(crate) mower: MowerApp,
        pub(crate) planner: Planner,
        pub(crate) metadata: Metadata,
        pub(crate) capabilities: Option<Capabilities>,
        pub(crate) calendar: Option<Calendar>,
        pub(crate) positions: Option<Vec<Position>>,
        pub(crate) settings: Option<Settings>,
        pub(crate) statistics: Option<Statistics>,
        pub(crate) stay_out_zones: Option<StayOutZones>,
        pub(crate) work_areas: Option<Vec<WorkArea>>,
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct System {
        pub(crate) name: String,
        pub(crate) model: String,
        pub(crate) serial_number: u32,
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct Battery {
        pub(crate) battery_percent: u8,
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct Capabilities {
        pub(crate) can_confirm_error: bool,
        pub(crate) headlights: bool,
        pub(crate) position: bool,
        pub(crate) stay_out_zones: bool,
        pub(crate) work_areas: bool,
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct MowerApp {
        pub(crate) mode: String,
        pub(crate) activity: String,
        pub(crate) inactive_reason: Option<String>,
        pub(crate) state: String,
        pub(crate) work_area_id: Option<i64>,
        pub(crate) error_code: Option<u16>,
        pub(crate) error_code_timestamp: Option<u64>,
        pub(crate) is_error_confirmable: Option<bool>,
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct Calendar {
        pub(crate) tasks: Vec<CalendarTask>,
    }

    #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    #[serde(deny_unknown_fields)]
    pub(crate) struct CalendarTask {
        pub(crate) start: u16,
        pub(crate) duration: u16,
        pub(crate) monday: bool,
        pub(crate) tuesday: bool,
        pub(crate) wednesday: bool,
        pub(crate) thursday: bool,
        pub(crate) friday: bool,
        pub(crate) saturday: bool,
        pub(crate) sunday: bool,
        // this field is not optional in API, we just don't want to send/require this field in the
        // MQTT property as it unnecessarily duplicates the data...
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_optional_i64_or_string"
        )]
        pub(crate) work_area_id: Option<i64>,
    }

    #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    #[serde(deny_unknown_fields)]
    pub(crate) struct CalendarWorkArea {
        pub(crate) tasks: Vec<CalendarTask>,
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct Planner {
        pub(crate) next_start_timestamp: u64,
        pub(crate) r#override: Option<PlannerOverride>,
        pub(crate) restricted_reason: String,
        pub(crate) external_reason: Option<i32>,
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct PlannerOverride {
        pub(crate) action: String,
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct Metadata {
        pub(crate) connected: bool,
        pub(crate) status_timestamp: u64,
    }

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct WorkAreas {
        pub(crate) items: Vec<WorkArea>,
    }

    #[derive(Debug, Clone, PartialEq, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct WorkArea {
        #[serde(default, deserialize_with = "deserialize_optional_i64_or_string")]
        pub(crate) work_area_id: Option<i64>,
        pub(crate) name: Option<String>,
        pub(crate) r#type: Option<String>,
        pub(crate) cutting_height: Option<u8>,
        pub(crate) enabled: Option<bool>,
        pub(crate) schedulable: Option<bool>,
        pub(crate) orientation: Option<u16>,
        pub(crate) orientation_shift: Option<u16>,
        pub(crate) current_orientation: Option<u16>,
        pub(crate) overlap: Option<u16>,
        pub(crate) progress: Option<u8>,
        pub(crate) last_time_completed: Option<i64>,
        pub(crate) last_time_abandoned: Option<i64>,
        pub(crate) use_global_cutting_height: Option<bool>,
        pub(crate) calendar: Option<CalendarWorkArea>,
    }

    #[derive(Debug, serde::Deserialize)]
    pub(crate) struct JsonApiDataListDocumentWorkAreas {
        pub(crate) data: Vec<JsonApiDataWorkArea>,
    }

    #[derive(Debug, serde::Deserialize)]
    pub(crate) struct JsonApiDataDocumentWorkArea {
        pub(crate) data: Option<JsonApiDataWorkArea>,
    }

    #[derive(Debug, serde::Deserialize)]
    pub(crate) struct JsonApiDataWorkArea {
        #[serde(deserialize_with = "deserialize_i64_or_string")]
        pub(crate) id: i64,
        pub(crate) attributes: WorkArea,
    }

    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum I64OrString {
        Integer(i64),
        String(String),
    }

    impl I64OrString {
        fn into_i64<E: serde::de::Error>(self) -> Result<i64, E> {
            match self {
                Self::Integer(value) => Ok(value),
                Self::String(value) => value.parse().map_err(E::custom),
            }
        }
    }

    fn deserialize_i64_or_string<'de, D>(deserializer: D) -> Result<i64, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        I64OrString::deserialize(deserializer)?.into_i64()
    }

    fn deserialize_optional_i64_or_string<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Option::<I64OrString>::deserialize(deserializer)?
            .map(I64OrString::into_i64)
            .transpose()
    }

    #[derive(Debug, serde::Deserialize, Clone)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct Position {
        pub(crate) latitude: Option<f64>,
        pub(crate) longitude: Option<f64>,
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct Settings {
        pub(crate) cutting_height: Option<u8>,
        pub(crate) headlight: Option<Headlight>,
        pub(crate) timer: Option<Timer>,
    }

    #[derive(Debug, Clone, PartialEq, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct Headlight {
        pub(crate) mode: String,
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct Timer {
        pub(crate) date_time: Option<i64>,
        pub(crate) time_zone: Option<String>,
    }

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct Messages {
        pub(crate) messages: Option<Vec<Message>>,
    }

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct Message {
        pub(crate) time: i64,
        pub(crate) code: i32,
        pub(crate) severity: String,
        pub(crate) latitude: Option<f64>,
        pub(crate) longitude: Option<f64>,
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct Statistics {
        pub(crate) cutting_blade_usage_time: Option<u64>,
        pub(crate) down_time: Option<u64>,
        pub(crate) number_of_charging_cycles: Option<u32>,
        pub(crate) number_of_collisions: Option<u32>,
        pub(crate) total_charging_time: Option<u64>,
        pub(crate) total_cutting_time: Option<u64>,
        pub(crate) total_drive_distance: Option<u64>,
        pub(crate) total_running_time: Option<u64>,
        pub(crate) total_searching_time: Option<u64>,
        pub(crate) up_time: Option<u64>,
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct StayOutZones {
        pub(crate) dirty: Option<bool>,
        pub(crate) zones: Option<Vec<StayOutZone>>,
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct StayOutZone {
        pub(crate) id: Option<String>,
        pub(crate) name: Option<String>,
        pub(crate) enabled: Option<bool>,
    }
}

pub(crate) mod websocket {
    use super::automower::*;

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct Battery {
        pub(crate) battery_percent: Option<u8>,
    }

    #[derive(Debug, serde::Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct BatteryAttributes {
        pub(crate) battery: Option<Battery>,
    }

    #[derive(Debug, serde::Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct CalendarAttributes {
        pub(crate) calendar: Option<Calendar>,
    }

    #[derive(Debug, serde::Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct CuttingHeightAttributes {
        pub(crate) cutting_height: Option<CuttingHeight>,
    }

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct CuttingHeight {
        pub(crate) height: Option<u8>,
    }

    #[derive(Debug, serde::Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct HeadlightAttributes {
        pub(crate) headlight: Option<Headlight>,
    }

    #[derive(Debug, serde::Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct MessageAttributes {
        pub(crate) message: Option<Message>,
    }

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct Mower {
        pub(crate) mode: Option<String>,
        pub(crate) activity: Option<String>,
        pub(crate) inactive_reason: Option<String>,
        pub(crate) state: Option<String>,
        pub(crate) work_area_id: Option<i64>,
        pub(crate) error_code: Option<u16>,
        pub(crate) error_code_timestamp: Option<u64>,
        pub(crate) is_error_confirmable: Option<bool>,
    }

    #[derive(Debug, serde::Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct MowerAttributes {
        pub(crate) mower: Option<Mower>,
    }

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct Planner {
        pub(crate) next_start_timestamp: Option<u64>,
        pub(crate) r#override: Option<PlannerOverride>,
        pub(crate) restricted_reason: Option<String>,
        pub(crate) external_reason: Option<i32>,
    }

    #[derive(Debug, serde::Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct PlannerAttributes {
        pub(crate) planner: Option<Planner>,
    }

    #[derive(Debug, serde::Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct PositionAttributes {
        pub(crate) position: Option<Position>,
    }

    #[derive(Debug, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct ConnectionEvent {
        ready: bool,
        connection_id: String,
    }

    #[derive(Debug, serde::Deserialize)]
    #[serde(tag = "type")]
    #[serde(rename_all = "kebab-case")]
    pub(crate) enum Event {
        BatteryEventV2 {
            id: String,
            attributes: BatteryAttributes,
        },
        CalendarEventV2 {
            id: String,
            attributes: CalendarAttributes,
        },
        #[serde(rename = "cuttingHeight-event-v2")]
        CuttingHeightEventV2 {
            id: String,
            attributes: CuttingHeightAttributes,
        },
        HeadlightsEventV2 {
            id: String,
            attributes: HeadlightAttributes,
        },
        MessageEventV2 {
            id: String,
            attributes: MessageAttributes,
        },
        MowerEventV2 {
            id: String,
            attributes: MowerAttributes,
        },
        PlannerEventV2 {
            id: String,
            attributes: PlannerAttributes,
        },
        PositionEventV2 {
            id: String,
            attributes: PositionAttributes,
        },
    }
}

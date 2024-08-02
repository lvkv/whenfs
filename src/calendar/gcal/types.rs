use crate::calendar::CalendarEventDetails;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use derive_more::{Constructor, Display};
use serde::{Deserialize, Serialize};

#[derive(Constructor, Display)]
#[display(fmt = "CreateCalendar {summary}")]
pub struct CreateCalendar {
    pub summary: String,
}

#[derive(Serialize, Debug)]
pub struct CreateCalendarBody {
    pub summary: String,
}

#[derive(Deserialize)]
pub struct CreateCalendarResponse {
    pub id: String,
    pub summary: String,
}

#[derive(Constructor, Display)]
#[display(fmt = "CreateEvent {summary} {location}")]
pub struct CreateEvent {
    pub calendar_id: String,
    pub summary: String,
    pub description: String,
    pub location: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Serialize)]
pub struct CreateEventBody {
    pub summary: String,
    pub description: String,
    pub location: String,
    pub start: EventDateTime,
    pub end: EventDateTime,
}

#[derive(Deserialize, Debug)]
pub struct CreateEventResponse {
    pub id: String,
    pub summary: String,
    pub description: String,
    pub location: String,
    pub start: EventDateTime,
    pub end: EventDateTime,
}

#[derive(Constructor, Display)]
#[display(fmt = "GetEvent {event_id}")]
pub struct GetEvent {
    pub calendar_id: String,
    pub event_id: String,
}

#[derive(Deserialize, Debug)]
pub struct GetEventResponse {
    pub id: String,
    pub summary: String,
    pub description: String,
    pub location: String,
    pub start: EventDateTime,
    pub end: EventDateTime,
}

#[derive(Constructor, Display)]
#[display(fmt = "DeleteEvent {event_id}")]
pub struct DeleteEvent {
    pub calendar_id: String,
    pub event_id: String,
}

#[derive(Constructor, Display)]
#[display(fmt = "UpdateEvent {event_id} {location}")]
pub struct UpdateEvent {
    pub calendar_id: String,
    pub event_id: String,
    pub summary: String,
    pub description: String,
    pub location: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Serialize)]
pub struct UpdateEventBody {
    pub summary: String,
    pub description: String,
    pub location: String,
    pub start: EventDateTime,
    pub end: EventDateTime,
}

#[derive(Deserialize, Debug)]
pub struct UpdateEventResponse {
    pub id: String,
    pub summary: String,
    pub description: String,
    pub location: String,
    pub start: EventDateTime,
    pub end: EventDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventDateTime {
    // RFC 3339 formatted DateTime string.
    #[serde(rename = "dateTime")]
    pub date_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    // Represents an all-day event if present
    pub date: Option<String>,
    #[serde(rename = "timeZone", skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
}

#[derive(Clone, Debug)]
pub struct GCal {
    pub id: String,
}

#[derive(Clone, Hash, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub struct GCalEvent {
    pub id: String,
    pub details: CalendarEventDetails,
}

#[derive(Debug)]
pub struct Endpoint(String);

impl Endpoint {
    const BASE_URL: &'static str = "https://www.googleapis.com/calendar/v3/calendars";

    pub fn all_calendars() -> Self {
        Self(Self::BASE_URL.to_string())
    }

    pub fn calendar(id: &String) -> Self {
        Self(format!("{}/{}/events", Self::BASE_URL, id))
    }

    pub fn event(calendar_id: &String, event_id: &String) -> Self {
        Self(format!(
            "{}/{}/events/{}",
            Self::BASE_URL,
            calendar_id,
            event_id
        ))
    }
}

impl From<Endpoint> for String {
    fn from(value: Endpoint) -> Self {
        value.0
    }
}

impl<T> From<DateTime<T>> for EventDateTime
where
    T: TimeZone,
{
    fn from(date: DateTime<T>) -> Self {
        Self {
            date_time: Some(date.to_rfc3339()),
            time_zone: None,
            date: None,
        }
    }
}

impl From<EventDateTime> for DateTime<Utc> {
    fn from(event_date_time: EventDateTime) -> Self {
        if let Some(date_time_str) = &event_date_time.date_time {
            // Try parsing the date-time string. If it fails, default to current UTC date-time.
            DateTime::parse_from_rfc3339(date_time_str)
                .map(|dt_with_offset| dt_with_offset.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now())
        } else if let Some(date_str) = &event_date_time.date {
            // Interpret the date as an all-day event, starting at midnight UTC of that day.
            Utc.from_utc_datetime(&NaiveDateTime::new(
                NaiveDate::parse_from_str(date_str, "%Y-%m-%d").unwrap_or_default(),
                NaiveTime::MIN,
            ))
        } else {
            // No date-time or date provided, default to current UTC date-time.
            Utc::now()
        }
    }
}

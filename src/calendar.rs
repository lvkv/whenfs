use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{error::Error, fmt::Debug, hash::Hash, str::FromStr};

pub mod gcal;

#[async_trait(?Send)]
pub trait CalendarClient
where
    Self: Debug,
{
    type Calendar: Calendar;
    type Event: Event + DeserializeOwned + Serialize;
    type Error: Debug + Error + Sync + Send;

    async fn create_calendar(&self, name: String) -> Result<Self::Calendar, Self::Error>;

    async fn calendar_from_id(
        &self,
        id: <Self::Calendar as Calendar>::Id,
    ) -> Result<Self::Calendar, Self::Error>;

    async fn create_event(
        &self,
        calendar: &Self::Calendar,
        event: CalendarEventDetails,
    ) -> Result<Self::Event, Self::Error>;

    async fn create_events(
        &self,
        calendar: &Self::Calendar,
        events: Vec<CalendarEventDetails>,
    ) -> Result<Vec<Self::Event>, Self::Error>;

    async fn get_event_by_id(
        &self,
        calendar: &Self::Calendar,
        event_id: &<Self::Event as Event>::Id,
    ) -> Result<Self::Event, Self::Error>;

    async fn update_event(
        &self,
        calendar: &Self::Calendar,
        event_id: &<Self::Event as Event>::Id,
        details: CalendarEventDetails,
    ) -> Result<Self::Event, Self::Error>;
    async fn delete_event(
        &self,
        calendar: &Self::Calendar,
        event_id: &<Self::Event as Event>::Id,
    ) -> Result<(), Self::Error>;

    async fn close(&self);

    fn limits(&self) -> &'static CalendarLimits;
}

pub trait Event
where
    Self: Clone + Hash + Eq + Debug,
{
    type Id: From<String> + ToString + Debug + Clone;

    fn id(&self) -> &Self::Id;

    fn details(&self) -> &CalendarEventDetails;
}

pub trait Calendar
where
    Self: Clone,
{
    type Id: FromStr + ToString;

    fn id(&self) -> &Self::Id;
}

#[derive(Clone, Hash, PartialEq, Eq, Debug, Deserialize, Default, Serialize)]
pub struct CalendarEventDetails {
    pub summary: String,
    pub description: String,
    pub location: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

pub struct CalendarLimits {
    pub summary: usize,
    pub description: usize,
    pub location: usize,
}

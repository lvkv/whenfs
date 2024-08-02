use super::types::{
    CreateCalendar, CreateCalendarBody, CreateCalendarResponse, CreateEvent, CreateEventBody,
    CreateEventResponse, DeleteEvent, Endpoint, GCal, GCalEvent, GetEvent, GetEventResponse,
    UpdateEvent, UpdateEventBody, UpdateEventResponse,
};
use crate::calendar::CalendarEventDetails;
use async_trait::async_trait;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use std::fmt::Display;
use tracing::trace;

#[async_trait(?Send)]
pub trait ApiAction
where
    Self: Display,
{
    type BodyType: Serialize;
    type ResponseType: for<'de> Deserialize<'de>;
    type CalendarReturnType;

    fn endpoint(&self) -> Endpoint;

    fn method(&self) -> Method;

    fn body(self) -> Option<Self::BodyType>;

    async fn handle(response: reqwest::Response) -> Self::ResponseType {
        let response_body = response.text().await.unwrap();
        // trace!(?response_body);
        serde_json::from_str(&response_body).unwrap()
    }

    fn to_abstract(response: Self::ResponseType) -> Self::CalendarReturnType;
}

#[async_trait(?Send)]
impl ApiAction for CreateCalendar {
    type BodyType = CreateCalendarBody;
    type ResponseType = CreateCalendarResponse;
    type CalendarReturnType = GCal;

    fn endpoint(&self) -> Endpoint {
        Endpoint::all_calendars()
    }

    fn method(&self) -> Method {
        Method::POST
    }

    fn body(self) -> Option<Self::BodyType> {
        Some(Self::BodyType {
            summary: self.summary,
        })
    }

    fn to_abstract(response: Self::ResponseType) -> Self::CalendarReturnType {
        Self::CalendarReturnType { id: response.id }
    }
}

#[async_trait(?Send)]
impl ApiAction for CreateEvent {
    type BodyType = CreateEventBody;
    type ResponseType = CreateEventResponse;
    type CalendarReturnType = GCalEvent;

    fn endpoint(&self) -> Endpoint {
        Endpoint::calendar(&self.calendar_id)
    }

    fn method(&self) -> Method {
        Method::POST
    }

    fn body(self) -> Option<Self::BodyType> {
        Some(CreateEventBody {
            summary: self.summary,
            description: self.description,
            location: self.location,
            start: self.start.into(),
            end: self.end.into(),
        })
    }

    fn to_abstract(response: Self::ResponseType) -> Self::CalendarReturnType {
        Self::CalendarReturnType {
            id: response.id,
            details: CalendarEventDetails {
                summary: response.summary,
                description: response.description,
                location: response.location,
                start: response.start.into(),
                end: response.end.into(),
            },
        }
    }
}

#[async_trait(?Send)]
impl ApiAction for GetEvent {
    type BodyType = ();
    type ResponseType = GetEventResponse;
    type CalendarReturnType = GCalEvent;

    fn endpoint(&self) -> Endpoint {
        Endpoint::event(&self.calendar_id, &self.event_id)
    }

    fn method(&self) -> Method {
        Method::GET
    }

    fn body(self) -> Option<Self::BodyType> {
        None
    }

    fn to_abstract(response: Self::ResponseType) -> Self::CalendarReturnType {
        Self::CalendarReturnType {
            id: response.id,
            details: CalendarEventDetails {
                summary: response.summary,
                description: response.description,
                location: response.location,
                start: response.start.into(),
                end: response.end.into(),
            },
        }
    }
}

#[async_trait(?Send)]
impl ApiAction for DeleteEvent {
    type BodyType = ();

    type ResponseType = ();

    type CalendarReturnType = ();

    fn endpoint(&self) -> Endpoint {
        Endpoint::event(&self.calendar_id, &self.event_id)
    }

    fn method(&self) -> Method {
        Method::DELETE
    }

    fn body(self) -> Option<Self::BodyType> {
        None
    }

    async fn handle(_response: reqwest::Response) -> Self::ResponseType {}

    fn to_abstract(_response: Self::ResponseType) -> Self::CalendarReturnType {}
}

#[async_trait(?Send)]
impl ApiAction for UpdateEvent {
    type BodyType = UpdateEventBody;

    type ResponseType = UpdateEventResponse;

    type CalendarReturnType = GCalEvent;

    fn endpoint(&self) -> Endpoint {
        Endpoint::event(&self.calendar_id, &self.event_id)
    }

    fn method(&self) -> Method {
        Method::PUT
    }

    fn body(self) -> Option<Self::BodyType> {
        Some(UpdateEventBody {
            summary: self.summary,
            description: self.description,
            location: self.location,
            start: self.start.into(),
            end: self.end.into(),
        })
    }

    fn to_abstract(response: Self::ResponseType) -> Self::CalendarReturnType {
        Self::CalendarReturnType {
            id: response.id,
            details: CalendarEventDetails {
                summary: response.summary,
                description: response.description,
                location: response.location,
                start: response.start.into(),
                end: response.end.into(),
            },
        }
    }
}

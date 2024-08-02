use self::{
    api::ApiAction,
    types::{CreateCalendar, CreateEvent, DeleteEvent, Endpoint, GCal, GCalEvent, UpdateEvent},
};
use super::{Calendar, CalendarClient, CalendarEventDetails, CalendarLimits, Event};
use crate::calendar::gcal::types::GetEvent;
use async_trait::async_trait;
use futures::future::join_all;
use reqwest::{Method, Response};
use serde::Serialize;
use std::path::PathBuf;
use thiserror::Error;
use tracing::{debug, trace};

pub mod api;
pub mod types;

#[derive(Debug, Error)]
pub enum GCalError {
    #[error("HTTP client error: {0}")]
    HttpClient(#[from] reqwest::Error),
    #[error("Response deserialization error: {0}")]
    JoinError(#[from] tokio::task::JoinError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("OAuth: {0}")]
    Oauth(#[from] yup_oauth2::Error),
    #[error("Unknown error: {0}")]
    Unknown(&'static str),
}

#[derive(Debug)]
pub struct GCalClient {
    access_token: String,
    client: reqwest::Client,
}

static LIMITS: CalendarLimits = CalendarLimits {
    summary: 512,
    description: 4096,
    location: 512,
};

impl GCalClient {
    pub async fn new(secret_path: PathBuf) -> Result<Self, GCalError> {
        let client = reqwest::Client::new();
        let secret = yup_oauth2::read_application_secret(secret_path).await?;
        let auth = yup_oauth2::InstalledFlowAuthenticator::builder(
            secret,
            yup_oauth2::InstalledFlowReturnMethod::HTTPRedirect,
        )
        .persist_tokens_to_disk("token_cache.json")
        .build()
        .await?;
        let scopes = &["https://www.googleapis.com/auth/calendar.app.created"];
        let access_token = auth
            .token(scopes)
            .await?
            .token()
            .ok_or(GCalError::Unknown("Failed to extract OAuth access token"))?
            .to_string();
        Ok(Self {
            access_token,
            client,
        })
    }

    pub async fn execute_request<Body>(
        &self,
        endpoint: Endpoint,
        method: Method,
        body: Option<Body>,
    ) -> Result<Response, GCalError>
    where
        Body: Serialize,
    {
        let mut request = self
            .client
            .request(method, String::from(endpoint))
            .bearer_auth(&self.access_token);

        if let Some(body) = body {
            request = request.json(&body);
        }

        trace!(?request, "Sending Google Calendar API request");
        let response = request.send().await?;
        trace!("Received Google Calendar API response");
        // trace!(?response, "Received Google Calendar API response");
        Ok(response)
    }

    async fn execute_api_action<Action: ApiAction>(
        &self,
        action: Action,
    ) -> Result<Action::ResponseType, GCalError> {
        debug!(%action, "Executing Google Calendar API action");
        let handled = Action::handle(
            self.execute_request(action.endpoint(), action.method(), action.body())
                .await?,
        )
        .await;
        Ok(handled)
    }
}

#[async_trait(?Send)]
impl CalendarClient for GCalClient {
    type Calendar = GCal;
    type Event = GCalEvent;
    type Error = GCalError;

    async fn create_calendar(&self, name: String) -> Result<Self::Calendar, Self::Error> {
        let action = CreateCalendar::new(name);
        let calendar = self.execute_api_action(action).await?;
        Ok(CreateCalendar::to_abstract(calendar))
    }

    async fn calendar_from_id(
        &self,
        id: <Self::Calendar as Calendar>::Id,
    ) -> Result<Self::Calendar, Self::Error> {
        Ok(Self::Calendar { id })
    }

    async fn create_event(
        &self,
        calendar: &Self::Calendar,
        event: CalendarEventDetails,
    ) -> Result<Self::Event, Self::Error> {
        let action = CreateEvent::new(
            calendar.id.clone(),
            event.summary,
            event.description,
            event.location,
            event.start,
            event.end,
        );
        let event = self.execute_api_action(action).await?;
        Ok(CreateEvent::to_abstract(event))
    }

    async fn create_events(
        &self,
        calendar: &Self::Calendar,
        events: Vec<CalendarEventDetails>,
    ) -> Result<Vec<Self::Event>, Self::Error> {
        Ok(join_all(
            events
                .into_iter()
                .map(|event| self.create_event(calendar, event)),
        )
        .await
        .into_iter()
        .map(|result| result.unwrap())
        .collect())
    }

    /// Fetches the CalendarEvent with the given CalendarEventID
    async fn get_event_by_id(
        &self,
        calendar: &Self::Calendar,
        event_id: &<Self::Event as Event>::Id,
    ) -> Result<Self::Event, Self::Error> {
        let action = GetEvent::new(calendar.id.clone(), event_id.clone());
        let event = self.execute_api_action(action).await?;
        Ok(GetEvent::to_abstract(event))
    }

    async fn update_event(
        &self,
        calendar: &Self::Calendar,
        event_id: &<Self::Event as Event>::Id,
        details: CalendarEventDetails,
    ) -> Result<Self::Event, Self::Error> {
        let action = UpdateEvent::new(
            calendar.id.clone(),
            event_id.clone(),
            details.summary,
            details.description,
            details.location,
            details.start,
            details.end,
        );
        let updated = self.execute_api_action(action).await?;
        Ok(UpdateEvent::to_abstract(updated))
    }

    async fn delete_event(
        &self,
        calendar: &Self::Calendar,
        event_id: &<Self::Event as Event>::Id,
    ) -> Result<(), Self::Error> {
        let action = DeleteEvent::new(calendar.id.clone(), event_id.clone());
        let deleted = self.execute_api_action(action).await?;
        Ok(DeleteEvent::to_abstract(deleted))
    }

    async fn close(&self) {}

    fn limits(&self) -> &'static CalendarLimits {
        &LIMITS
    }
}

impl Event for GCalEvent {
    type Id = String;

    fn id(&self) -> &Self::Id {
        &self.id
    }

    fn details(&self) -> &CalendarEventDetails {
        &self.details
    }
}

impl Calendar for GCal {
    type Id = String;

    fn id(&self) -> &Self::Id {
        &self.id
    }
}

#[cfg(test)]
mod tests {
    use super::GCalClient;
    use crate::{
        calendar::{CalendarClient, CalendarEventDetails},
        LOGGER,
    };
    use chrono::{Duration, Utc};
    use ctor::ctor;
    use tracing::info;

    #[ctor]
    fn init() {
        let _ = &*LOGGER;
    }

    #[tokio::test]
    #[ignore = "Google Calendar API calls are expensive"]
    async fn google_calendar_client_end_to_end() {
        // Create client
        info!("Creating GCalClient");
        let client = GCalClient::new("secret.json".into()).await.unwrap();

        // Create calendar
        info!("Creating new calendar");
        let calendar = client.create_calendar("WhenFS".to_string()).await.unwrap();

        // Create single event
        info!("Creating new event");
        let now = Utc::now();
        let later = now.checked_add_signed(Duration::minutes(30)).unwrap();
        let mut details = CalendarEventDetails {
            summary: "hello world".to_string(),
            description: "description".to_string(),
            location: "location".to_string(),
            start: now,
            end: later,
        };
        let created = client
            .create_event(&calendar, details.clone())
            .await
            .unwrap();
        assert_eq!(&created.details.summary, &details.summary);
        assert_eq!(&created.details.location, &details.location);

        // Get created event
        info!("Getting created event");
        let foo = client
            .get_event_by_id(&calendar, &created.id)
            .await
            .unwrap();

        // Create multiple events
        info!("Creating multiple events");
        let mut events: Vec<CalendarEventDetails> = Vec::new();
        for offset in [60, 70, 80] {
            let start = now.checked_add_signed(Duration::minutes(offset)).unwrap();
            let end = start.checked_add_signed(Duration::minutes(5)).unwrap();
            let mut event = details.clone();
            event.start = start;
            event.end = end;
            events.push(event);
        }
        client.create_events(&calendar, events).await.unwrap();

        // Update Event
        info!("Updating event");
        details.summary = "Updated Summary".to_string();
        let foo = client
            .update_event(&calendar, &foo.id, details)
            .await
            .unwrap();

        // Delete event
        info!("Deleting event");
        client.delete_event(&calendar, &foo.id).await.unwrap();
    }
}

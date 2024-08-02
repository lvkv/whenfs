use crate::calendar::{Calendar, CalendarClient, CalendarEventDetails, Event};
use async_trait::async_trait;
use serde::Deserialize;
use serde::{de::DeserializeOwned, Serialize};
use std::collections::VecDeque;
use std::{fmt::Debug, hash::Hash};
use thiserror::Error;
use tracing::{debug, trace};

#[async_trait(?Send)]
pub trait Store {
    type Entry: Eq + Hash + Clone + DeserializeOwned + Serialize;
    type Error: Debug + Send + Sync + std::error::Error;

    async fn store<T: Serialize>(&self, item: &T, name: String)
        -> Result<Self::Entry, Self::Error>;

    async fn retrieve<T: DeserializeOwned>(&self, id: Self::Entry) -> Result<T, Self::Error>;

    async fn update<T: Serialize>(
        &self,
        old: Self::Entry,
        new: &T,
    ) -> Result<Self::Entry, Self::Error>;

    async fn delete(&self, item: Self::Entry) -> Result<(), Self::Error>;

    fn get_raw_id(&self, entry: &Self::Entry) -> RecoveryDetails;
}

#[derive(Debug)]
pub struct CalStore<TCalendarClient: CalendarClient> {
    client: TCalendarClient,
    calendar: TCalendarClient::Calendar,
}

#[derive(Error, Debug)]
pub enum CalStoreError<T: CalendarClient> {
    #[error("Encode/Decode error: {0}")]
    EncodeDecode(#[from] encoding::EncodingError),
    #[error("Calendar error: {0}")]
    Calendar(<T as CalendarClient>::Error),
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, Deserialize, Serialize)]
pub struct CalStoreEntry<TEvent: Event> {
    pub name: String,
    pub events: Vec<TEvent>,
}

#[async_trait(?Send)]
impl<TCalendarClient: CalendarClient> Store for CalStore<TCalendarClient> {
    type Entry = CalStoreEntry<TCalendarClient::Event>;
    type Error = CalStoreError<TCalendarClient>;

    async fn store<T: Serialize>(
        &self,
        item: &T,
        name: String,
    ) -> Result<Self::Entry, Self::Error> {
        debug!(%name, "Base64 encoding item for storage");
        let encoded = encoding::encode(item)?;
        debug!(%name, size_bytes = encoded.len(), "Base64 encoded item");
        let split = zip::split(&encoded, self.client.limits().description);
        debug!(
            %name,
            number_of_chunks = split.len(),
            chunk_size_bytes = self.client.limits().description,
            "Split encoded data up into chunks"
        );
        debug!(%name, "Converting split encoded data into calendar events");
        let calendarized = calendarize::calendarize(split);
        debug!(%name, "Uploading calendar events");
        let events = self.upload(calendarized, name.clone()).await?;
        Ok(Self::Entry { name, events })
    }

    async fn retrieve<T: DeserializeOwned>(&self, entry: Self::Entry) -> Result<T, Self::Error> {
        let CalStoreEntry { name, events } = entry;
        let tail_event = events.last().unwrap();
        debug!(
            ?name,
            tail_event_id = ?tail_event.id(),
            "Downloading calendar events"
        );
        let events = self.download(tail_event.id().clone(), name.clone()).await?;
        debug!(
            ?name,
            number_of_events = events.len(),
            "Downloaded calendar events"
        );
        let details = events
            .iter()
            .map(|event| event.details().clone())
            .collect::<Vec<_>>();
        debug!(?name, "Collected calendar event details");
        let uncalendarized = calendarize::uncalendarize(details);
        debug!(
            ?name,
            number_of_chunks = uncalendarized.len(),
            "Condensed calendar event details into workable data chunks"
        );
        let zipped = zip::zip(uncalendarized);
        debug!(
            ?name,
            "Zipped event data chunks back into contiguous memory"
        );
        let decoded: T = encoding::decode(&zipped)?;
        debug!(?name, "Base64-decoded data back into original item");
        Ok(decoded)
    }

    async fn update<T: Serialize>(
        &self,
        old: Self::Entry,
        new: &T,
    ) -> Result<Self::Entry, Self::Error> {
        let new = self.store(&new, old.name).await?;
        Ok(new)
    }

    async fn delete(&self, item: Self::Entry) -> Result<(), Self::Error> {
        todo!()
    }

    fn get_raw_id(&self, entry: &Self::Entry) -> RecoveryDetails {
        let first = entry.events.last().unwrap();
        let root_id = first.id().to_string();
        let cal_id = self.calendar.id().to_string();
        RecoveryDetails { cal_id, root_id }
    }
}

pub struct RecoveryDetails {
    pub cal_id: String,
    pub root_id: String,
}

impl<TCalendarClient: CalendarClient> CalStore<TCalendarClient> {
    pub fn new(client: TCalendarClient, calendar: TCalendarClient::Calendar) -> Self {
        Self { client, calendar }
    }

    async fn upload(
        &self,
        details: Vec<CalendarEventDetails>,
        sentinel: String,
    ) -> Result<Vec<TCalendarClient::Event>, CalStoreError<TCalendarClient>> {
        let mut events: Vec<TCalendarClient::Event> = Vec::new();
        let mut prev = sentinel;
        for mut detail in details {
            detail.summary = prev.to_string();
            let event = self
                .client
                .create_event(&self.calendar, detail)
                .await
                .map_err(CalStoreError::Calendar)?;
            prev = event.id().to_string();
            events.push(event);
        }
        Ok(events)
    }

    async fn download(
        &self,
        tail_event_id: <TCalendarClient::Event as Event>::Id,
        sentinel: String,
    ) -> Result<Vec<TCalendarClient::Event>, CalStoreError<TCalendarClient>> {
        trace!(%sentinel, "Downloading event string");
        let mut events: VecDeque<TCalendarClient::Event> = VecDeque::new();
        let mut id = tail_event_id.to_string();
        while id != sentinel {
            trace!(%id, "Downloading event");
            let event = self
                .client
                .get_event_by_id(&self.calendar, &id.clone().into())
                .await
                .map_err(CalStoreError::Calendar)?;
            id.clone_from(&event.details().summary);
            trace!("Next event ID is {id}");
            // if id == "root event" {
            //     break;
            // }
            events.push_front(event);
        }
        Ok(events.into())
    }
}

mod calendarize {
    use crate::calendar::CalendarEventDetails;
    use chrono::{Duration, Utc};
    pub fn calendarize(data: Vec<String>) -> Vec<CalendarEventDetails> {
        let now = Utc::now();
        data.into_iter()
            .enumerate()
            .map(|(i, datum)| CalendarEventDetails {
                summary: String::new(),
                description: datum,
                location: i.to_string(),
                start: now + Duration::minutes(i as i64 * 5),
                end: now + Duration::minutes(i as i64 * 5 + 5),
            })
            .collect()
    }

    pub fn uncalendarize(events: Vec<CalendarEventDetails>) -> Vec<String> {
        events
            .into_iter()
            .map(|details| details.description)
            .collect()
    }

    #[cfg(test)]
    pub mod tests {
        #[test]
        fn test_calendarize() {
            let source = vec![
                "The", "quick", "brown", "fox", "jumped", "over", "the", "lazy", "dog",
            ];
            let data: Vec<String> = source.iter().map(ToString::to_string).collect();
            let calendarized = super::calendarize(data);
            let uncalendarized = super::uncalendarize(calendarized);
            let _ = uncalendarized
                .into_iter()
                .zip(source)
                .for_each(|(expected, actual)| assert_eq!(expected, actual));
        }
    }
}

mod zip {
    pub fn split(data: &str, chunk_size: usize) -> Vec<String> {
        data.as_bytes()
            .chunks(chunk_size)
            .map(|chunk| unsafe { std::str::from_utf8_unchecked(chunk) })
            .map(ToString::to_string)
            .collect()
    }

    pub fn zip(strings: Vec<String>) -> String {
        strings.join("")
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn test_split_zip() {
            let data = "The quick brown fox jumped over the lazy dog".to_string();
            let split = super::split(&data, 4);
            let zip = super::zip(split);
            assert_eq!(data, zip)
        }
    }
}

mod encoding {
    use base64::Engine;
    use serde::{de::DeserializeOwned, Serialize};
    use thiserror::Error;

    #[derive(Error, Debug)]
    pub enum EncodingError {
        #[error("JSON byte vector encoding error: {0}")]
        JsonEncode(serde_json::Error),
        #[error("JSON byte vector decoding error: {0}")]
        JsonDecode(serde_json::Error),
        #[error("Base64 decoding error: {0}")]
        Base64Decode(#[from] base64::DecodeError),
    }

    pub fn encode<T: Serialize>(data: &T) -> Result<String, EncodingError> {
        use base64::Engine as _;
        let json = serde_json::to_vec(&data).map_err(EncodingError::JsonEncode)?;
        let b64 = base64::engine::general_purpose::URL_SAFE.encode(json);
        Ok(b64)
    }

    pub fn decode<'de, T: DeserializeOwned>(b64: &str) -> Result<T, EncodingError> {
        let json = base64::engine::general_purpose::URL_SAFE.decode(b64)?;
        let data: T = serde_json::from_slice(&json).map_err(EncodingError::JsonDecode)?;
        Ok(data)
    }

    #[cfg(test)]
    mod tests {
        use serde::{Deserialize, Serialize};

        use crate::store::encoding::{decode, encode};

        #[test]
        fn test_encode_decode() {
            #[derive(Serialize, Deserialize, Clone)]
            struct MyThing {
                foo: String,
                bar: u64,
                baz: Vec<u8>,
            }

            let my_thing = MyThing {
                foo: "foo".into(),
                bar: u64::MAX,
                baz: vec![1, 2, 3, 4, 5],
            };

            let expected = my_thing.clone();
            let encoded = encode(&my_thing).unwrap();
            let decoded: MyThing = decode(&encoded).unwrap();

            assert_eq!(expected.foo, decoded.foo);
            assert_eq!(expected.bar, decoded.bar);
            assert_eq!(expected.baz, decoded.baz);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        calendar::{gcal::GCalClient, CalendarClient},
        store::{CalStore, Store},
    };
    use serde::{Deserialize, Serialize};
    use tracing::info;

    #[tokio::test]
    async fn test_block_device_end_to_end() {
        #[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
        struct MyStruct {
            foo: String,
        }

        // Create client
        info!("Creating GCalClient");
        let client = GCalClient::new("secret.json".into()).await.unwrap();

        // Create calendar
        info!("Creating new calendar");
        let calendar = client.create_calendar("WhenFS".to_string()).await.unwrap();

        // Create block layer
        let block_layer = CalStore::new(client, calendar);

        let item = MyStruct {
            foo: "Lorem ipsum dolor sit amet, consectetur adipiscing elit. Proin varius justo a vehicula vulputate. Vestibulum auctor, nunc eu euismod hendrerit, neque mauris ultricies ligula, eget lacinia nulla nisi nec enim. Cras sit amet orci a odio suscipit maximus non et tortor. Sed blandit luctus dolor, sed porta mauris tempor non. Sed in cursus leo, eget posuere massa. Sed vitae nulla ut lectus malesuada iaculis a ut dolor. Maecenas in accumsan est. Donec convallis ipsum risus, ut egestas sem ultricies eget. Donec consectetur, purus non facilisis condimentum, lacus erat facilisis velit, a tincidunt massa neque sit amet diam. Sed at finibus tellus.
            Integer eu metus iaculis, auctor felis eget, sollicitudin ante. Phasellus posuere nulla vitae felis pulvinar sollicitudin at aliquam est. Integer sit amet ex eros. Proin vel sapien eros. Nullam quis nisl lectus. Ut auctor, mauris quis efficitur elementum, risus neque porta dolor, quis laoreet nisi purus ut nibh. Mauris aliquet dolor ex, ac mattis neque gravida eu. Sed auctor a nisl ut venenatis. Vestibulum egestas turpis sed libero venenatis dignissim. Orci varius natoque penatibus et magnis dis parturient montes, nascetur ridiculus mus.
            Donec non dignissim magna. Sed eu quam magna. Donec posuere lacus libero, ultricies cursus ligula vehicula quis. Morbi varius rhoncus mollis. Praesent quis pharetra velit. Curabitur eget condimentum massa, a aliquet orci. Nullam vitae dui sed erat convallis maximus et vel augue. Aenean vestibulum dolor eget lobortis feugiat. Pellentesque viverra cursus sem ac posuere. Fusce commodo ante in placerat rutrum. Nullam fermentum risus non ipsum posuere volutpat nec id dolor. In nunc sapien, convallis eget luctus sit amet, hendrerit at nunc. Donec rhoncus sagittis justo ac sagittis. Integer id sapien commodo diam tempus elementum ut ac massa.
            Integer facilisis tellus non dui posuere eleifend. Ut posuere ligula pulvinar, dapibus elit nec, condimentum sem. Sed nisl augue, mattis at enim quis, venenatis rhoncus est. Quisque sollicitudin lacus a ante rutrum, sit amet consectetur mi malesuada. Pellentesque tincidunt porttitor ultricies. Vestibulum quis cursus libero. Quisque elementum volutpat tempor. Donec tempor efficitur hendrerit. Proin nisl est, suscipit in orci nec, consectetur ornare sem. Praesent tempus congue velit eget tristique. Vivamus eu nibh interdum, hendrerit neque quis, viverra neque.
            Fusce sit amet pretium ipsum. Proin vitae quam eros. Praesent aliquam tortor vitae lacus imperdiet, vel eleifend felis volutpat. Donec mattis eget tellus id commodo. Aenean scelerisque, odio vitae aliquam convallis, ligula tortor ullamcorper nibh, sit amet convallis nibh sapien id sapien. Donec at molestie ante, vel molestie mauris. Phasellus mollis condimentum odio, eu luctus nulla lobortis non. Nam dapibus tortor eget ante maximus elementum. Orci varius natoque penatibus et magnis dis parturient montes, nascetur ridiculus mus. Morbi vehicula molestie sem in eleifend. Nulla elementum nisl velit, euismod suscipit ante laoreet vel. Fusce ullamcorper fermentum tempus. Nunc quam purus, convallis vitae lectus eu, ullamcorper iaculis libero. Fusce magna metus, tristique et sapien ut, viverra mollis ante. Donec est nibh, fringilla a diam sit amet, iaculis placerat leo. Sed iaculis massa at ipsum cursus, in sodales ligula cursus.
            ".into(),
        };
        let foo = block_layer.store(&item, "dog.txt".into()).await.unwrap();
        let bar = block_layer.retrieve::<MyStruct>(foo).await.unwrap();
        assert_eq!(item, bar)
    }
}

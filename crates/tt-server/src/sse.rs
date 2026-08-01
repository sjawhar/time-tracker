use std::convert::Infallible;
use std::time::Duration;

use axum::response::sse::{Event, Sse};
use futures_util::stream::{self, Stream};
use tokio::sync::broadcast;

use crate::ServerEvent;

pub fn response(
    receiver: broadcast::Receiver<ServerEvent>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(30),
        Duration::from_secs(30),
    );
    let stream = stream::unfold(
        (receiver, heartbeat),
        |(mut receiver, mut heartbeat)| async move {
            let event = tokio::select! {
                result = receiver.recv() => match result {
                Ok(event) => event_from_notification(&event),
                    Err(broadcast::error::RecvError::Lagged(_)) => resync_event(),
                    Err(broadcast::error::RecvError::Closed) => return None,
                },
                _ = heartbeat.tick() => heartbeat_event(),
            };
            Some((Ok(event), (receiver, heartbeat)))
        },
    );
    Sse::new(stream)
}

fn event_from_notification(notification: &ServerEvent) -> Event {
    match notification {
        ServerEvent::EventsAppended { count } => Event::default()
            .event("events_appended")
            .data(format!(r#"{{"count":{count}}}"#, count = *count)),
        ServerEvent::StatusChanged => Event::default().event("status_changed").data("{}"),
    }
}

fn resync_event() -> Event {
    Event::default().event("resync_required").data("{}")
}

fn heartbeat_event() -> Event {
    Event::default().event("heartbeat").data("{}")
}

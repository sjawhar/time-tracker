pub mod api;
pub mod loops;
pub mod sse;
pub mod web;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerEvent {
    EventsAppended { count: u64 },
    StatusChanged,
}

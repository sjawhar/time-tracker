use anyhow::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveWindow {
    pub app_id: String,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleState {
    Active,
    Idle { since_ms: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub active: Option<ActiveWindow>,
    pub idle: IdleState,
}

pub trait WindowBackend {
    fn poll(&mut self) -> Result<Snapshot>;
}

#[cfg(test)]
pub struct FakeBackend {
    scripted: std::collections::VecDeque<Snapshot>,
}

#[cfg(test)]
impl FakeBackend {
    pub fn new(scripted: Vec<Snapshot>) -> Self {
        Self {
            scripted: scripted.into(),
        }
    }
}

#[cfg(test)]
impl WindowBackend for FakeBackend {
    fn poll(&mut self) -> Result<Snapshot> {
        self.scripted
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("no more snapshots"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_backend_yields_scripted_snapshots() {
        let mut backend = FakeBackend::new(vec![
            Snapshot {
                active: Some(ActiveWindow {
                    app_id: "firefox".into(),
                    title: "Docs".into(),
                }),
                idle: IdleState::Active,
            },
            Snapshot {
                active: None,
                idle: IdleState::Idle { since_ms: 180_000 },
            },
        ]);

        assert_eq!(
            backend.poll().expect("first snapshot").idle,
            IdleState::Active
        );
        assert!(matches!(
            backend.poll().expect("second snapshot").idle,
            IdleState::Idle { since_ms: 180_000 }
        ));
        assert!(backend.poll().is_err());
    }
}

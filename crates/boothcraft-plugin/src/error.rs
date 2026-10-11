use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq)]
pub enum HostError {
    #[error("could not load plugin: {0}")]
    Load(String),
    #[error("plugin does not support this: {0}")]
    Unsupported(String),
    #[error("plugin state: {0}")]
    State(String),
    #[error("could not activate plugin: {0}")]
    Activate(String),
    #[error("{0}")]
    Other(String),
}

impl From<HostError> for boothcraft_dsp::SlotError {
    fn from(e: HostError) -> Self {
        boothcraft_dsp::SlotError::Failed(e.to_string())
    }
}

//! Module implementing image captioning.

mod engine;
mod error;
mod output;
mod task;


pub use self::engine::{Builder as EngineBuilder,
                       BuildError as EngineBuildError,
                       Config as EngineConfig,
                       ConfigError as EngineConfigError,
                       Engine};
pub use self::error::CaptionError;
pub use self::output::CaptionOutput;

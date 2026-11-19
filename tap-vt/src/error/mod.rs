use crate::config::ConfigError;
#[cfg(feature = "renderer")]
use crate::sugarloaf::font::SugarloafFont;

#[derive(Clone, Copy, PartialEq)]
pub enum TapErrorLevel {
    Warning,
    Error,
}

#[derive(Clone)]
pub struct TapError {
    pub report: TapErrorType,
    pub level: TapErrorLevel,
}

impl TapError {
    pub fn configuration_not_found() -> Self {
        TapError {
            level: TapErrorLevel::Warning,
            report: TapErrorType::ConfigurationNotFound,
        }
    }
}

impl From<ConfigError> for TapError {
    fn from(error: ConfigError) -> Self {
        match error {
            ConfigError::ErrLoadingConfig(message) => TapError {
                report: TapErrorType::InvalidConfigurationFormat(message),
                level: TapErrorLevel::Warning,
            },
            ConfigError::ErrLoadingTheme(message) => TapError {
                report: TapErrorType::InvalidConfigurationTheme(message),
                level: TapErrorLevel::Warning,
            },
            ConfigError::PathNotFound => TapError {
                report: TapErrorType::ConfigurationNotFound,
                level: TapErrorLevel::Warning,
            },
        }
    }
}

#[derive(Clone, PartialEq)]
pub enum TapErrorType {
    // font was not found
    #[cfg(feature = "renderer")]
    FontsNotFound(Vec<SugarloafFont>),

    // navigation configuration has changed
    // NavigationHasChanged,
    InitializationError(String),

    // configurlation file was not found
    ConfigurationNotFound,
    // configuration file have an invalid format
    InvalidConfigurationFormat(String),
    // configuration invalid theme
    InvalidConfigurationTheme(String),

    // background image referenced in config could not be loaded
    BackgroundImageLoadFailure(String),

    // reports that are ignored by TapErrorType
    IgnoredReport,
}

impl std::fmt::Display for TapErrorType {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            #[cfg(feature = "renderer")]
            TapErrorType::FontsNotFound(fonts) => {
                let mut font_str = String::from("");
                for font in fonts.iter() {
                    font_str += format!("\n• \"{}\"", font.family).as_str();
                }

                write!(f, "Font(s) not found:\n{font_str}")
            }
            TapErrorType::ConfigurationNotFound => {
                write!(f, "Configuration file was not found")
            }
            // TapErrorType::NavigationHasChanged => {
            //     write!(f, "Navigation has changed\n\nPlease reopen Tap terminal.")
            // }
            TapErrorType::InitializationError(message) => {
                write!(f, "Error initializing Tap terminal:\n{message}")
            }
            TapErrorType::IgnoredReport => write!(f, ""),
            TapErrorType::InvalidConfigurationFormat(message) => {
                write!(f, "Found an issue loading the configuration file:\n\n{message}\n\nTap will proceed with the default configuration")
            }
            TapErrorType::InvalidConfigurationTheme(message) => {
                write!(f, "Found an issue in the configured theme:\n\n{message}")
            }
            TapErrorType::BackgroundImageLoadFailure(message) => {
                write!(
                    f,
                    "Could not load the configured background image:\n\n{message}\n\nCheck `window.background-image.path` in your config."
                )
            }
        }
    }
}

//! Functions: parameterized sequences of Minecraft commands (JSON or `.mcfunction`).
//! Same syntax and limits as the plugin; the engine advances by ticks instead of by threads.
pub mod compile;
pub mod engine;
pub mod model;
pub mod params;
pub mod parse;
pub mod render;

/// A function error with the same short code the plugin uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnError {
    pub message: String,
    pub code: &'static str,
}

impl FnError {
    pub fn new(message: impl Into<String>, code: &'static str) -> FnError {
        FnError { message: message.into(), code }
    }
}

impl std::fmt::Display for FnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

pub type FnResult<T> = Result<T, FnError>;

/// `namespace:path` with the characters the plugin admits.
pub fn valid_function_id(id: &str) -> bool {
    let Some((namespace, path)) = id.split_once(':') else {
        return false;
    };
    !namespace.is_empty()
        && !path.is_empty()
        && namespace.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'))
        && path.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '/' | '.' | '-'))
}

/// A function's limits (`FunctionLimits`).
#[derive(Debug, Clone)]
pub struct Limits {
    pub max_expanded_commands: usize,
    pub max_loop_iterations: usize,
    pub max_nesting_depth: usize,
    pub max_delay_ticks: u32,
    pub max_execution_seconds: u64,
    pub max_file_bytes: usize,
    pub max_args: usize,
}

impl Limits {
    pub fn from_config(config: &crate::config::Config) -> Limits {
        Limits {
            max_expanded_commands: 256,
            max_loop_iterations: config.max_repeat as usize,
            max_nesting_depth: config.max_depth as usize,
            max_delay_ticks: 1200,
            max_execution_seconds: config.max_execution_seconds as u64,
            max_file_bytes: 256 * 1024,
            max_args: 32,
        }
    }
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_expanded_commands: 256,
            max_loop_iterations: 100,
            max_nesting_depth: 8,
            max_delay_ticks: 1200,
            max_execution_seconds: 120,
            max_file_bytes: 256 * 1024,
            max_args: 32,
        }
    }
}

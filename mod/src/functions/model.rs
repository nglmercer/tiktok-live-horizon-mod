//! Already parsed function definitions.
use crate::value::Val;

pub const TYPES: [&str; 10] =
    ["string", "integer", "number", "boolean", "entity", "player", "selector", "position", "enum", "resource_location"];

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ptype: String,
    pub required: bool,
    pub default: Option<Val>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub values: Vec<String>,
    #[allow(dead_code)] // reserved: part of the configuration's API/semantics
    pub description: String,
}

#[derive(Debug, Clone)]
pub enum Step {
    Command(String),
    Summon { entity: String, position: String, nbt: String },
    Title { target: String, title: String, subtitle: String, fade_in: String, stay: String, fade_out: String },
    Sound { sound: String, source: String, target: String, volume: String, pitch: String, position: String },
    Particle { particle: String, position: String, count: String },
    Repeat { times: String, steps: Vec<Step> },
    Delay(String),
    Call { name: String, args: Vec<(String, String)> },
    If { parameter: String, operator: String, value: String, then: Vec<Step>, otherwise: Vec<Step> },
}

impl Step {
    pub fn type_name(&self) -> &'static str {
        match self {
            Step::Command(_) => "command",
            Step::Summon { .. } => "summon",
            Step::Title { .. } => "title",
            Step::Sound { .. } => "sound",
            Step::Particle { .. } => "particle",
            Step::Repeat { .. } => "repeat",
            Step::Delay(_) => "delay",
            Step::Call { .. } => "function",
            Step::If { .. } => "if",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
    McFunction,
}

#[derive(Debug, Clone)]
pub struct Definition {
    pub id: String,
    #[allow(dead_code)] // reserved: part of the configuration's API/semantics
    pub description: String,
    pub params: Vec<Param>,
    pub steps: Vec<Step>,
    #[allow(dead_code)] // reserved: part of the configuration's API/semantics
    pub format: Format,
    #[allow(dead_code)] // reserved
    pub origin: String,
}

impl Definition {
    #[allow(dead_code)] // reserved: part of the configuration's API/semantics
    pub fn param(&self, name: &str) -> Option<&Param> {
        self.params.iter().find(|p| p.name == name)
    }
}

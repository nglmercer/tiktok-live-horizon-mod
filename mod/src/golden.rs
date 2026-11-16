//! "Golden" tests: the Paper plugin's examples (`golden/`) must load unchanged.

use crate::actions::Catalog;
use crate::config::{Config, DEFAULT_CONFIG};
use crate::functions::parse::{parse_json, parse_mcfunction};
use crate::rules::loader::load_rules;

const RULE_FILES: [(&str, &str); 5] = [
    ("chat.yml", include_str!("golden/chat.yml")),
    ("finger-heart-lightning.yml", include_str!("golden/finger-heart-lightning.yml")),
    ("gifts.yml", include_str!("golden/gifts.yml")),
    ("likes.yml", include_str!("golden/likes.yml")),
    ("obsidian-cage.yml", include_str!("golden/obsidian-cage.yml")),
];

/// (id the file declares, contents)
const FUNCTION_FILES: [(&str, &str); 3] = [
    ("gifts:spawn_chicken", include_str!("golden/functions/spawn_chicken.json")),
    ("gifts:spawn_zombies", include_str!("golden/functions/spawn_zombies.json")),
    ("effects:welcome", include_str!("golden/functions/welcome.json")),
];

#[test]
fn every_example_rule_file_loads() {
    let config = Config::parse(DEFAULT_CONFIG).expect("default config parses");
    let catalog = Catalog { config: &config, functions: None };
    for (file, yaml) in RULE_FILES {
        let rules = load_rules(yaml, file, &catalog).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert!(!rules.is_empty(), "{file} produced no rules");
    }
}

#[test]
fn every_example_function_loads() {
    for (id, json) in FUNCTION_FILES {
        let definition = parse_json("fallback:id", json, id).unwrap_or_else(|e| panic!("{id}: {e}"));
        assert_eq!(definition.id, id);
        assert!(!definition.steps.is_empty(), "{id} has no steps");
    }
}

#[test]
fn an_mcfunction_file_takes_its_id_from_the_path() {
    let text = "# welcome\nsay Hello\nsay Bye\n";
    let definition = parse_mcfunction("minecraft:hello", text, "hello.mcfunction").expect("parses");
    assert_eq!(definition.id, "minecraft:hello");
    assert!(definition.steps.len() >= 2);
}

// --- deterministic fuzzing -------------------------------------------------------------
//
// Text that arrives from hand-edited files (and, through TikTok, names and comments)
// must never make any parser panic: a panic on the server thread takes it down.

struct Xorshift(u64);

impl Xorshift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

const NOISE: &[&str] = &["\n", "  ", ":", "-", "{", "}", "[", "]", "\"", "'", "#", "%", "$", "~", "\\", "\u{0}", "é", "😀", "9999999999999999999", "-1", "null", "true", "&", "|", "(", ")"];

/// Mutates the text: deletes a span, inserts noise, duplicates a span or truncates it.
fn mutate(rng: &mut Xorshift, text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return NOISE[rng.below(NOISE.len())].to_owned();
    }
    let mut out = chars.clone();
    for _ in 0..1 + rng.below(6) {
        let at = rng.below(out.len().max(1));
        match rng.below(4) {
            0 => {
                let end = (at + 1 + rng.below(40)).min(out.len());
                out.drain(at.min(out.len())..end);
            }
            1 => {
                let noise: Vec<char> = NOISE[rng.below(NOISE.len())].chars().collect();
                let at = at.min(out.len());
                out.splice(at..at, noise);
            }
            2 => {
                let end = (at + 1 + rng.below(60)).min(out.len());
                let piece: Vec<char> = out[at.min(end)..end].to_vec();
                let at = at.min(out.len());
                out.splice(at..at, piece);
            }
            _ => out.truncate(at),
        }
        if out.is_empty() {
            break;
        }
    }
    out.into_iter().collect()
}

const FUZZ_ROUNDS: usize = 400;

fn seeds() -> Vec<&'static str> {
    let mut all: Vec<&'static str> = RULE_FILES.iter().map(|(_, t)| *t).collect();
    all.extend(FUNCTION_FILES.iter().map(|(_, t)| *t));
    all
}

#[test]
fn mutated_rule_files_never_panic() {
    let config = Config::parse(DEFAULT_CONFIG).expect("default config parses");
    let catalog = Catalog { config: &config, functions: None };
    let mut rng = Xorshift(0x9E37_79B9_7F4A_7C15);
    for seed in RULE_FILES.iter().map(|(_, t)| *t) {
        for _ in 0..FUZZ_ROUNDS {
            let _ = load_rules(&mutate(&mut rng, seed), "fuzz.yml", &catalog);
        }
    }
}

#[test]
fn mutated_configs_never_panic() {
    let mut rng = Xorshift(0xD1B5_4A32_D192_ED03);
    for _ in 0..FUZZ_ROUNDS * 3 {
        let _ = Config::parse(&mutate(&mut rng, DEFAULT_CONFIG));
    }
}

#[test]
fn mutated_functions_never_panic() {
    let mut rng = Xorshift(0x2545_F491_4F6C_DD1D);
    for seed in FUNCTION_FILES.iter().map(|(_, t)| *t) {
        for _ in 0..FUZZ_ROUNDS {
            let text = mutate(&mut rng, seed);
            let _ = parse_json("fuzz:f", &text, "fuzz.json");
            let _ = parse_mcfunction("fuzz:f", &text, "fuzz.mcfunction");
        }
    }
}

#[test]
fn mutated_scripts_never_panic() {
    let config = Config::parse(DEFAULT_CONFIG).expect("default config parses");
    let catalog = Catalog { config: &config, functions: None };
    let script = include_str!("golden/example.ttl");
    let mut rng = Xorshift(0x1234_5678_9ABC_DEF1);
    for _ in 0..FUZZ_ROUNDS * 3 {
        let _ = crate::dsl::compile("fuzz.ttl", &mutate(&mut rng, script), &catalog);
    }
}

#[test]
fn arbitrary_bytes_never_panic_any_parser() {
    let config = Config::parse(DEFAULT_CONFIG).expect("default config parses");
    let catalog = Catalog { config: &config, functions: None };
    let mut rng = Xorshift(0xDEAD_BEEF_CAFE_F00D);
    let _ = seeds();
    for _ in 0..FUZZ_ROUNDS {
        let len = rng.below(300);
        let text: String = (0..len).map(|_| char::from_u32(rng.next() as u32 % 0x250).unwrap_or('?')).collect();
        let _ = load_rules(&text, "x.yml", &catalog);
        let _ = Config::parse(&text);
        let _ = parse_json("a:b", &text, "x");
        let _ = parse_mcfunction("a:b", &text, "x");
        let _ = crate::dsl::compile("x.ttl", &text, &catalog);
    }
}

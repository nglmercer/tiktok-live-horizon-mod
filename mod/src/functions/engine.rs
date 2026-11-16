//! Function registry and execution by ticks (`FunctionEngineService` + `FunctionJobManager`).
use std::collections::{BTreeMap, HashSet, VecDeque};

use super::compile::{Compiler, Item, Plan, Policy};
use super::model::Definition;
use super::{FnError, Limits};
use crate::value::Val;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
}

pub struct Job {
    pub id: u64,
    pub function_id: String,
    /// The player it runs on (its relative coordinates start from them). `None`: the server.
    pub target: Option<String>,
    pub state: JobState,
    items: VecDeque<Item>,
    wait_until: u64,
    deadline_ms: i64,
    pub last_error: String,
}

#[derive(Default)]
pub struct Engine {
    pub registry: BTreeMap<String, Definition>,
    jobs: Vec<Job>,
    next_id: u64,
    pub completed: u64,
    pub failed: u64,
    pub rejected: u64,
}

/// Does the command use relative coordinates (`~` or `^`)?
pub fn uses_relative(command: &str) -> bool {
    command.contains('~') || command.contains('^')
}

impl Engine {
    pub fn replace_registry(&mut self, registry: BTreeMap<String, Definition>) {
        self.registry = registry;
    }

    pub fn ids(&self) -> Vec<String> {
        self.registry.keys().cloned().collect()
    }

    pub fn compile(&self, function_id: &str, args: &BTreeMap<String, Val>, limits: &Limits, allowed: &[String]) -> Result<Plan, FnError> {
        let policy = Policy { allowed };
        Compiler { registry: &self.registry, limits, policy: &policy }.compile(function_id, args)
    }

    /// The commands and waits a function would run, without running anything.
    pub fn preview(&self, function_id: &str, args: &BTreeMap<String, Val>, limits: &Limits, allowed: &[String]) -> Result<Vec<String>, FnError> {
        let plan = self.compile(function_id, args, limits, allowed)?;
        Ok(plan
            .items
            .iter()
            .map(|item| match item {
                Item::Command(c) => format!("command {c}"),
                Item::Delay(t) => format!("delay {t} ticks"),
            })
            .collect())
    }

    /// Compiles and queues. Returns the job id or why it was rejected.
    #[allow(clippy::too_many_arguments)]
    pub fn submit(
        &mut self,
        function_id: &str,
        args: &BTreeMap<String, Val>,
        target: Option<String>,
        limits: &Limits,
        allowed: &[String],
        max_concurrent: usize,
        max_queued: usize,
        now_ms: i64,
    ) -> Result<u64, String> {
        let plan = self.compile(function_id, args, limits, allowed).map_err(|e| {
            self.failed += 1;
            e.to_string()
        })?;
        let running = self.jobs.iter().filter(|j| j.state == JobState::Running).count();
        let queued = self.jobs.len() - running;
        let state = if running < max_concurrent {
            JobState::Running
        } else if queued < max_queued {
            JobState::Queued
        } else {
            self.rejected += 1;
            return Err("The function queue is full".to_owned());
        };
        self.next_id += 1;
        self.jobs.push(Job {
            id: self.next_id,
            function_id: function_id.to_owned(),
            target,
            state,
            items: plan.items.into(),
            wait_until: 0,
            deadline_ms: now_ms + (limits.max_execution_seconds as i64) * 1000,
            last_error: String::new(),
        });
        Ok(self.next_id)
    }

    pub fn jobs(&self) -> &[Job] {
        &self.jobs
    }

    pub fn active(&self) -> usize {
        self.jobs.len()
    }

    pub fn cancel(&mut self, id: u64) -> bool {
        let before = self.jobs.len();
        self.jobs.retain(|j| j.id != id);
        self.jobs.len() != before
    }

    #[allow(dead_code)] // reserved: part of the configuration's API/semantics
    pub fn cancel_all(&mut self) {
        self.jobs.clear();
    }

    /// Advances the jobs one tick and returns the commands to run (without a leading slash).
    pub fn tick(&mut self, tick: u64, now_ms: i64, mut budget: usize, max_concurrent: usize, online: &HashSet<String>) -> Vec<String> {
        // Queue promotion.
        let mut running = self.jobs.iter().filter(|j| j.state == JobState::Running).count();
        for job in &mut self.jobs {
            if running >= max_concurrent {
                break;
            }
            if job.state == JobState::Queued {
                job.state = JobState::Running;
                running += 1;
            }
        }
        let mut out = Vec::new();
        let mut finished: Vec<(u64, bool)> = Vec::new();
        for job in &mut self.jobs {
            if job.state != JobState::Running {
                continue;
            }
            if let Some(name) = &job.target {
                if !online.contains(&name.to_ascii_lowercase()) {
                    job.last_error = "SESSION_CHANGED".to_owned();
                    finished.push((job.id, false));
                    continue;
                }
            }
            if now_ms > job.deadline_ms {
                job.last_error = "EXECUTION_TIMEOUT".to_owned();
                finished.push((job.id, false));
                continue;
            }
            if tick < job.wait_until {
                continue;
            }
            loop {
                match job.items.front() {
                    None => {
                        finished.push((job.id, true));
                        break;
                    }
                    Some(Item::Delay(ticks)) => {
                        job.wait_until = tick + u64::from(*ticks);
                        job.items.pop_front();
                        break;
                    }
                    Some(Item::Command(_)) => {
                        if budget == 0 {
                            break;
                        }
                        let Some(Item::Command(command)) = job.items.pop_front() else { break };
                        let bare = command.trim_start_matches('/').to_owned();
                        if uses_relative(&bare) {
                            match &job.target {
                                Some(name) => out.push(format!("execute at {name} run {bare}")),
                                None => {
                                    job.last_error = "NO_TARGET".to_owned();
                                    finished.push((job.id, false));
                                    break;
                                }
                            }
                        } else {
                            out.push(bare);
                        }
                        budget -= 1;
                    }
                }
            }
        }
        for (id, ok) in finished {
            self.jobs.retain(|j| j.id != id);
            if ok {
                self.completed += 1;
            } else {
                self.failed += 1;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions::parse::parse_json;

    fn engine() -> Engine {
        let text = r#"{"name":"t:f","steps":[{"type":"summon","entity":"minecraft:zombie","position":"~ ~1 ~"},{"type":"delay","ticks":3},{"type":"command","value":"say hi"}]}"#;
        let mut engine = Engine::default();
        engine.registry.insert("t:f".to_owned(), parse_json("t:f", text, "t").unwrap());
        engine
    }

    fn submit(engine: &mut Engine, target: Option<&str>) -> Result<u64, String> {
        engine.submit("t:f", &BTreeMap::new(), target.map(str::to_owned), &Limits::default(), &["*".to_owned()], 2, 1, 0)
    }

    #[test]
    fn runs_with_delay_and_wraps_relative_commands() {
        let mut engine = engine();
        submit(&mut engine, Some("Ana")).unwrap();
        let online: HashSet<String> = ["ana".to_owned()].into();
        assert_eq!(engine.tick(1, 0, 4, 2, &online), vec!["execute at Ana run summon minecraft:zombie ~ ~1 ~".to_owned()]);
        assert!(engine.tick(2, 0, 4, 2, &online).is_empty(), "waiting");
        assert_eq!(engine.tick(4, 0, 4, 2, &online), vec!["say hi".to_owned()]);
        assert_eq!(engine.active(), 0);
        assert_eq!(engine.completed, 1);
    }

    #[test]
    fn a_relative_command_without_a_target_fails_and_a_departed_player_cancels() {
        let mut engine = engine();
        submit(&mut engine, None).unwrap();
        assert!(engine.tick(1, 0, 4, 2, &HashSet::new()).is_empty());
        assert_eq!(engine.failed, 1);
        submit(&mut engine, Some("Ana")).unwrap();
        assert!(engine.tick(1, 0, 4, 2, &HashSet::new()).is_empty());
        assert_eq!(engine.failed, 2);
    }

    #[test]
    fn concurrency_and_queue_are_bounded() {
        let mut engine = engine();
        for _ in 0..2 {
            submit(&mut engine, Some("Ana")).unwrap();
        }
        submit(&mut engine, Some("Ana")).unwrap(); // queued
        assert_eq!(submit(&mut engine, Some("Ana")).unwrap_err(), "The function queue is full");
        assert_eq!(engine.rejected, 1);
    }

    #[test]
    fn the_command_budget_is_shared() {
        let mut engine = engine();
        submit(&mut engine, Some("Ana")).unwrap();
        submit(&mut engine, Some("Bea")).unwrap();
        let online: HashSet<String> = ["ana".to_owned(), "bea".to_owned()].into();
        assert_eq!(engine.tick(1, 0, 1, 2, &online).len(), 1);
    }
}

//! Scheduler on top of `server_tick`: delayed commands and bounded repetitions
//! (storms, TNT bursts, restorations). Replaces `BukkitRunnable`.

/// Limit of active repeating jobs and of pending delayed commands.
const MAX_EVERY: usize = 128;
const MAX_LATER: usize = 8192;
/// How many repeating commands run at most per tick.
const EVERY_BUDGET_PER_TICK: usize = 32;

struct EveryJob {
    key: Option<String>,
    next: u64,
    interval: u64,
    until: u64,
    commands: Vec<String>,
}

#[derive(Default)]
pub struct Scheduler {
    later: Vec<(u64, u64, String)>,
    every: Vec<EveryJob>,
    seq: u64,
}

impl Scheduler {
    pub fn later(&mut self, now: u64, ticks: u64, command: String) -> bool {
        if self.later.len() >= MAX_LATER {
            return false;
        }
        self.seq += 1;
        self.later.push((now + ticks.max(1), self.seq, command));
        true
    }

    /// One repetition every `interval` ticks for `duration` ticks. With a key, it replaces the previous one.
    pub fn every(&mut self, now: u64, key: Option<String>, interval: u64, duration: u64, commands: Vec<String>) -> bool {
        let job = EveryJob { key: key.clone(), next: now + interval.max(1), interval: interval.max(1), until: now + duration, commands };
        if let Some(key) = &key {
            if let Some(existing) = self.every.iter_mut().find(|j| j.key.as_ref() == Some(key)) {
                *existing = job;
                return true;
            }
        }
        if self.every.len() >= MAX_EVERY {
            return false;
        }
        self.every.push(job);
        true
    }

    /// The commands that are due in this tick.
    pub fn due(&mut self, tick: u64) -> Vec<String> {
        let mut out = Vec::new();
        let mut keep = Vec::with_capacity(self.later.len());
        let mut ready: Vec<(u64, u64, String)> = Vec::new();
        for item in self.later.drain(..) {
            if item.0 <= tick {
                ready.push(item);
            } else {
                keep.push(item);
            }
        }
        self.later = keep;
        ready.sort_by_key(|(due, seq, _)| (*due, *seq));
        out.extend(ready.into_iter().map(|(_, _, c)| c));

        let mut budget = EVERY_BUDGET_PER_TICK;
        for job in &mut self.every {
            if tick >= job.next && tick < job.until {
                for command in &job.commands {
                    if budget == 0 {
                        break;
                    }
                    out.push(command.clone());
                    budget -= 1;
                }
                job.next += job.interval;
            }
        }
        self.every.retain(|j| tick < j.until);
        out
    }

    #[allow(dead_code)] // reserved: part of the configuration's API/semantics
    pub fn clear(&mut self) {
        self.later.clear();
        self.every.clear();
    }

    pub fn pending(&self) -> usize {
        self.later.len() + self.every.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn later_runs_once_in_order() {
        let mut s = Scheduler::default();
        s.later(0, 3, "b".into());
        s.later(0, 1, "a".into());
        assert_eq!(s.due(1), vec!["a".to_string()]);
        assert!(s.due(2).is_empty());
        assert_eq!(s.due(3), vec!["b".to_string()]);
        assert_eq!(s.pending(), 0);
    }

    #[test]
    fn every_repeats_until_duration_and_keys_replace() {
        let mut s = Scheduler::default();
        s.every(0, Some("storm:ana".into()), 10, 25, vec!["x".into()]);
        assert!(s.due(9).is_empty());
        assert_eq!(s.due(10).len(), 1);
        s.every(10, Some("storm:ana".into()), 10, 25, vec!["y".into()]);
        assert_eq!(s.pending(), 1, "same key does not stack");
        assert_eq!(s.due(20), vec!["y".to_string()]);
        assert!(s.due(40).is_empty());
        assert_eq!(s.pending(), 0);
    }
}

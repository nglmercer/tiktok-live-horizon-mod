//! Actions that change blocks with restoration: `block-cage`/`tnt-cage`, `void-shaft`,
//! `chunk-replace` and `place-cobweb`. The logic sits behind the `World` trait to test it without the game.
//!
//! Everything that is changed is recorded in `saved` (position -> previous state and placed block): it is used
//! to avoid overlapping actions, to restore only if the block is still the one we placed, and to
//! give everything back when the server stops.
use std::collections::HashMap;

use crate::actions::Emit;

pub type Pos = [i32; 3];

/// The world as these actions need it. States are canonical strings
/// (`minecraft:oak_stairs[facing=east]`).
pub trait World {
    fn is_loaded(&self, dim: &str, pos: Pos) -> bool;
    /// `(minimum y, exclusive maximum y)`.
    fn height(&self, dim: &str) -> Option<(i32, i32)>;
    fn state(&self, dim: &str, pos: Pos) -> Option<String>;
    fn set_state(&mut self, dim: &str, pos: Pos, state: &str) -> bool;
    /// Is it a block that exists? (parses its default state)
    fn valid_block(&self, id: &str) -> bool;
}

/// Blocks with an inventory or other data that are never touched (`TileState` in Paper).
pub fn is_container(state: &str) -> bool {
    let id = block_id(state);
    const NAMES: [&str; 21] = [
        "chest", "trapped_chest", "ender_chest", "barrel", "furnace", "blast_furnace", "smoker", "hopper", "dropper",
        "dispenser", "brewing_stand", "crafter", "chiseled_bookshelf", "decorated_pot", "lectern", "jukebox", "beacon",
        "spawner", "trial_spawner", "vault", "sign",
    ];
    let name = id.strip_prefix("minecraft:").unwrap_or(id);
    NAMES.contains(&name) || name.ends_with("shulker_box") || name.ends_with("_sign") || name.ends_with("_hanging_sign") || name.ends_with("_bed") || name.ends_with("_banner") || name.ends_with("_head") || name.ends_with("_skull")
}

pub fn block_id(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

pub fn is_air(state: &str) -> bool {
    matches!(block_id(state), "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air")
}

#[derive(Clone)]
struct Saved {
    previous: String,
    placed: String,
}

type Key = (String, Pos);

enum Task {
    /// Gives the blocks back at once.
    RestoreAt { due: u64, keys: Vec<Key> },
    /// Gives the blocks back little by little.
    RestoreGradually { due: u64, keys: Vec<Key>, cursor: usize, per_tick: usize },
    /// Ignites the cage's TNT if it is still there.
    Ignite { due: u64, dim: String, pos: Pos, fuse: i64 },
    /// Walks a region reading blocks and collects the ones that must change.
    Scan(Box<ScanTask>),
    /// Changes the collected blocks little by little.
    Apply(Box<ApplyTask>),
}

struct ScanTask {
    dim: String,
    /// Columns `(x, z)` to walk and the vertical range.
    columns: Vec<(i32, i32)>,
    min_y: i32,
    max_y: i32,
    cursor_column: usize,
    cursor_y: i32,
    only: Option<String>,
    place: String,
    skip_if_same: bool,
    max_blocks: usize,
    per_tick: usize,
    restore_ticks: u64,
    gradual_per_tick: usize,
    found: Vec<Pos>,
    label: &'static str,
    /// `void-shaft` only: the player to recenter when it finishes.
    recenter: Option<(String, [f64; 3])>,
}

struct ApplyTask {
    dim: String,
    positions: Vec<Pos>,
    cursor: usize,
    place: String,
    per_tick: usize,
    restore_ticks: u64,
    gradual_per_tick: usize,
    owned: Vec<Key>,
    label: &'static str,
    recenter: Option<(String, [f64; 3])>,
}

/// Block reads per tick while a region is walked.
const SCAN_PER_TICK: usize = 16_384;

#[derive(Default)]
pub struct BlockManager {
    saved: HashMap<Key, Saved>,
    tasks: Vec<Task>,
    pub changed: u64,
    pub restored: u64,
}

pub struct CageParams {
    pub dim: String,
    /// The player's exact position.
    pub center: [f64; 3],
    pub material: String,
    pub radius: i32,
    pub height: i32,
    pub restore_seconds: u64,
    pub ignite: Option<(u64, i64)>,
    pub occupant: Option<(String, bool)>,
}

pub enum Started {
    Ok(String),
    Skipped(String),
    Failed(String),
}

impl BlockManager {
    pub fn pending(&self) -> usize {
        self.saved.len()
    }

    pub fn tasks(&self) -> usize {
        self.tasks.len()
    }

    fn write(&mut self, world: &mut dyn World, dim: &str, pos: Pos, new_state: &str) -> bool {
        let key = (dim.to_owned(), pos);
        let Some(previous) = world.state(dim, pos) else { return false };
        if !world.set_state(dim, pos, new_state) {
            return false;
        }
        self.saved.insert(key, Saved { previous, placed: new_state.to_owned() });
        self.changed += 1;
        true
    }

    /// Gives a block back to its previous state only if it is still the one we placed.
    fn restore_one(&mut self, world: &mut dyn World, key: &Key) {
        let Some(saved) = self.saved.remove(key) else { return };
        if !world.is_loaded(&key.0, key.1) {
            return;
        }
        if world.state(&key.0, key.1).as_deref() == Some(saved.placed.as_str()) && world.set_state(&key.0, key.1, &saved.previous) {
            self.restored += 1;
        }
    }

    pub fn restore_all(&mut self, world: &mut dyn World) {
        self.tasks.clear();
        let keys: Vec<Key> = self.saved.keys().cloned().collect();
        for key in keys {
            self.restore_one(world, &key);
        }
        self.saved.clear();
    }

    // --- block-cage / tnt-cage ---------------------------------------------------------------

    /// Builds the cage. Returns `Skipped`/`Failed` with the same text as the plugin.
    pub fn cage(&mut self, world: &mut dyn World, tick: u64, params: &CageParams, out: &mut Vec<Emit>) -> Started {
        let Some((min_y, max_y)) = world.height(&params.dim) else {
            return Started::Skipped("Player world unavailable".to_owned());
        };
        let (cx, cy, cz) = (params.center[0].floor() as i32, params.center[1].floor() as i32, params.center[2].floor() as i32);
        let (bottom, top) = (cy - 1, cy + params.height);
        if bottom < min_y || top >= max_y {
            return Started::Skipped("Block cage exceeds build height".to_owned());
        }
        let mut shell: Vec<Pos> = Vec::new();
        for y in bottom..=top {
            for dx in -params.radius..=params.radius {
                for dz in -params.radius..=params.radius {
                    let boundary = y == bottom || y == top || dx.abs() == params.radius || dz.abs() == params.radius;
                    if !boundary {
                        continue;
                    }
                    let pos = [cx + dx, y, cz + dz];
                    if !world.is_loaded(&params.dim, pos) {
                        return Started::Skipped("Cage would reach an unloaded chunk".to_owned());
                    }
                    let state = world.state(&params.dim, pos).unwrap_or_default();
                    if self.saved.contains_key(&(params.dim.clone(), pos)) || is_container(&state) {
                        return Started::Skipped("Cage overlaps an active cage or a container".to_owned());
                    }
                    shell.push(pos);
                }
            }
        }
        if shell.len() > 260 {
            return Started::Skipped("Block cage budget exceeded".to_owned());
        }
        let mut built: Vec<Key> = Vec::new();
        for pos in &shell {
            if self.write(world, &params.dim, *pos, &params.material) {
                built.push((params.dim.clone(), *pos));
            } else {
                for key in &built {
                    self.restore_one(world, key);
                }
                return Started::Failed("Could not complete block cage: a block could not be placed".to_owned());
            }
        }
        if let Some((entity, charged)) = &params.occupant {
            let nbt = if *charged { " {powered:1b}" } else { "" };
            out.push(Emit::Command(format!(
                "execute in {} run summon {entity} {:.3} {:.3} {:.3}{nbt}",
                params.dim, params.center[0], params.center[1] + 0.15, params.center[2]
            )));
        }
        if let Some((delay, fuse)) = params.ignite {
            self.tasks.push(Task::Ignite { due: tick + delay, dim: params.dim.clone(), pos: [cx + params.radius, cy + 1, cz], fuse });
        }
        if params.restore_seconds > 0 {
            self.tasks.push(Task::RestoreAt { due: tick + params.restore_seconds * 20, keys: built });
        } else {
            for key in built {
                self.saved.remove(&key);
            }
        }
        Started::Ok(format!("Built {} {} cage blocks", shell.len(), params.material))
    }

    // --- void-shaft / chunk-replace (scan + batched application) -------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn start_scan(
        &mut self,
        dim: &str,
        columns: Vec<(i32, i32)>,
        min_y: i32,
        max_y: i32,
        only: Option<String>,
        place: String,
        max_blocks: usize,
        per_tick: usize,
        restore_ticks: u64,
        gradual_per_tick: usize,
        label: &'static str,
        recenter: Option<(String, [f64; 3])>,
    ) {
        self.tasks.push(Task::Scan(Box::new(ScanTask {
            dim: dim.to_owned(),
            columns,
            min_y,
            max_y,
            cursor_column: 0,
            cursor_y: min_y,
            only,
            place,
            skip_if_same: true,
            max_blocks,
            per_tick,
            restore_ticks,
            gradual_per_tick,
            found: Vec::new(),
            label,
            recenter,
        })));
    }

    /// `place-cobweb`: temporary cobwebs in a spiral around the player.
    pub fn cobwebs(&mut self, world: &mut dyn World, tick: u64, dim: &str, origin: [f64; 3], count: i64, restore_seconds: u64) -> Started {
        let mut applied = 0;
        let mut keys = Vec::new();
        for i in 0..(count * 3) {
            if applied >= count {
                break;
            }
            let phi = 2.399963229728653 * i as f64;
            let radius = 1.0 + (i % 4) as f64;
            let pos = [
                (origin[0] + phi.cos() * radius).floor() as i32,
                (origin[1] + (i % 2) as f64).floor() as i32,
                (origin[2] + phi.sin() * radius).floor() as i32,
            ];
            if !world.is_loaded(dim, pos) {
                continue;
            }
            let key = (dim.to_owned(), pos);
            if self.saved.contains_key(&key) || !world.state(dim, pos).is_some_and(|s| is_air(&s)) {
                continue;
            }
            if self.write(world, dim, pos, "minecraft:cobweb") {
                keys.push(key);
                applied += 1;
            }
        }
        if !keys.is_empty() {
            self.tasks.push(Task::RestoreAt { due: tick + restore_seconds * 20, keys });
        }
        Started::Ok(format!("Placed {applied} temporary cobwebs"))
    }

    // --- tick ---------------------------------------------------------------------------------

    pub fn tick(&mut self, world: &mut dyn World, tick: u64, out: &mut Vec<Emit>) -> Vec<String> {
        let mut log_lines = Vec::new();
        let tasks = std::mem::take(&mut self.tasks);
        let mut keep = Vec::new();
        for task in tasks {
            match task {
                Task::RestoreAt { due, keys } => {
                    if tick < due {
                        keep.push(Task::RestoreAt { due, keys });
                    } else {
                        for key in &keys {
                            self.restore_one(world, key);
                        }
                    }
                }
                Task::RestoreGradually { due, keys, mut cursor, per_tick } => {
                    if tick < due {
                        keep.push(Task::RestoreGradually { due, keys, cursor, per_tick });
                        continue;
                    }
                    let end = (cursor + per_tick).min(keys.len());
                    for key in &keys[cursor..end] {
                        self.restore_one(world, key);
                    }
                    cursor = end;
                    if cursor < keys.len() {
                        keep.push(Task::RestoreGradually { due, keys, cursor, per_tick });
                    }
                }
                Task::Ignite { due, dim, pos, fuse } => {
                    if tick < due {
                        keep.push(Task::Ignite { due, dim, pos, fuse });
                        continue;
                    }
                    if world.is_loaded(&dim, pos) && world.state(&dim, pos).as_deref().map(block_id) == Some("minecraft:tnt") {
                        self.saved.remove(&(dim.clone(), pos));
                        if world.set_state(&dim, pos, "minecraft:air") {
                            out.push(Emit::Command(format!(
                                "execute in {dim} run summon minecraft:tnt {} {} {} {{fuse:{fuse}}}",
                                f64::from(pos[0]) + 0.5,
                                f64::from(pos[1]) + 0.5,
                                f64::from(pos[2]) + 0.5
                            )));
                        }
                    }
                }
                Task::Scan(mut scan) => match self.advance_scan(world, &mut scan) {
                    ScanStep::More => keep.push(Task::Scan(scan)),
                    ScanStep::Done(message) => {
                        log_lines.push(message);
                        if !scan.found.is_empty() {
                            keep.push(Task::Apply(Box::new(ApplyTask {
                                dim: scan.dim.clone(),
                                positions: std::mem::take(&mut scan.found),
                                cursor: 0,
                                place: scan.place.clone(),
                                per_tick: scan.per_tick,
                                restore_ticks: scan.restore_ticks,
                                gradual_per_tick: scan.gradual_per_tick,
                                owned: Vec::new(),
                                label: scan.label,
                                recenter: scan.recenter.clone(),
                            })));
                        }
                    }
                },
                Task::Apply(mut apply) => {
                    let mut modified = 0;
                    while modified < apply.per_tick && apply.cursor < apply.positions.len() {
                        let pos = apply.positions[apply.cursor];
                        apply.cursor += 1;
                        if !world.is_loaded(&apply.dim, pos) {
                            continue;
                        }
                        let key = (apply.dim.clone(), pos);
                        if self.saved.contains_key(&key) {
                            continue;
                        }
                        let Some(current) = world.state(&apply.dim, pos) else { continue };
                        if is_container(&current) || current == apply.place {
                            continue;
                        }
                        if self.write(world, &apply.dim, pos, &apply.place) {
                            apply.owned.push(key);
                            modified += 1;
                        }
                    }
                    if apply.cursor < apply.positions.len() {
                        keep.push(Task::Apply(apply));
                        continue;
                    }
                    log_lines.push(format!("{}: changed {} block(s)", apply.label, apply.owned.len()));
                    if let Some((player, start)) = &apply.recenter {
                        out.push(Emit::Command(format!(
                            "execute in {} run tp {player} {:.2} ~ {:.2}",
                            apply.dim,
                            start[0].floor() + 0.5,
                            start[2].floor() + 0.5
                        )));
                    }
                    if apply.restore_ticks > 0 {
                        keep.push(Task::RestoreGradually { due: tick + apply.restore_ticks, keys: std::mem::take(&mut apply.owned), cursor: 0, per_tick: apply.gradual_per_tick });
                    } else {
                        for key in &apply.owned {
                            self.saved.remove(key);
                        }
                    }
                }
            }
        }
        keep.extend(std::mem::take(&mut self.tasks));
        self.tasks = keep;
        log_lines
    }

    fn advance_scan(&mut self, world: &mut dyn World, scan: &mut ScanTask) -> ScanStep {
        let mut reads = 0;
        while scan.cursor_column < scan.columns.len() {
            let (x, z) = scan.columns[scan.cursor_column];
            while scan.cursor_y <= scan.max_y {
                if reads >= SCAN_PER_TICK {
                    return ScanStep::More;
                }
                let pos = [x, scan.cursor_y, z];
                scan.cursor_y += 1;
                reads += 1;
                if !world.is_loaded(&scan.dim, pos) {
                    // An unloaded chunk is skipped whole: the rest of that column is not loaded either.
                    scan.cursor_y = scan.max_y + 1;
                    break;
                }
                let Some(state) = world.state(&scan.dim, pos) else { continue };
                if self.saved.contains_key(&(scan.dim.clone(), pos)) || is_container(&state) {
                    continue;
                }
                if let Some(only) = &scan.only {
                    if block_id(&state) != block_id(only) {
                        continue;
                    }
                }
                if scan.skip_if_same && state == scan.place {
                    continue;
                }
                if scan.place.is_empty() || (is_air(&state) && is_air(&scan.place)) {
                    continue;
                }
                scan.found.push(pos);
                if scan.max_blocks > 0 && scan.found.len() > scan.max_blocks {
                    let message = format!("{}: needs more than {} blocks (max-blocks); nothing changed", scan.label, scan.max_blocks);
                    scan.found.clear();
                    scan.cursor_column = scan.columns.len();
                    return ScanStep::Done(message);
                }
            }
            scan.cursor_column += 1;
            scan.cursor_y = scan.min_y;
        }
        let message = format!("{}: {} block(s) to change", scan.label, scan.found.len());
        ScanStep::Done(message)
    }
}

enum ScanStep {
    More,
    Done(String),
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[derive(Default)]
    pub struct Fake {
        pub blocks: HashMap<Key, String>,
        pub loaded: bool,
    }

    impl Fake {
        pub fn filled(min: i32, max: i32, area: i32, state: &str) -> Fake {
            let mut blocks = HashMap::new();
            for x in -area..=area {
                for z in -area..=area {
                    for y in min..max {
                        blocks.insert(("minecraft:overworld".to_owned(), [x, y, z]), state.to_owned());
                    }
                }
            }
            Fake { blocks, loaded: true }
        }
        fn get(&self, pos: Pos) -> String {
            self.blocks.get(&("minecraft:overworld".to_owned(), pos)).cloned().unwrap_or_else(|| "minecraft:air".to_owned())
        }
    }

    impl World for Fake {
        fn is_loaded(&self, _dim: &str, _pos: Pos) -> bool {
            self.loaded
        }
        fn height(&self, _dim: &str) -> Option<(i32, i32)> {
            Some((0, 64))
        }
        fn state(&self, dim: &str, pos: Pos) -> Option<String> {
            Some(self.blocks.get(&(dim.to_owned(), pos)).cloned().unwrap_or_else(|| "minecraft:air".to_owned()))
        }
        fn set_state(&mut self, dim: &str, pos: Pos, state: &str) -> bool {
            self.blocks.insert((dim.to_owned(), pos), state.to_owned());
            true
        }
        fn valid_block(&self, _id: &str) -> bool {
            true
        }
    }

    const DIM: &str = "minecraft:overworld";

    fn params(restore: u64) -> CageParams {
        CageParams {
            dim: DIM.into(),
            center: [0.5, 10.0, 0.5],
            material: "minecraft:obsidian".into(),
            radius: 1,
            height: 3,
            restore_seconds: restore,
            ignite: None,
            occupant: Some(("minecraft:creeper".into(), true)),
        }
    }

    #[test]
    fn a_cage_is_built_hollow_and_restored_exactly() {
        let mut world = Fake::filled(0, 30, 3, "minecraft:stone");
        // The interior (where the player stands) is air.
        for y in 10..13 {
            world.blocks.insert((DIM.into(), [0, y, 0]), "minecraft:air".into());
        }
        let before = world.blocks.clone();
        let mut manager = BlockManager::default();
        let mut out = Vec::new();
        assert!(matches!(manager.cage(&mut world, 0, &params(1), &mut out), Started::Ok(_)));
        assert_eq!(world.get([1, 10, 0]), "minecraft:obsidian", "wall");
        assert_eq!(world.get([0, 10, 0]), "minecraft:air", "inside stays empty");
        assert_eq!(world.get([0, 13, 0]), "minecraft:obsidian", "roof (cy + height)");
        assert_eq!(world.get([0, 9, 0]), "minecraft:obsidian", "floor (cy - 1)");
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], Emit::Command(c) if c.contains("summon minecraft:creeper") && c.contains("{powered:1b}")));
        manager.tick(&mut world, 5, &mut out);
        assert_eq!(world.get([1, 10, 0]), "minecraft:obsidian", "not yet");
        manager.tick(&mut world, 20, &mut out);
        assert_eq!(world.blocks, before, "restored exactly");
        assert_eq!(manager.pending(), 0);
    }

    #[test]
    fn a_second_cage_does_not_overwrite_the_first_restore() {
        let mut world = Fake::filled(0, 30, 3, "minecraft:stone");
        let mut manager = BlockManager::default();
        let mut out = Vec::new();
        assert!(matches!(manager.cage(&mut world, 0, &params(30), &mut out), Started::Ok(_)));
        assert!(matches!(manager.cage(&mut world, 1, &params(30), &mut out), Started::Skipped(m) if m.contains("active cage")));
    }

    #[test]
    fn containers_and_unloaded_chunks_are_refused() {
        let mut world = Fake::filled(0, 30, 3, "minecraft:stone");
        world.blocks.insert((DIM.into(), [1, 10, 0]), "minecraft:chest[facing=north]".into());
        let mut manager = BlockManager::default();
        let mut out = Vec::new();
        assert!(matches!(manager.cage(&mut world, 0, &params(5), &mut out), Started::Skipped(m) if m.contains("container")));
        world.loaded = false;
        assert!(matches!(manager.cage(&mut world, 0, &params(5), &mut out), Started::Skipped(m) if m.contains("unloaded")));
    }

    #[test]
    fn chunk_replace_scans_changes_and_restores_gradually() {
        let mut world = Fake::filled(0, 4, 1, "minecraft:dirt");
        world.blocks.insert((DIM.into(), [0, 0, 0]), "minecraft:stone".into());
        let before = world.blocks.clone();
        let mut manager = BlockManager::default();
        let columns: Vec<(i32, i32)> = (-1..=1).flat_map(|x| (-1..=1).map(move |z| (x, z))).collect();
        manager.start_scan(DIM, columns, 0, 3, Some("minecraft:dirt".into()), "minecraft:glass".into(), 0, 1000, 20, 1024, "chunk-replace", None);
        let mut out = Vec::new();
        for tick in 1..10 {
            manager.tick(&mut world, tick, &mut out);
        }
        assert_eq!(world.get([1, 1, 1]), "minecraft:glass");
        assert_eq!(world.get([0, 0, 0]), "minecraft:stone", "only the `from` block changes");
        for tick in 10..40 {
            manager.tick(&mut world, tick, &mut out);
        }
        assert_eq!(world.blocks, before);
    }

    #[test]
    fn max_blocks_makes_the_scan_change_nothing() {
        let mut world = Fake::filled(0, 4, 1, "minecraft:dirt");
        let mut manager = BlockManager::default();
        manager.start_scan(DIM, vec![(0, 0), (1, 0)], 0, 3, None, "minecraft:air".into(), 3, 100, 0, 128, "void-shaft", None);
        let mut out = Vec::new();
        let lines: Vec<String> = (1..5).flat_map(|t| manager.tick(&mut world, t, &mut out)).collect();
        assert!(lines.iter().any(|l| l.contains("max-blocks")), "{lines:?}");
        assert_eq!(world.get([0, 0, 0]), "minecraft:dirt");
    }

    #[test]
    fn restore_all_returns_everything() {
        let mut world = Fake::filled(0, 30, 3, "minecraft:stone");
        let before = world.blocks.clone();
        let mut manager = BlockManager::default();
        let mut out = Vec::new();
        manager.cage(&mut world, 0, &params(120), &mut out);
        manager.restore_all(&mut world);
        assert_eq!(world.blocks, before);
    }
}

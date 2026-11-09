// Big Skies' host on native Horizon: the fork's game/mods.rs (tick_mods, mods_frame, mod_command,
// show_mod_output) for one native mod, driving its SDK (`horizon_skies::horizon_sdk::__host`) through upstream
// Horizon's hooks and helpers. A client half on the window runs the mod; a server half, in the level being ticked,
// does its world work (vanilla's weather kept in step, the snow round the player, the climate from the world's
// generator), the two talking over mod messages (`skies:work`, `skies:answer`).
//
// Adapted from Orbital Laser's native glue (github.com/Stink-O/orbital-laser, MIT License, Copyright (c) 2026 Stink-O):
// the wire, the two halves and the hooks are its; the climate is Skies' own.
use super::Game;
use super::native_world_columns::NativeColumnBlocks;
use super::native_world_edit::NativeHeightmap;
use horizon_client::text::{self, Text};
use horizon_skies::horizon_sdk::{__host as host, game as g, world};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

/// The mod's id on Horizon (its settings, its data folder, its row on Options > Mods); its command is `/skies`.
pub(super) const ID: &str = "big_skies";
const COMMAND: &str = "skies";
const NAME: &str = "Big Skies";
const VERSION: &str = "@VERSION@";
const WORK: &str = "skies:work";
const ANSWER: &str = "skies:answer";
/// A message's bytes at most (the channel's 30 000, less room for its header).
const MESSAGE: usize = 28_000;
/// Columns a read asks in one message (8 bytes each); the answers are split by their own size.
const READ_COLUMNS: usize = 3000;

thread_local! {
    /// Started (its `init` ran) on this thread: the window's.
    static STARTED: Cell<Option<bool>> = const { Cell::new(None) };
    static TICKS: Cell<u64> = const { Cell::new(0) };
    static CLIENT: RefCell<Client> = RefCell::new(Client::default());
    static SERVER: RefCell<Server> = RefCell::new(Server::default());
}
/// `/skies …` heard on the server thread (`horizon-ctl command`): the mod runs on the window's.
static SERVER_COMMANDS: Mutex<Vec<String>> = Mutex::new(Vec::new());

// ---- the client half's state ------------------------------------------------------------------------------------

#[derive(Default)]
struct Client {
    /// Work for the server half, in order, as messages.
    out: VecDeque<Vec<u8>>,
    /// Answers coming back in pieces: column reads and climate reads by tag.
    columns: BTreeMap<u32, Columns>,
    climate: BTreeMap<u32, Climate>,
    /// The client level last loaded (`client_level_load`).
    level: Option<String>,
}

/// A column read coming back in pieces.
#[derive(Default)]
struct Columns {
    total: usize,
    got: usize,
    span: usize,
    palette: Vec<String>,
    index: BTreeMap<String, u16>,
    bases: Vec<Option<i32>>,
    blocks: Vec<u16>,
    tints: Vec<u32>,
}

/// A climate read coming back in pieces.
#[derive(Default)]
struct Climate {
    total: usize,
    got: usize,
    biomes: Vec<String>,
    temperatures: Vec<f32>,
    downfalls: Vec<f32>,
    index: BTreeMap<String, u16>,
    columns: Vec<Option<u16>>,
}

// ---- starting -----------------------------------------------------------------------------------------------------

fn running(game: &mut Game<'_>) -> bool {
    if !game.native_window() || !game.native_on(ID) {
        return false;
    }
    if let Some(on) = STARTED.with(Cell::get) {
        return on;
    }
    let dir = game.native_data_dir(ID);
    host::with(|s| {
        s.id = ID.to_owned();
        s.name = NAME.to_owned();
        s.data_dir = dir;
    });
    let on = match (horizon_skies::__EXPORTS.init)() {
        Ok(_) => true,
        Err(e) => {
            log::warn!("mod {ID} did not start: {e}");
            false
        }
    };
    STARTED.with(|s| s.set(Some(on)));
    show_output(game);
    on
}

/// What the mod reads of the game this tick: where the player is, and the clock.
fn snapshot(game: &mut Game<'_>) {
    let snapshot = host::Snapshot {
        player: game.native_player().map(|p| g::Player {
            x: p.x,
            y: p.y,
            z: p.z,
            yaw: p.yaw,
            pitch: p.pitch,
            health: 20.0,
            food: 20,
            dimension: p.dimension.to_owned(),
        }),
        // (the SDK keeps the fork's unsigned clock)
        clock: game.native_clock().map(|c| g::Clock {
            game_time: c.game_time.max(0) as u64,
            day_time: c.day_time.max(0) as u64,
        }),
        ..host::Snapshot::default()
    };
    host::with(|s| s.snapshot = snapshot);
}

fn heightmap(h: world::Heightmap) -> u8 {
    match h {
        world::Heightmap::WorldSurface => 1,
        world::Heightmap::MotionBlocking => 2,
        world::Heightmap::MotionBlockingNoLeaves => 3,
        world::Heightmap::OceanFloor => 4,
    }
}
fn heightmap_of(h: u8) -> Option<NativeHeightmap> {
    Some(match h {
        1 => NativeHeightmap::WorldSurface,
        2 => NativeHeightmap::MotionBlocking,
        3 => NativeHeightmap::MotionBlockingNoLeaves,
        4 => NativeHeightmap::OceanFloor,
        _ => return None,
    })
}

/// `show_mod_output`, and the post pass the mod keeps.
fn show_output(game: &mut Game<'_>) {
    let (out, post, params, data) =
        host::with(|s| (std::mem::take(&mut s.out), s.post_source.clone(), s.post_params, s.post_data.clone()));
    for output in out {
        match output {
            host::Output::Sound { event, at, volume, pitch } => {
                if game.native_window() {
                    game.audio.play(&event, at, volume, pitch);
                }
            }
            host::Output::Commands { commands } => send_commands(&commands),
            host::Output::Chat { name, text: t } => {
                let mut line = Text::literal(format!("[{name}] ")).with_color(text::GRAY);
                line.push(Text::literal(t));
                chat(game, line);
            }
            host::Output::Columns { tag, columns, heightmap: h, below, above } => {
                send_columns(tag, &columns, heightmap(h), below, above)
            }
            host::Output::Climate { tag, columns } => send_climate(tag, &columns),
            // (Skies asks nothing else of the game)
            _ => {}
        }
    }
    // the pass, while there is a world to draw it over
    let day_time = game.native_clock().map_or(0, |c| c.day_time.max(0) as u64);
    let pass = match (post, params) {
        (Some((source, version)), Some(params)) if game.native_player().is_some() => {
            Some(horizon_client_render::skies_post::Pass { source, version, params, data, day_time })
        }
        _ => None,
    };
    horizon_client_render::skies_post::set(pass);
}

/// A line in chat, not through system_message (a mod must not hear its own lines).
fn chat(game: &mut Game<'_>, line: Text) {
    let focused = game.screen == super::Screen::Chat;
    game.ui.chat.add(&game.gui.atlas, line, None, horizon_client::chat::ChatSource::Client, game.ticks, focused);
}

// ---- the wire: requests (kind 0, 3, 5) and answers (11, 12) --------------------------------------------------------

struct Writer(Vec<u8>);
impl Writer {
    fn new(kind: u8) -> Self {
        Self(vec![kind])
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.0.extend_from_slice(s.as_bytes());
    }
    fn strs(&mut self, v: &[String]) {
        self.u32(v.len() as u32);
        for s in v {
            self.str(s);
        }
    }
}
struct Reader<'a>(&'a [u8]);
impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_le_bytes(b.try_into().unwrap()))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    }
    fn i32(&mut self) -> Option<i32> {
        self.take(4).map(|b| i32::from_le_bytes(b.try_into().unwrap()))
    }
    fn f32(&mut self) -> Option<f32> {
        self.take(4).map(|b| f32::from_le_bytes(b.try_into().unwrap()))
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        self.take(n).and_then(|b| String::from_utf8(b.to_vec()).ok())
    }
    fn strs(&mut self) -> Option<Vec<String>> {
        let n = self.u32()? as usize;
        (0..n).map(|_| self.str()).collect()
    }
    fn columns(&mut self) -> Option<Vec<(i32, i32)>> {
        let n = self.u32()? as usize;
        (0..n).map(|_| Some((self.i32()?, self.i32()?))).collect()
    }
}

fn send(message: Vec<u8>) {
    CLIENT.with(|c| c.borrow_mut().out.push_back(message));
}
/// Commands, as many to a message as fit.
fn send_commands(commands: &[String]) {
    for part in commands.chunks(64) {
        let mut w = Writer::new(0);
        w.strs(part);
        send(w.0);
    }
}
fn send_columns(tag: u32, columns: &[(i32, i32)], h: u8, below: u32, above: u32) {
    let parts: Vec<&[(i32, i32)]> = if columns.is_empty() { vec![&[]] } else { columns.chunks(READ_COLUMNS).collect() };
    for (i, part) in parts.into_iter().enumerate() {
        let mut w = Writer::new(3);
        w.u32(tag);
        w.0.push(h);
        w.u32(below);
        w.u32(above);
        w.u32((i * READ_COLUMNS) as u32);
        w.u32(columns.len() as u32);
        w.u32(part.len() as u32);
        for &(x, z) in part {
            w.i32(x);
            w.i32(z);
        }
        send(w.0);
    }
}
/// `world.read-climate`, split as a column read is.
fn send_climate(tag: u32, columns: &[(i32, i32)]) {
    let parts: Vec<&[(i32, i32)]> = if columns.is_empty() { vec![&[]] } else { columns.chunks(READ_COLUMNS).collect() };
    for (i, part) in parts.into_iter().enumerate() {
        let mut w = Writer::new(5);
        w.u32(tag);
        w.u32((i * READ_COLUMNS) as u32);
        w.u32(columns.len() as u32);
        w.u32(part.len() as u32);
        for &(x, z) in part {
            w.i32(x);
            w.i32(z);
        }
        send(w.0);
    }
}

/// The client half: the work waiting goes to the server half, as much as the channel takes this tick, the requests
/// packed into as few messages as they fit (a message, kind 9: requests, each its length then its bytes).
fn flush_work(game: &mut Game<'_>) {
    CLIENT.with(|c| {
        let mut c = c.borrow_mut();
        while !c.out.is_empty() {
            let mut batch = vec![9u8];
            let mut taken = 0;
            for m in c.out.iter() {
                if taken > 0 && batch.len() + 4 + m.len() > MESSAGE {
                    break;
                }
                batch.extend_from_slice(&(m.len() as u32).to_le_bytes());
                batch.extend_from_slice(m);
                taken += 1;
            }
            if let Err(e) = game.native_net_send_to_server(WORK, &batch) {
                if c.out.len() > 512 {
                    static SAID: std::sync::Once = std::sync::Once::new();
                    SAID.call_once(|| log::warn!("[mod {ID}] world work waiting: {e}"));
                    // (a server without the mod never takes it: keep the newest only)
                    let excess = c.out.len() - 256;
                    c.out.drain(..excess);
                }
                break;
            }
            c.out.drain(..taken);
        }
    });
}

/// The `net_from_server` hook: answers from the server half.
pub(super) fn from_server(game: &mut Game<'_>, channel: &str, data: &[u8]) {
    if channel != ANSWER || !game.native_window() {
        return;
    }
    let mut r = Reader(data);
    let Some(kind) = r.u8() else { return };
    let done = CLIENT.with(|c| -> Option<()> {
        let mut c = c.borrow_mut();
        match kind {
            11 => {
                let (tag, offset, total, span) = (r.u32()?, r.u32()? as usize, r.u32()? as usize, r.u32()? as usize);
                let palette = r.strs()?;
                let n = r.u32()? as usize;
                let entry = c.columns.entry(tag).or_insert_with(|| Columns {
                    total,
                    span,
                    bases: vec![None; total],
                    blocks: vec![0; total * span],
                    tints: vec![0; total * 3],
                    ..Columns::default()
                });
                // this piece's palette, as indexes into the whole answer's
                let map: Vec<u16> = palette
                    .into_iter()
                    .map(|name| {
                        if let Some(&i) = entry.index.get(&name) {
                            return i;
                        }
                        let i = entry.palette.len() as u16;
                        entry.palette.push(name.clone());
                        entry.index.insert(name, i);
                        i
                    })
                    .collect();
                for i in 0..n {
                    let k = offset + i;
                    let has = r.u8()?;
                    let base = r.i32()?;
                    if k < entry.total {
                        entry.bases[k] = (has == 1).then_some(base);
                    }
                    for j in 0..entry.span {
                        let b = r.u16()? as usize;
                        if k < entry.total {
                            entry.blocks[k * entry.span + j] = map.get(b).copied().unwrap_or(0);
                        }
                    }
                    for t in 0..3 {
                        let v = r.u32()?;
                        if k < entry.total {
                            entry.tints[k * 3 + t] = v;
                        }
                    }
                }
                entry.got += n;
                if entry.got >= entry.total {
                    let e = c.columns.remove(&tag)?;
                    host::with(|s| {
                        s.columns.push(world::ColumnBlocks {
                            tag,
                            palette: e.palette,
                            bases: e.bases,
                            blocks: e.blocks,
                            tints: e.tints,
                        })
                    });
                }
            }
            12 => {
                let (tag, offset, total) = (r.u32()?, r.u32()? as usize, r.u32()? as usize);
                let names = r.strs()?;
                let mut info = Vec::with_capacity(names.len());
                for _ in 0..names.len() {
                    info.push((r.f32()?, r.f32()?));
                }
                let n = r.u32()? as usize;
                let entry = c
                    .climate
                    .entry(tag)
                    .or_insert_with(|| Climate { total, columns: vec![None; total], ..Climate::default() });
                let map: Vec<u16> = names
                    .into_iter()
                    .zip(info)
                    .map(|(name, (t, d))| {
                        if let Some(&i) = entry.index.get(&name) {
                            return i;
                        }
                        let i = entry.biomes.len() as u16;
                        entry.biomes.push(name.clone());
                        entry.temperatures.push(t);
                        entry.downfalls.push(d);
                        entry.index.insert(name, i);
                        i
                    })
                    .collect();
                for i in 0..n {
                    let b = r.u16()?;
                    if let Some(slot) = entry.columns.get_mut(offset + i) {
                        *slot = map.get(usize::from(b)).copied();
                    }
                }
                entry.got += n;
                if entry.got >= entry.total {
                    let e = c.climate.remove(&tag)?;
                    host::with(|s| {
                        s.climate.push(world::BiomeClimate {
                            tag,
                            biomes: e.biomes,
                            temperatures: e.temperatures,
                            downfalls: e.downfalls,
                            columns: e.columns,
                        })
                    });
                }
            }
            _ => {}
        }
        Some(())
    });
    if done.is_none() {
        log::warn!("[mod {ID}] a bad answer from the server half");
    }
}

// ---- the server half ----------------------------------------------------------------------------------------------

#[derive(Default)]
struct Server {
    /// Each player's requests, in the order they came.
    work: BTreeMap<u128, VecDeque<Vec<u8>>>,
    /// Answers waiting for the channel, by player.
    answers: BTreeMap<u128, VecDeque<Vec<u8>>>,
}

/// The `net_from_client` hook: a player's request, kept for the tick of their level.
pub(super) fn from_client(game: &mut Game<'_>, player: u128, channel: &str, data: &[u8]) {
    if channel != WORK || game.native_window() || !game.native_on(ID) {
        return;
    }
    SERVER.with(|s| s.borrow_mut().work.entry(player).or_default().push_back(data.to_vec()));
}

/// The `level_tick_post` hook: the requests of the players in this level, in order, and the answers that fit.
pub(super) fn level_tick(game: &mut Game<'_>, dimension: &str) {
    if game.native_window() || !game.native_on(ID) {
        return;
    }
    let players = game.native_server_players();
    // (the world's host: the first to join. Only theirs keeps the world's weather; anyone may read.)
    let host_player = players.first().map(|p| p.uuid);
    let mine: Vec<u128> = players.iter().filter(|p| p.dimension == dimension).map(|p| p.uuid).collect();
    for player in mine {
        let work = SERVER.with(|s| s.borrow_mut().work.remove(&player)).unwrap_or_default();
        for batch in work {
            let mut r = Reader(&batch);
            if r.u8() != Some(9) {
                continue;
            }
            while let Some(n) = r.u32() {
                let Some(request) = r.take(n as usize) else { break };
                if serve(game, player, Some(player) == host_player, dimension, request).is_none() {
                    log::warn!("[mod {ID}] a bad request");
                }
            }
        }
    }
    // answers leave as the channel allows, the rest next tick
    let ids: Vec<u128> = SERVER.with(|s| s.borrow().answers.keys().copied().collect());
    for player in ids {
        loop {
            let Some(m) = SERVER.with(|s| s.borrow_mut().answers.get_mut(&player).and_then(|q| q.pop_front())) else {
                break;
            };
            if m.len() > 30_000 {
                log::warn!("[mod {ID}] an answer of {} bytes left out", m.len());
                continue;
            }
            if let Err(e) = game.native_net_send_to_player(player, ANSWER, &m) {
                log::debug!("[mod {ID}] answers waiting: {e}");
                SERVER.with(|s| s.borrow_mut().answers.entry(player).or_default().push_front(m));
                break;
            }
        }
    }
    SERVER.with(|s| s.borrow_mut().answers.retain(|_, q| !q.is_empty()));
}

fn answer(player: u128, message: Vec<u8>) {
    SERVER.with(|s| s.borrow_mut().answers.entry(player).or_default().push_back(message));
}

/// Whether `line` is one of the commands Skies runs: vanilla's weather, kept in step with the sky overhead.
fn weather_command(line: &str) -> bool {
    let mut words = line.trim_start_matches('/').split_whitespace();
    words.next() == Some("weather")
        && matches!(words.next(), Some("clear" | "rain" | "thunder"))
        && words.next().is_none_or(|n| n.parse::<u32>().is_ok())
        && words.next().is_none()
}

fn serve(game: &mut Game<'_>, player: u128, host_player: bool, dimension: &str, request: &[u8]) -> Option<()> {
    let mut r = Reader(request);
    match r.u8()? {
        0 => {
            // the world's weather, as the console runs it: the host's only, and only weather
            for line in r.strs()? {
                if host_player && weather_command(&line) {
                    game.native_run_command(line.trim_start_matches('/'));
                }
            }
        }
        3 => {
            let (tag, h, below, above, offset, total) = (r.u32()?, r.u8()?, r.u32()?, r.u32()?, r.u32()?, r.u32()?);
            let columns = r.columns()?;
            let (below, above) = (below.min(64), above.min(64));
            let span = (below + above) as usize;
            let read = game
                .native_read_columns(dimension, &columns, heightmap_of(h)?, (below, above))
                .unwrap_or_else(|_| NativeColumnBlocks {
                    bases: vec![None; columns.len()],
                    blocks: vec![0; columns.len() * span],
                    tints: vec![0; columns.len() * 3],
                    ..NativeColumnBlocks::default()
                });
            let palette_bytes: usize = read.palette.iter().map(|p| p.len() + 4).sum();
            let per = ((MESSAGE.saturating_sub(palette_bytes + 32)) / (5 + 2 * span + 12)).max(1);
            for start in (0..columns.len().max(1)).step_by(per) {
                let end = (start + per).min(columns.len());
                let mut w = Writer::new(11);
                w.u32(tag);
                w.u32(offset + start as u32);
                w.u32(total);
                w.u32(span as u32);
                w.strs(&read.palette);
                w.u32((end.saturating_sub(start)) as u32);
                for i in start..end {
                    let base = read.bases.get(i).copied().flatten();
                    w.0.push(u8::from(base.is_some()));
                    w.i32(base.unwrap_or(0));
                    for j in 0..span {
                        w.0.extend_from_slice(&read.blocks.get(i * span + j).copied().unwrap_or(0).to_le_bytes());
                    }
                    for t in 0..3 {
                        w.u32(read.tints.get(i * 3 + t).copied().unwrap_or(0));
                    }
                }
                answer(player, w.0);
            }
        }
        5 => {
            let (tag, offset, total) = (r.u32()?, r.u32()?, r.u32()?);
            let columns = r.columns()?;
            let (names, info, biome_of) = climate(game, &columns);
            // u16::MAX: none
            let mut w = Writer::new(12);
            w.u32(tag);
            w.u32(offset);
            w.u32(total);
            w.strs(&names);
            for (t, d) in info {
                w.f32(t);
                w.f32(d);
            }
            w.u32(biome_of.len() as u32);
            for b in biome_of {
                w.0.extend_from_slice(&b.unwrap_or(u16::MAX).to_le_bytes());
            }
            answer(player, w.0);
        }
        _ => return None,
    }
    Some(())
}

/// The fork's `world.read-climate`: the biome of each column at sea level from the level's generator (loaded or not,
/// as a map's preview reads it), with each biome's temperature and downfall; none where the level has none.
fn climate(game: &Game<'_>, columns: &[(i32, i32)]) -> (Vec<String>, Vec<(f32, f32)>, Vec<Option<u16>>) {
    let mut names = Vec::new();
    let mut info = Vec::new();
    let mut out = vec![None; columns.len()];
    let Some(terrain) = game.end.locator() else {
        return (names, info, out);
    };
    let quarts: Vec<[i32; 3]> = columns.iter().map(|&(x, z)| [x >> 2, 63 >> 2, z >> 2]).collect();
    let found = terrain.noise_biomes(&quarts);
    let biomes = game.res.biomes();
    let mut seen: BTreeMap<String, u16> = BTreeMap::new();
    for (slot, name) in out.iter_mut().zip(found) {
        let Some(name) = name else { continue };
        let index = match seen.get(&name) {
            Some(&i) => i,
            None => {
                let b = biomes.get(biomes.id(&name));
                let i = names.len() as u16;
                info.push((b.temperature, b.downfall));
                names.push(name.clone());
                seen.insert(name, i);
                i
            }
        };
        *slot = Some(index);
    }
    (names, info, out)
}

// ---- the hooks ----------------------------------------------------------------------------------------------------

/// The `mods` hook: the mod's row on Options > Mods.
pub(super) fn mods(game: &mut Game<'_>, mods: &mut Vec<super::native_hooks::NativeMod>) {
    running(game);
    mods.push(super::native_hooks::NativeMod {
        id: ID.to_owned(),
        title: format!("{NAME} {VERSION}"),
        description: "Regional weather: storms, fronts and showers that live and drift across the world.".to_owned(),
        keys: Vec::new(),
    });
}

pub(super) fn tick(game: &mut Game<'_>) {
    if !running(game) {
        return;
    }
    let heard: Vec<String> = SERVER_COMMANDS.lock().map(|mut c| std::mem::take(&mut *c)).unwrap_or_default();
    for c in heard {
        run_command(game, &c);
    }
    snapshot(game);
    let ticks = TICKS.with(|t| {
        t.set(t.get() + 1);
        t.get()
    });
    (horizon_skies::__EXPORTS.tick)(ticks);
    show_output(game);
    flush_work(game);
}

pub(super) fn frame(game: &mut Game<'_>, elapsed: std::time::Duration) {
    if STARTED.with(Cell::get) != Some(true) || !game.native_window() {
        return;
    }
    (horizon_skies::__EXPORTS.frame)(elapsed.as_secs_f32());
    show_output(game);
}

/// The `shader_params` hook: what the sky lets through, for a Horizon shader pack (`render.set-lighting` as the
/// mod gives it): [0] how much of the sun's light gets through the clouds here, and its colour; [1] the sky's colour
/// at the horizon, and 1 (Skies is here); [2] the sky's colour overhead. Natural Lighting reads them as `frame.mods`.
pub(super) fn shader_params(game: &Game<'_>, params: &mut [[f32; 4]; 8]) {
    if !game.native_window() || !game.native_on(ID) {
        return;
    }
    let Some(l) = host::with(|s| s.lighting) else { return };
    params[0] = [l.direct, l.light.0, l.light.1, l.light.2];
    params[1] = [l.horizon.0, l.horizon.1, l.horizon.2, 1.0];
    params[2] = [l.zenith.0, l.zenith.1, l.zenith.2, 0.0];
}

/// `/skies …`: true when it is the mod's.
pub(super) fn command(game: &mut Game<'_>, command: &str) -> bool {
    let line = command.trim_start_matches('/');
    if line != COMMAND && !line.starts_with("skies ") {
        return false;
    }
    if !game.native_window() {
        if let Ok(mut c) = SERVER_COMMANDS.lock() {
            c.push(line.to_owned());
        }
        return true;
    }
    if !running(game) {
        return false;
    }
    run_command(game, line);
    true
}

fn run_command(game: &mut Game<'_>, line: &str) {
    snapshot(game);
    let args = line.strip_prefix(COMMAND).unwrap_or("").trim_start();
    match (horizon_skies::__EXPORTS.command)(COMMAND, args) {
        Ok(Some(t)) => chat(game, Text::literal(t)),
        Ok(None) => {}
        Err(t) => chat(game, Text::literal(t).with_color(text::RED)),
    }
    show_output(game);
}

/// `/skies`' usages, as the mod's own help gives them: in the server's command tree, so the chat box colours and
/// completes them as the game's own (the window's `command` hook still runs them).
const USAGES: &[&str] = &[
    "skies",
    "skies status",
    "skies storm",
    "skies storm here",
    "skies storm <km:number>",
    "skies storm cell",
    "skies storm cell here",
    "skies storm cell <km:number>",
    "skies storm cluster",
    "skies storm cluster here",
    "skies storm cluster <km:number>",
    "skies storm line",
    "skies storm line here",
    "skies storm line <km:number>",
    "skies storm pulse",
    "skies storm pulse here",
    "skies storm pulse <km:number>",
    "skies shower",
    "skies shower here",
    "skies shower <km:number>",
    "skies front",
    "skies front here",
    "skies front <km:number>",
    "skies overcast",
    "skies overcast here",
    "skies overcast <km:number>",
    "skies fair",
    "skies fair here",
    "skies fair <km:number>",
    "skies clear",
    "skies weather auto",
    "skies weather clear",
    "skies weather fair",
    "skies weather showery",
    "skies weather thundery",
    "skies weather overcast",
    "skies weather rainy",
    "skies wind auto",
    "skies wind <from:word>",
    "skies wind <from:word> <speed:number>",
    "skies speed <times:number>",
    "skies dawn",
    "skies fog",
    "skies aurora",
    "skies rainbow",
    "skies falls",
    "skies bolt",
    "skies bolt <metres:number>",
    "skies sync on",
    "skies sync off",
    "skies quality low",
    "skies quality medium",
    "skies quality high",
    "skies on",
    "skies off",
    "skies reset",
];

/// The `register_commands` hook: `/skies` and its usages in the server's command tree.
pub(super) fn register_commands(game: &mut Game<'_>, commands: &mut Vec<crate::mod_api::NativeCommand>) {
    if game.native_on(ID) {
        commands.extend(USAGES.iter().map(|u| crate::mod_api::NativeCommand::new(u)));
    }
}

/// The `client_level_load` hook.
pub(super) fn client_level_load(_game: &mut Game<'_>, dimension: &str) {
    CLIENT.with(|c| c.borrow_mut().level = Some(dimension.to_owned()));
}

/// The `client_level_unload` hook: leaving the world, the pass stops and the work waiting is dropped (the mod saves
/// its state as it always does, every minute).
pub(super) fn client_level_unload(game: &mut Game<'_>, dimension: &str) {
    let left = CLIENT.with(|c| {
        let mut c = c.borrow_mut();
        let left = c.level.as_deref() == Some(dimension);
        if left {
            c.level = None;
            c.out.clear();
            c.columns.clear();
            c.climate.clear();
        }
        left
    });
    if left && game.native_window() {
        horizon_client_render::skies_post::set(None);
    }
}

/// The `perform_command` hook: `/skies` reaching the server through its command tree (the owner's console,
/// `horizon-ctl command`), handed to the window as the `command` hook hands it.
pub(super) fn perform_command(game: &mut Game<'_>, command: &str, ran: &mut bool) {
    let line = command.trim_start_matches('/');
    if *ran || game.native_window() || (line != COMMAND && !line.starts_with("skies ")) {
        return;
    }
    if let Ok(mut c) = SERVER_COMMANDS.lock() {
        c.push(line.to_owned());
    }
    *ran = true;
}

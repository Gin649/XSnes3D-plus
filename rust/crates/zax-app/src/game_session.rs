//! One game being played: the core, the conversion engine, the Lua runtime and the bridges
//! between them (SPEC-07 §3.1, SPEC-05 REQ-LUA-015).
//!
//! # Ownership
//!
//! The settings model belongs to the host. The Lua runtime keeps a model of its own inside
//! `Setting`, so the session lends the host's model to the runtime for the length of each call
//! that can run a script (`lend`), by exchanging it with a placeholder, and gives it back
//! afterwards.
//!
//! The session owns the core and the script inputs; the host owns the scene and passes it to
//! every call that can run a script. The runtime keeps none of them: each call into the
//! conversion engine that can reach a hook borrows them for its length (`scripted`), through
//! `zax_scripting::lend_context`, so the compiler rules out a conflicting access.

use std::sync::mpsc::{channel, Receiver, Sender};

use zax_engine3d::draw_list::FrameDrawList;
use zax_engine3d::engine::ConversionEngine;
use zax_engine3d::error::EngineError;
use zax_engine3d::palette::Rgb24;
use zax_engine3d::settings::RenderAlgorithm;
use zax_engine3d::vectors::Vec3;
use zax_snes::layers::{self, LayerEngines};
use zax_snes::EngineInput;
use zax_profiles::hash::tiles::TileResolver;
use zax_profiles::profile::GameProfile;
use zax_profiles::setting_application;
use zax_render::scene::SceneState;
use zax_scripting::{
    lend_context, persistent, LuaScriptError, ProfileScriptRuntime, ScriptContext,
    ScriptEnvironment, ScriptInput, ScriptLight, ScriptLog, ScriptMemory, ScriptProfileInfo,
    ScriptSettings,
};
use zax_settings::catalogue::{Port, SettingId};
use zax_settings::model::SettingsModel;

use crate::core_worker::CoreWorker;
use crate::frame_profiler::{Stage, StageClock, Thread};
use crate::input::input_action::InputAction;
use crate::input::routing;
use crate::scene_script_bridge::{self, SceneBackdropBridge, SceneCameraBridge};
use crate::snes::{PadWord, RomLoadResult, SnesCore};

/// Why a session could not be created.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The Lua runtime could not be created.
    #[error(transparent)]
    Script(#[from] LuaScriptError),
    /// The profile cannot be loaded by the engine.
    #[error(transparent)]
    Engine(#[from] EngineError),
}

/// What a session reports to the host after the scripts ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    /// A setting changed, whoever changed it.
    SettingChanged(SettingId),
    /// `Setting:Save()`: write the settings file (REQ-PERS-015).
    SaveSettingsRequested,
    /// `Setting:Load()`: read the settings file again (REQ-PERS-015).
    ReloadSettingsRequested,
}

/// Tells the session an event happened. A closed channel means the session is being dropped, so
/// nobody is left to tell.
fn notify(sender: &Sender<SessionEvent>, event: SessionEvent) {
    if sender.send(event).is_err() {
        // The session, and with it the runtime that calls this, is going away.
    }
}

/// What the scripts read of the inputs and write of the Zapper aim.
#[derive(Debug, Default)]
struct ScriptInputState {
    players: [i32; 2],
    zapper_offset: Vec3,
}

/// The machine as scripts see it, borrowed for one call into the engine: the players' action
/// masks and the core.
struct CoreMachine<'a> {
    core: &'a mut SnesCore,
    input: &'a mut ScriptInputState,
}

impl ScriptInput for CoreMachine<'_> {
    fn player_state(&self, index: usize) -> i32 {
        self.input.players.get(index).copied().unwrap_or(0)
    }

    fn global_state(&self) -> i32 {
        let [one, two] = self.input.players;
        one | two
    }

    // The SNES has a pad in each port: no port byte to read and no Zapper to plug in.
    fn port_byte(&self, _port: usize) -> i32 {
        0
    }

    fn port_type(&self, _port: usize) -> i32 {
        0
    }

    fn set_port_types(&mut self, _first: i32, _second: i32) {}

    fn zapper_offset(&self) -> Vec3 {
        self.input.zapper_offset
    }

    fn set_zapper_offset(&mut self, offset: Vec3) {
        self.input.zapper_offset = offset;
    }
}

impl ScriptMemory for CoreMachine<'_> {
    fn read(&self, address: u16) -> u8 {
        self.core.read_memory(address)
    }

    fn write(&mut self, address: u16, value: u8) {
        self.core.write_memory(address, value);
    }
}

/// Runs `operation` with the scene, the core and the script inputs lent to the script API.
fn with_context<R>(
    scene: &mut SceneState,
    core: &mut SnesCore,
    input: &mut ScriptInputState,
    operation: impl FnOnce(&mut ScriptContext<'_>) -> R,
) -> R {
    let mut camera = SceneCameraBridge(&mut scene.camera);
    let mut backdrop = SceneBackdropBridge(&mut scene.backdrop);
    let mut machine = CoreMachine { core, input };
    operation(&mut ScriptContext {
        camera: &mut camera,
        backdrop: &mut backdrop,
        machine: &mut machine,
    })
}

/// What `GameSession::create` needs to know besides the profile.
pub struct SessionRequest<'a> {
    /// The profile to play.
    pub profile: &'a GameProfile,
    /// The title to show when the profile has none.
    pub title: &'a str,
    /// The scene the camera and backdrop bridges drive.
    pub scene: &'a mut SceneState,
    /// Where `print` and the script errors go.
    pub log: Box<dyn ScriptLog>,
    /// Whether the profile's settings are applied to the model: true for a different game
    /// (REQ-PERS-005), false when the same game is relaunched (REQ-PERS-006).
    pub different_game: bool,
    /// Whether the game is played flat; `None` means "when the profile has no pattern".
    pub two_dimensional_mode: Option<bool>,
    /// Whether this run shows the gamepad-only interface (RO-2 part 4): a Lua write of
    /// `Setting.Msaa` leaves the anti-aliasing alone there.
    pub console_interface: bool,
    /// Which manifest strings of a "3dh" profile have their sixteen bytes, or none for a 3dn
    /// profile (AMD-062 REQ-PROF-080). The cache of REQ-PROF-082 and the walk through the ROM of
    /// REQ-PROF-078 happen before the session is made, so that the patterns are built from what
    /// the ROM gave; what is still waiting is looked for in each displayed frame and in the
    /// cartridge's CHR-RAM.
    pub tiles: Option<TileResolver>,
}

/// The inputs of one emulated frame.
#[derive(Debug, Clone, Copy)]
pub struct SessionFrame {
    /// The first player's actions.
    pub player_one: InputAction,
    /// The second player's actions.
    pub player_two: InputAction,
}

/// What [`GameSession::capture_batch`] keeps of a frame: its layers.
type CapturedBatch = EngineInput;

/// One game being played.
#[allow(clippy::struct_excessive_bools)] // Independent states of the session.
pub struct GameSession {
    core: Box<SnesCore>,
    /// The engine of background layer 1 and the sprites, which runs the profile's scripts.
    engine: ConversionEngine,
    /// The engines of the other layers ([`zax_snes::layers`]).
    layers: LayerEngines,
    /// The layers of the frame converted last, which the draw list puts together.
    converted: Option<EngineInput>,
    /// The draw list of every layer, built from the engines' own.
    merged: FrameDrawList,
    /// The camera's distance from the picture the layers are lined up for.
    layer_distance: f32,
    input: ScriptInputState,
    events: Receiver<SessionEvent>,
    title: String,
    frame_counter: i32,
    /// `emu.pause_3d_in_2d` while the 2D picture is shown ([`GameSession::pause_3d`]).
    paused_3d: bool,
    /// The frame being finished is not the one a catch-up will show
    /// ([`GameSession::set_catching_up`]).
    catching_up: bool,
    /// The last frame captured while the 3D was paused: the
    /// one converted when the 3D comes back.
    skipped_batch: Option<CapturedBatch>,
    started: bool,
    pipeline: FramePipeline,
    /// `video.widescreen_strict_edges`: whether the engine is told where each band of a
    /// widescreen frame stops being drawn, so that a shape cut there stays unfinished.
    strict_wide_edges: bool,
    /// The picture's sides cut for this game (`crate::overscan`): the tiles half or more in them
    /// are left out of the 3D scene.
    overscan: crate::overscan::Overscan,
    /// Run-ahead N, 0 to `SnesCore::MAX_RUN_AHEAD` (AMD-041 REQ-LOOP-064, and `Q-181` for the
    /// bound the owner raised to 10): the frames of a real frame the core runs in
    /// advance of it. 0 — what a freshly created session carries — runs the frame of
    /// REQ-LOOP-060 unchanged, so a host that drives the session itself gets no run-ahead unless
    /// it asks for one (AMD-041 §4, the libretro frontend).
    run_ahead: u8,
    /// Q-103: whether a script of this profile names a member that changes the core from a frame
    /// hook, in which case the **effective** run-ahead of this game is 0 whatever the setting
    /// says. Decided once at profile load ([`scripts_change_the_core`]) and read by
    /// [`GameSession::apply_run_ahead_setting`] and by the two interfaces.
    run_ahead_forced_to_zero: bool,
    /// Whether the effective run-ahead is 0: the scripts' guard-rail above.
    run_ahead_held_at_zero: bool,
    /// Whether no script names a member that reads or drives the core ([`CORE_MEMBERS`]), so
    /// that the core's frames follow from the inputs alone. Decided once at profile load.
    core_follows_the_inputs: bool,
}

/// The state of the "emulation on a separate core" setting (Rust-only, handhelds): the next
/// frame is prepared (input hook, input word) during the host's tick and runs on the emulation
/// thread while the main thread draws the frame before it; the next tick takes the core back
/// and finishes the frame (tile capture, conversion, frame hooks) before anything else.
#[derive(Default)]
struct FramePipeline {
    worker: Option<CoreWorker>,
    /// A core without a ROM that stands in `GameSession::core` while the real one is away.
    spare: Option<Box<SnesCore>>,
    /// The input word of the prepared frame that has not started yet.
    prepared: Option<PadWord>,
    /// Whether the emulation thread holds the core.
    in_flight: bool,
    /// The picture of the last finished frame, kept while the core is away.
    picture: Vec<u8>,
    /// Its rows.
    picture_height: usize,
    /// Its experimental smooth scrolling shift ([`SnesCore::picture_shift`]).
    picture_shift: Option<(f32, i32, i32)>,
    /// Its camera motion to the next frame ([`SnesCore::picture_motion`]).
    picture_motion: Option<f32>,
    /// Its sprites that move with the camera, moved out of it ([`SnesCore::split_steady_sprites`]).
    overlay: Vec<u8>,
    /// Whether the profile's scripts leave the core alone (no input hook, no memory or port
    /// access), so that the next frame may run while the scripts of the last one do
    /// ([`GameSession::launch_ahead`]).
    ahead_allowed: bool,
    /// The palette and edge masks of a frame captured but not converted yet; its tile records
    /// are in `GameSession::batch_records`.
    pending: Option<CapturedBatch>,
}

/// The members of the script API that read or drive the core: a profile whose scripts name
/// none of them cannot tell whether the next frame has already started. `ReadMem` is not one:
/// it reads the RAM copied when the frame came back ([`MemorySnapshot`]).
const CORE_MEMBERS: [&str; 6] = [
    "InputProcess",
    "WriteMem",
    "GetInput",
    "GetInputType",
    "SetInputType",
    "ZapperEnable",
];

/// Whether no script of `profile` names a member of [`CORE_MEMBERS`].
fn scripts_leave_the_core_alone(profile: &GameProfile) -> bool {
    profile.scripts.iter().all(|script| {
        script
            .source
            .as_deref()
            .is_none_or(|source| CORE_MEMBERS.iter().all(|member| !source.contains(member)))
    })
}

/// The members of [`CORE_MEMBERS`] through which a **frame** hook can change the core, so that
/// what it writes would be thrown away by step 5 of REQ-LOOP-065 — the guard-rail Q-103's answer
/// asks for. A documented subset, kept here beside the whole list so the two are read together:
///
/// - `WriteMem` writes the bus (`ctx.machine.write`), which the in-memory state of REQ-CORE-121
///   holds and the restore puts back;
/// - `SetInputType` writes the core's controller ports (`ctx.machine.set_port_types`), also in
///   that state, and its other half — the setting — would persist, leaving the two disagreeing;
/// - `GetInput`, `GetInputType` and `ZapperEnable` only read, so they are not concerned;
/// - `InputProcess` runs at step (a), **before** the real frame, so what it writes is part of the
///   real state, as the answer of Q-103 says itself.
///
/// The Zapper aim a script writes is not concerned either: it lives in `ScriptInputState`, on this
/// side of the core.
// Q-103
const CORE_CHANGING_MEMBERS: [&str; 2] = ["WriteMem", "SetInputType"];

/// Whether a script of `profile` names a member of [`CORE_CHANGING_MEMBERS`] (Q-103). The same
/// kind of text test as [`scripts_leave_the_core_alone`], with the same accepted false positive —
/// the name in a comment counts — because the result stays safe: run-ahead off.
fn scripts_change_the_core(profile: &GameProfile) -> bool {
    profile.scripts.iter().any(|script| {
        script.source.as_deref().is_some_and(|source| {
            CORE_CHANGING_MEMBERS
                .iter()
                .any(|member| source.contains(member))
        })
    })
}

impl GameSession {
    /// Creates the session of a profile: the Lua chunks run first (REQ-LUA-015 step 1), then the
    /// profile's settings reach the model (step 2), then the patterns and their `BuildPattern`
    /// hooks (step 3). `model` is the host's settings model.
    ///
    /// # Errors
    /// The runtime could not be installed, or a pattern of the profile is invalid.
    pub fn create(
        request: SessionRequest<'_>,
        model: &mut SettingsModel,
    ) -> Result<Self, SessionError> {
        let SessionRequest {
            profile,
            title,
            scene,
            log,
            different_game,
            two_dimensional_mode,
            console_interface,
            tiles,
        } = request;
        let mut core = SnesCore::new();
        let mut input = ScriptInputState::default();
        let mut engine = ConversionEngine::new();
        engine.use_geometry();
        engine.two_dimensional_mode = two_dimensional_mode.unwrap_or(
            profile.permanent_patterns.is_empty() && profile.temporary_patterns.is_empty(),
        );
        let (sender, events) = channel();
        let optimized = model.get(SettingId::EmuLuaOptimized).as_bool();
        let experimental = model.get(SettingId::EmuLuaExperimental).as_bool();
        let mut settings = ScriptSettings::new(
            std::mem::replace(model, SettingsModel::new()),
            ScriptLight::Model,
        );
        // RO-2 part 4: read once, when the session is made. The desktop's F1 switches the
        // presentation mid-game, which is not a change of platform and does not change this.
        settings.set_console_interface(console_interface);
        let changes = sender.clone();
        settings.set_on_changed(move |id| notify(&changes, SessionEvent::SettingChanged(id)));
        let saves = sender.clone();
        settings.set_on_save_requested(move || notify(&saves, SessionEvent::SaveSettingsRequested));
        settings
            .set_on_load_requested(move || notify(&sender, SessionEvent::ReloadSettingsRequested));
        // The scene starts from the settings as they stand; the profile's own then reach it
        // through the changes the model reports.
        scene_script_bridge::push_settings(settings.model(), scene);
        let environment = ScriptEnvironment {
            settings,
            log,
            profile: ScriptProfileInfo {
                title: profile.title.clone().unwrap_or_else(|| title.to_owned()),
                maker: profile.author.clone().unwrap_or_default(),
                game_dev: profile.publisher.clone().unwrap_or_default(),
                description: profile.description.clone().unwrap_or_default(),
                id: profile.id.clone().unwrap_or_default(),
                version: profile.version_text.clone().unwrap_or_default(),
            },
            optimized,
            experimental,
            ..ScriptEnvironment::default()
        };
        // REQ-LUA-015 step 1: every chunk runs before anything else, with the scene and the
        // machine at hand.
        let runtime = with_context(scene, &mut core, &mut input, |context| {
            ProfileScriptRuntime::create_in(profile, environment, engine.state_mut(), Some(context))
        })?;
        let light = runtime.light_update_s_scripts();
        if !light.is_empty() {
            crate::app_log::line(&format!(
                "XSnes3D+: Lua: the UpdateS of {} take(s) the light shape.",
                light.join(", ")
            ));
        }
        engine.set_script_host(Box::new(runtime));
        let mut session = Self {
            core: Box::new(core),
            engine,
            layers: LayerEngines::new(),
            converted: None,
            merged: FrameDrawList::default(),
            layer_distance: zax_render::geometry::reset_distance(scene.camera.fov()),
            input,
            events,
            title: title.to_owned(),
            frame_counter: 0,
            paused_3d: false,
            catching_up: false,
            skipped_batch: None,
            started: false,
            strict_wide_edges: false,
            overscan: crate::overscan::Overscan::default(),
            run_ahead: 0,
            run_ahead_forced_to_zero: false,
            run_ahead_held_at_zero: false,
            core_follows_the_inputs: false,
            pipeline: FramePipeline::default(),
        };
        session.pipeline.ahead_allowed = scripts_leave_the_core_alone(profile);
        session.core_follows_the_inputs = session.pipeline.ahead_allowed;
        // Q-103: the guard-rail is decided here, once, and nothing of it reaches the frame loop.
        session.run_ahead_forced_to_zero = scripts_change_the_core(profile);
        session.run_ahead_held_at_zero = session.run_ahead_forced_to_zero;
        if session.run_ahead_forced_to_zero {
            crate::app_log::line(
                "XSnes3D+: run-ahead: the scripts of this profile change the core from a frame \
                 hook, so the game runs at run-ahead 0 (Q-103); the setting is kept for the next \
                 game.",
            );
        }
        // The runtime holds the model from here until the last line of this function.
        let loaded = session.load_profile(profile, different_game, scene, tiles);
        session.exchange_model(model);
        loaded?;
        Ok(session)
    }

    /// REQ-LUA-015 step 2, REQ-LOOP-021 step 2 and REQ-PERS-005: the profile's P settings land in
    /// the model, which mirrors them into the engine slice and into the scene. This runs after
    /// the chunks, so an assignment made at the main level of a chunk is overwritten, as
    /// REQ-LUA-015 demands. Relaunching the same game skips it, so the scripts' changes survive
    /// (REQ-PERS-006).
    fn load_profile(
        &mut self,
        profile: &GameProfile,
        different_game: bool,
        scene: &mut SceneState,
        tiles: Option<TileResolver>,
    ) -> Result<(), SessionError> {
        let ports = self
            .engine
            .with_script_host::<ProfileScriptRuntime, _>(|runtime, _| {
                if different_game {
                    let values = profile
                        .settings
                        .as_ref()
                        .map(setting_application::to_setting_values)
                        .unwrap_or_default();
                    runtime.settings_mut().model_mut().apply_profile(values);
                }
                // Both ports go back to Gamepad at every load of a different game (REQ-PERS-012,
                // DEC-09-Q6); the input layer stays the authority and takes its types from the model.
                let model = runtime.settings().model();
                [
                    model.get(SettingId::InputPortType(Port::ALL[0])).as_int(),
                    model.get(SettingId::InputPortType(Port::ALL[1])).as_int(),
                ]
            });
        if let Some([first, second]) = ports {
            CoreMachine {
                core: &mut self.core,
                input: &mut self.input,
            }
            .set_port_types(first, second);
        }
        // Step 3: the patterns with their BuildPattern hooks. The settings were applied above,
        // through the model, so the engine must not copy them a second time.
        self.sync_runtime_settings(scene);
        let mut loaded = Ok(());
        let mut tiles = tiles;
        self.scripted(scene, |engine| {
            // AMD-062 REQ-PROF-080: from here on the conversion looks for the tiles that are
            // still waiting, in the frame it converts and in the cartridge's CHR-RAM.
            loaded = engine.load_profile_with_tiles(profile, false, tiles.take());
        });
        loaded?;
        self.sync_runtime_settings(scene);
        Ok(())
    }

    /// Runs `operation` on the engine with the scene, the core and the script inputs lent to the
    /// script API for its length. The borrows end with the call: the runtime keeps none.
    fn scripted(&mut self, scene: &mut SceneState, operation: impl FnOnce(&mut ConversionEngine)) {
        let Self {
            core,
            input,
            engine,
            ..
        } = self;
        with_context(scene, core, input, |context| {
            lend_context(engine, |engine| engine, context, operation);
        });
    }

    /// Runs `operation` on the Lua runtime and the conversion state it works on, or returns `None`
    /// when the session has no runtime. The scripts it runs see `scene` and the core. Used by the
    /// acceptance harness to evaluate chunks; the settings model is not lent, so what a chunk
    /// writes to the settings stays in the runtime.
    pub fn with_runtime<R>(
        &mut self,
        scene: &mut SceneState,
        operation: impl FnOnce(
            &mut ProfileScriptRuntime,
            &mut zax_engine3d::engine::ConversionState,
        ) -> R,
    ) -> Option<R> {
        let mut result = None;
        self.scripted(scene, |engine| {
            result = engine.with_script_host::<ProfileScriptRuntime, R>(operation);
        });
        result
    }

    fn sync_runtime_settings(&mut self, scene: &mut SceneState) {
        self.with_runtime(scene, ProfileScriptRuntime::sync_settings);
    }

    fn exchange_model(&mut self, model: &mut SettingsModel) {
        self.engine
            .with_script_host::<ProfileScriptRuntime, _>(|runtime, _| {
                std::mem::swap(runtime.settings_mut().model_mut(), model);
            });
    }

    /// Lends the host's `model` to the Lua runtime for the length of `operation`, then mirrors
    /// what the scripts changed and gives the model back.
    fn lend<R>(
        &mut self,
        model: &mut SettingsModel,
        scene: &mut SceneState,
        operation: impl FnOnce(&mut Self, &mut SceneState) -> R,
    ) -> R {
        self.exchange_model(model);
        let result = operation(self, scene);
        self.sync_runtime_settings(scene);
        self.exchange_model(model);
        result
    }

    /// Whether the core is here, not away running a frame on the emulation thread (where
    /// [`GameSession::core`] is a stand-in).
    #[must_use]
    pub const fn core_is_home(&self) -> bool {
        !self.pipeline.in_flight
    }

    /// The core, for reading.
    #[must_use]
    pub fn core(&self) -> &SnesCore {
        &self.core
    }

    /// The core, for writing.
    #[must_use]
    pub fn core_mut(&mut self) -> &mut SnesCore {
        &mut self.core
    }

    /// Sets `video.widescreen_strict_edges` (see the field `strict_wide_edges`).
    pub fn set_strict_wide_edges(&mut self, strict: bool) {
        self.strict_wide_edges = strict;
    }

    /// Sets the picture's sides cut for this game (see the field `overscan`).
    pub fn set_overscan(&mut self, overscan: crate::overscan::Overscan) {
        self.overscan = overscan;
    }

    /// The conversion engine.
    #[must_use]
    pub const fn engine(&self) -> &ConversionEngine {
        &self.engine
    }

    /// The conversion engine, for writing.
    pub fn engine_mut(&mut self) -> &mut ConversionEngine {
        &mut self.engine
    }

    /// The title of the game.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Emulated frames since the game started or was reset.
    #[must_use]
    pub const fn frame_counter(&self) -> i32 {
        self.frame_counter
    }

    /// The Lua runtime.
    #[must_use]
    pub fn runtime(&self) -> Option<&ProfileScriptRuntime> {
        self.engine.script_host_as::<ProfileScriptRuntime>()
    }

    /// Puts the scripts' Lua collector in its smooth or its original tuning (see
    /// `ProfileScriptRuntime::set_smooth_gc`).
    pub fn set_smooth_gc(&self, smooth: bool) {
        if let Some(runtime) = self.runtime() {
            runtime.set_smooth_gc(smooth);
        }
    }

    /// The names of the scripts, in hook order.
    #[must_use]
    pub fn script_names(&self) -> Vec<String> {
        self.runtime()
            .map(|runtime| {
                runtime
                    .scripts()
                    .iter()
                    .map(|script| script.name().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Nominal frames per second of the core's region.
    #[must_use]
    pub fn nominal_frame_rate(&self) -> i32 {
        self.core.nominal_frame_rate()
    }

    /// Loads a ROM image into the core (REQ-LOOP-021 step 4).
    pub fn load_rom(&mut self, image: &[u8], name: &str) -> RomLoadResult {
        let result = self.core.load_rom(image, name);
        if result.success {
            self.frame_counter = 0;
        }
        result
    }

    /// Whether a ROM is loaded.
    #[must_use]
    pub fn has_rom(&self) -> bool {
        self.pipeline.in_flight || self.core.is_running()
    }

    /// Whether the next frame may run on the emulation thread while the scripts of the last one
    /// run here: the profile's scripts leave the core alone.
    ///
    /// **Not** the run-ahead of AMD-041 ([`GameSession::set_run_ahead`]): this is the Rust-only
    /// "emulation ahead" of `docs/PERFORMANCE-HISTORY.md` §5.5, a pipelining choice with no
    /// visible latency change. The two are orthogonal and compose.
    #[must_use]
    pub const fn can_run_ahead(&self) -> bool {
        self.pipeline.ahead_allowed
    }

    /// Sets the run-ahead of AMD-041 REQ-LOOP-064: the number of frames the core runs in advance
    /// of the real one, brought into `[0, SnesCore::MAX_RUN_AHEAD]` (`Q-181`).
    ///
    /// The loop writes it before every real frame, which is how "applied at the next real frame"
    /// is honoured: the value is a property of the session rather than an argument of
    /// [`GameSession::run_frame`], so a frame already under way is never cut in half.
    ///
    /// This is the **raw** setter — the guard-rail of Q-103 lives in
    /// [`GameSession::apply_run_ahead_setting`], which is what the loop calls. A host that drives
    /// the session itself (the frame steps, the tests, a frontend) gets exactly the N it asks for.
    pub fn set_run_ahead(&mut self, frames: i32) {
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)] // Clamped to the domain.
        {
            self.run_ahead = frames.clamp(0, i32::from(SnesCore::MAX_RUN_AHEAD)) as u8;
        }
    }

    /// The run-ahead the user's setting asks for, with the guard-rail of Q-103 applied: a profile
    /// whose scripts change the core from a frame hook runs at **0** whatever the setting says,
    /// because step 5 of REQ-LOOP-065 would throw that change away (REQ-LOOP-066).
    ///
    /// The setting itself is left alone — nothing is written and nothing is persisted — so the
    /// next game gets the user's value back. This is the one site the loop calls, where it already
    /// reads the setting once per emulated frame; the guard costs one `bool` test there and
    /// nothing anywhere else.
    // Q-103
    pub fn apply_run_ahead_setting(&mut self, frames: i32) {
        self.set_run_ahead(if self.run_ahead_held_at_zero {
            0
        } else {
            frames
        });
    }

    /// Q-103: whether this game's run-ahead is forced to 0 by its own scripts, which is what the
    /// two interfaces show beside the Run-ahead setting while it is playing.
    #[must_use]
    pub const fn forces_run_ahead_to_zero(&self) -> bool {
        self.run_ahead_forced_to_zero
    }

    /// The run-ahead in force (AMD-041 REQ-LOOP-064) — the **effective** one, so 0 for a game the
    /// guard-rail of Q-103 holds back.
    #[must_use]
    pub const fn run_ahead(&self) -> u8 {
        self.run_ahead
    }

    /// Step 5 of REQ-LOOP-065: the real state goes back once everything that reads the displayed
    /// frame has read it.
    ///
    /// **Invariant**: every public entry point of the session that runs a frame leaves the core
    /// in the **real** state. That is what makes REQ-LOOP-066 true by construction — the rollback
    /// snapshots, the frame steps, the savestates, the input recording and the achievements all
    /// read `GameSession::core()` between two frames and can never see a frame in advance.
    ///
    /// Q-103: the same invariant means a `Script:WriteMem` called from a **frame** hook writes
    /// into the displayed state and is lost here, which is REQ-LOOP-066 read to the letter and
    /// what the .NET implementation of AMD-041 does too. The input hook `InputProcess` runs at
    /// step (a), before the real frame, so it is not concerned.
    ///
    /// R0's answer keeps that — the write is **not** replayed, so what happens here is right —
    /// and adds the guard-rail that stops a profile from losing its writes silently: a profile
    /// whose scripts name a member of [`CORE_CHANGING_MEMBERS`] runs at an **effective** run-ahead
    /// of 0, the user's setting kept ([`GameSession::apply_run_ahead_setting`], decided once at
    /// profile load by [`scripts_change_the_core`], with a note beside the setting in both
    /// interfaces and a line in the log). So this restore only ever loses the writes of a profile
    /// the text test did not see — one that reaches the member without naming it.
    ///
    /// REQ-LOOP-067 allows the restore to be delayed past step (d), and in this application that
    /// is the placement that needs no copy at all: the conversion pulls `core.frame_batch()`
    /// after the frame rather than through the video callback, the frame hooks read the live core
    /// through `Script:ReadMem`, and the 2D picture is composed from the core's own pixels and
    /// records. All three therefore see the displayed frame, and this runs after them. With
    /// run-ahead off it is one `bool` test and nothing else.
    fn put_the_real_frame_back(&mut self, clock: &mut StageClock) {
        if !self.core.holds_a_real_frame() {
            return;
        }
        let started = clock.start();
        self.core.restore_real_frame();
        clock.stop(Stage::Core, started);
    }

    /// Takes the core back from the emulation thread, if it is away, and captures the frame it
    /// ran (tile records, palette, picture) for [`GameSession::finish_captured`]: the core is
    /// free again for the host, the conversion of the frame comes later. "Emulation ahead".
    pub fn capture_in_flight(&mut self, clock: &mut StageClock) {
        if !self.pipeline.in_flight {
            return;
        }
        self.pipeline.in_flight = false;
        let Some(finished) = self.pipeline.worker.as_ref().and_then(CoreWorker::finish) else {
            crate::app_log::error("XSnes3D+: the emulation thread stopped during a frame.");
            return;
        };
        self.pipeline.spare = Some(std::mem::replace(&mut self.core, finished.core));
        clock.add(Stage::Core, finished.elapsed);
        clock.add_thread(Thread::Emulation, finished.elapsed);
        let captured = self.capture_batch(clock);
        self.pipeline.pending = Some(captured);
        self.pipeline.picture_height = self.core.wide_picture_into(&mut self.pipeline.picture);
        self.pipeline.picture_shift = self.core.picture_shift();
        self.pipeline.picture_motion = self.core.picture_motion();
        self.core
            .split_steady_sprites(&mut self.pipeline.picture, &mut self.pipeline.overlay);
        // Step 5 of REQ-LOOP-065 on the emulation-ahead path: the three things this path reads
        // from the displayed frame — its batch, the RAM copy the scripts' `ReadMem` will serve
        // (REQ-LOOP-067) and its 2D picture — have just been taken, and the next frame has not
        // been launched yet, so the real state goes back here.
        self.put_the_real_frame_back(clock);
    }

    /// Starts the next frame on the emulation thread at once ("emulation ahead"): its input word
    /// (the scripts have no input hook) and the core leave now, and the frame captured before it
    /// is converted afterwards by [`GameSession::finish_captured`], while it runs.
    pub fn launch_ahead(
        &mut self,
        model: &mut SettingsModel,
        scene: &mut SceneState,
        frame: SessionFrame,
        clock: &mut StageClock,
    ) {
        if clock.enabled() != self.engine.last_hook_time().is_some() {
            self.engine.set_hook_timing(clock.enabled());
        }
        let word = self.lend(model, scene, |session, scene| {
            session.begin_frame_lent(scene, frame, clock)
        });
        self.pipeline.prepared = Some(word);
        self.launch_prepared();
        if !self.pipeline.in_flight {
            // No thread: the frame captured before is converted first, then this one runs here
            // and is captured like one that came back.
            if let Some(word) = self.pipeline.prepared.take() {
                self.finish_captured(model, scene, clock);
                let started = clock.start();
                self.core.run_frame_ahead(word, self.run_ahead);
                clock.stop(Stage::Core, started);
                let captured = self.capture_batch(clock);
                self.pipeline.pending = Some(captured);
                self.pipeline.picture_height =
                    self.core.wide_picture_into(&mut self.pipeline.picture);
                self.pipeline.picture_shift = self.core.picture_shift();
                self.pipeline.picture_motion = self.core.picture_motion();
                self.core
                    .split_steady_sprites(&mut self.pipeline.picture, &mut self.pipeline.overlay);
                // Step 5 here too, so that the invariant below holds on this path as well. The
                // frame hooks of this frame run from `finish_captured`, just after this returns,
                // and on this path alone they read the real frame rather than the displayed one
                // (the memory snapshot of REQ-LOOP-067 is only served while the core is away).
                // The path is taken only when the operating system refused the emulation thread.
                self.put_the_real_frame_back(clock);
            }
        }
    }

    /// Converts the frame [`GameSession::capture_in_flight`] captured, if any: conversion, frame
    /// hooks, camera tracking, publication.
    pub fn finish_captured(
        &mut self,
        model: &mut SettingsModel,
        scene: &mut SceneState,
        clock: &mut StageClock,
    ) {
        let Some(captured) = self.pipeline.pending.take() else {
            return;
        };
        self.lend(model, scene, |session, scene| {
            session.convert_batch_lent(scene, clock, captured);
        });
    }

    /// The conversion a pipelined frame left to its own thread ([`EmulationHost`]'s
    /// `defer_conversion`): the captured frame is converted, then its draw list is built.
    ///
    /// [`EmulationHost`]: crate::emulation::host::EmulationHost
    pub fn convert_deferred(
        &mut self,
        model: &mut SettingsModel,
        scene: &mut SceneState,
        clock: &mut StageClock,
    ) {
        self.finish_captured(model, scene, clock);
        let started = clock.start();
        self.build_draw_list();
        clock.stop(Stage::DrawList, started);
    }

    /// Whether a frame was captured and waits for its conversion.
    #[must_use]
    pub const fn has_captured_frame(&self) -> bool {
        self.pipeline.pending.is_some()
    }

    /// Swaps the draw list built last with `list`, which the caller then draws.
    pub fn exchange_draw_list(&mut self, list: &mut zax_engine3d::draw_list::FrameDrawList) {
        std::mem::swap(&mut self.merged, list);
    }

    /// Puts the frame counter back to zero.
    pub const fn reset_frame_counter(&mut self) {
        self.frame_counter = 0;
    }

    /// The soft reset of AMD-044 REQ-CORE-122 and REQ-LOOP-072: the core imitates a press on
    /// the RESET button and the frame counter goes back to 0, but the profile is **not**
    /// relaunched — no `End`, no `Start`, the Lua state and `PData` untouched.
    ///
    /// The converted frames in flight belong to before the reset and are dropped, as they are
    /// on a relaunch and on a savestate restore (REQ-E3D-281); the learned conversion state is
    /// kept, which is what distinguishes this from a relaunch. Called with the core home, like
    /// every other command of the tick.
    pub fn soft_reset(&mut self) {
        self.core.soft_reset();
        self.frame_counter = 0;
        self.engine.clear_frames();
        self.layers.clear_frames();
    }

    /// Restores a core state block, clearing the engine's frames (they described the old
    /// state).
    ///
    /// # Errors
    /// The text of the refusal.
    pub fn restore_core(&mut self, block: &[u8]) -> Result<(), String> {
        let restored = self
            .core
            .try_load_state(block, 0)
            .map(|_| ())
            .map_err(|error| error.to_string());
        if restored.is_ok() {
            self.engine.clear_frames();
            self.layers.clear_frames();
        }
        restored
    }

    /// Whether no script of this profile reads or drives the core ([`CORE_MEMBERS`]).
    #[must_use]
    pub const fn core_follows_the_inputs(&self) -> bool {
        self.core_follows_the_inputs
    }

    /// Whether a script of this profile changes the core from a frame hook (Q-103).
    #[must_use]
    pub const fn scripts_change_the_core(&self) -> bool {
        self.run_ahead_forced_to_zero
    }

    /// Runs the `Start` hooks, once.
    pub fn start(&mut self, model: &mut SettingsModel, scene: &mut SceneState) {
        if self.started {
            return;
        }
        self.started = true;
        self.lend(model, scene, |session, scene| {
            session.with_runtime(scene, ProfileScriptRuntime::invoke_start);
        });
    }

    /// Runs the `End` hooks and resets what a game leaves in the scene and the settings.
    pub fn end(&mut self, model: &mut SettingsModel, scene: &mut SceneState) {
        if !self.started {
            return;
        }
        self.started = false;
        self.lend(model, scene, |session, scene| {
            session.with_runtime(scene, ProfileScriptRuntime::invoke_end);
            scene.backdrop.reset();
            session.with_runtime(scene, |runtime, _| runtime.settings_mut().end_game());
        });
    }

    /// Resets the game: `End`, a power cycle of the core, `Start`.
    pub fn reset(&mut self, model: &mut SettingsModel, scene: &mut SceneState) {
        self.end(model, scene);
        self.core.reset();
        self.frame_counter = 0;
        self.start(model, scene);
    }

    /// The scripts' state for a save state: runs `Save`, then reads `PData`.
    ///
    /// # Errors
    /// The Lua state could not be read.
    pub fn capture_script_state(
        &mut self,
        model: &mut SettingsModel,
        scene: &mut SceneState,
    ) -> Result<Vec<u8>, LuaScriptError> {
        self.lend(model, scene, |session, scene| {
            session
                .with_runtime(scene, |runtime, state| {
                    runtime.invoke_save(state);
                    runtime
                        .capture_persistent_data()
                        .map(|block| block.to_vec())
                })
                .unwrap_or_else(|| Ok(Vec::new()))
        })
    }

    /// The 64 `PData` values a rollback snapshot carries (AMD-042 REQ-LOOP-069), read the way
    /// `GetPData(i)` reads them. The `Save` hook is **not** called: it stays reserved for
    /// savestates and for serialisation (REQ-LOOP-070, REQ-LUA-075).
    ///
    /// Nothing is lent to the Lua state and no script runs, so this is a read of 64 table
    /// entries and nothing more — it sits in the frame loop, once every 60 frames.
    pub fn capture_rollback_data(&mut self) -> Option<[u8; persistent::BLOCK_SIZE]> {
        self.engine
            .with_script_host::<ProfileScriptRuntime, _>(|runtime, _| {
                runtime.capture_persistent_data().ok()
            })
            .flatten()
    }

    /// The value of `PData[index]`, as `GetPData(index - 1)` reads it, or `None` without a Lua
    /// runtime. No script runs and nothing is lent.
    pub fn read_persistent_data(&mut self, index: i64) -> Option<f64> {
        self.engine
            .with_script_host::<ProfileScriptRuntime, _>(|runtime, _| {
                runtime.read_persistent_data(index).ok()
            })
            .flatten()
    }

    /// Writes the 64 `PData` values of a rollback snapshot back, exactly as `SetPData` would
    /// (AMD-042 REQ-LOOP-070). The `Load` hook is **not** called.
    pub fn restore_rollback_data(&mut self, block: &[u8]) {
        self.engine
            .with_script_host::<ProfileScriptRuntime, _>(|runtime, _| {
                let _ = runtime.restore_persistent_data(block);
            });
    }

    /// Puts the scripts' state back and runs `Start` then `Load` (REQ-LUA-072, DEC-R2-T6).
    ///
    /// # Errors
    /// The Lua state could not be written.
    pub fn complete_script_restore(
        &mut self,
        model: &mut SettingsModel,
        scene: &mut SceneState,
        block: &[u8],
    ) -> Result<(), LuaScriptError> {
        let restored = self
            .with_runtime(scene, |runtime, _| runtime.restore_persistent_data(block))
            .unwrap_or(Ok(()));
        self.frame_counter = 0;
        self.start(model, scene);
        self.lend(model, scene, |session, scene| {
            session.with_runtime(scene, ProfileScriptRuntime::invoke_load);
        });
        restored
    }

    /// The `retro_unserialize` path of REQ-LUA-075 and DEC-R2-D5: the profile stays loaded, so
    /// the entries go back and `Load` runs, with no `End` and no `Start`.
    ///
    /// # Errors
    /// The Lua state could not be written.
    pub fn restore_script_state_in_place(
        &mut self,
        model: &mut SettingsModel,
        scene: &mut SceneState,
        block: &[u8],
    ) -> Result<(), LuaScriptError> {
        let restored = self
            .with_runtime(scene, |runtime, _| runtime.restore_persistent_data(block))
            .unwrap_or(Ok(()));
        self.lend(model, scene, |session, scene| {
            session.with_runtime(scene, ProfileScriptRuntime::invoke_load);
        });
        restored
    }

    /// Runs one emulated frame: the script pad, the core, the conversion, the publication.
    ///
    /// The script pad runs before the core sees the frame and is additive: it adds bits to the
    /// player it is attached to and can never mask a physical input (REQ-INP-071, 072).
    pub fn run_frame(
        &mut self,
        model: &mut SettingsModel,
        scene: &mut SceneState,
        player_one: InputAction,
        player_two: InputAction,
    ) {
        let frame = SessionFrame {
            player_one,
            player_two,
        };
        self.run_frame_timed(model, scene, frame, &mut StageClock::default());
    }

    /// [`GameSession::run_frame`] with its stages timed on `clock` when the clock is on (the
    /// frame profiler): core, tile-record capture, conversion and Lua hooks.
    pub fn run_frame_timed(
        &mut self,
        model: &mut SettingsModel,
        scene: &mut SceneState,
        frame: SessionFrame,
        clock: &mut StageClock,
    ) {
        self.complete_frame_timed(model, scene, clock);
        if clock.enabled() != self.engine.last_hook_time().is_some() {
            self.engine.set_hook_timing(clock.enabled());
        }
        self.lend(model, scene, |session, scene| {
            let word = session.begin_frame_lent(scene, frame, clock);
            let started = clock.start();
            // REQ-LOOP-065: the real frame and the frames in advance, step (b) of REQ-LOOP-060.
            session.core.run_frame_ahead(word, session.run_ahead);
            clock.stop(Stage::Core, started);
            // Steps (c) and (d), on the displayed frame (REQ-LOOP-067).
            session.finish_frame_lent(scene, clock);
        });
        // Step 5, once (c) and (d) are over.
        self.put_the_real_frame_back(clock);
    }

    /// Prepares the next frame for the emulation thread ("emulation on a separate core"): the
    /// frame before it is finished first, then the input hook of the scripts runs and the input
    /// word is computed, as at the start of [`GameSession::run_frame_timed`]. The frame itself
    /// runs from [`GameSession::launch_prepared`] and is finished by
    /// [`GameSession::complete_frame_timed`].
    pub fn prepare_frame_timed(
        &mut self,
        model: &mut SettingsModel,
        scene: &mut SceneState,
        frame: SessionFrame,
        clock: &mut StageClock,
    ) {
        self.complete_frame_timed(model, scene, clock);
        if clock.enabled() != self.engine.last_hook_time().is_some() {
            self.engine.set_hook_timing(clock.enabled());
        }
        let word = self.lend(model, scene, |session, scene| {
            session.begin_frame_lent(scene, frame, clock)
        });
        self.pipeline.prepared = Some(word);
    }

    /// Hands the core and the prepared frame to the emulation thread, which is started the first
    /// time. Without a thread (the system refused one) the frame stays prepared and runs on the
    /// calling thread when it is completed.
    pub fn launch_prepared(&mut self) {
        let Some(word) = self.pipeline.prepared.take() else {
            return;
        };
        if self.pipeline.worker.is_none() {
            match CoreWorker::spawn() {
                Ok(worker) => {
                    self.pipeline.worker = Some(worker);
                    self.pipeline.spare = Some(Box::new(SnesCore::new()));
                }
                Err(error) => {
                    crate::app_log::error(&format!(
                        "XSnes3D+: no emulation thread ({error}); the emulation stays on the main thread."
                    ));
                }
            }
        }
        let (Some(worker), Some(spare)) = (&self.pipeline.worker, self.pipeline.spare.take())
        else {
            self.pipeline.prepared = Some(word);
            return;
        };

        // The 2D mode shows the core's picture while the next frame runs (emulation ahead has
        // kept the picture of the frame it captured).
        if self.pipeline.pending.is_none() {
            self.pipeline.picture_height = self.core.wide_picture_into(&mut self.pipeline.picture);
            self.pipeline.picture_shift = self.core.picture_shift();
            self.pipeline.picture_motion = self.core.picture_motion();
            self.core
                .split_steady_sprites(&mut self.pipeline.picture, &mut self.pipeline.overlay);
        }
        let core = std::mem::replace(&mut self.core, spare);
        match worker.start(core, word, self.run_ahead) {
            Ok(()) => self.pipeline.in_flight = true,
            Err(core) => {
                self.pipeline.spare = Some(std::mem::replace(&mut self.core, core));
                self.pipeline.prepared = Some(word);
            }
        }
    }

    /// Finishes the frame [`GameSession::prepare_frame_timed`] prepared, if any: takes the core
    /// back from the emulation thread (or runs the frame here when it never left), then captures
    /// the tile records, converts the frame and runs the frame hooks. Whatever reads or changes
    /// the core calls this first.
    pub fn complete_frame_timed(
        &mut self,
        model: &mut SettingsModel,
        scene: &mut SceneState,
        clock: &mut StageClock,
    ) {
        if self.pipeline.in_flight {
            self.pipeline.in_flight = false;
            let Some(finished) = self.pipeline.worker.as_ref().and_then(CoreWorker::finish) else {
                crate::app_log::error("XSnes3D+: the emulation thread stopped during a frame.");
                return;
            };
            self.pipeline.spare = Some(std::mem::replace(&mut self.core, finished.core));
            clock.add(Stage::Core, finished.elapsed);
            clock.add_thread(Thread::Emulation, finished.elapsed);
        } else if let Some(word) = self.pipeline.prepared.take() {
            // A frame captured ahead of this one is converted first: its records are pending.
            self.finish_captured(model, scene, clock);
            let started = clock.start();
            self.core.run_frame_ahead(word, self.run_ahead);
            clock.stop(Stage::Core, started);
        } else {
            self.finish_captured(model, scene, clock);
            return;
        }
        // The frame before the one that came back, captured ahead, is converted first.
        self.finish_captured(model, scene, clock);
        self.lend(model, scene, |session, scene| {
            session.finish_frame_lent(scene, clock);
        });
        // Step 5 of REQ-LOOP-065, after the conversion and the frame hooks of this frame.
        self.put_the_real_frame_back(clock);
    }

    /// The scripts' input hook and the input word of the next frame (the first half of a frame).
    fn begin_frame_lent(
        &mut self,
        scene: &mut SceneState,
        frame: SessionFrame,
        clock: &mut StageClock,
    ) -> PadWord {
        let SessionFrame {
            player_one,
            player_two,
        } = frame;
        let counter = self.frame_counter;
        let started = clock.start();
        #[allow(clippy::cast_precision_loss)] // the C# `FrameCounter / 60f`
        let time = counter as f32 / 60.0;
        let (pad, pad_index) = self
            .with_runtime(scene, |runtime, state| {
                (
                    runtime.invoke_input_process(state, counter, time),
                    runtime.script_pad_index(),
                )
            })
            .unwrap_or((0, 0));
        clock.stop(Stage::Lua, started);
        let (first, second) = routing::merge_script_pad(
            player_one,
            player_two,
            InputAction::from_bits(u32::try_from(pad).unwrap_or(0)),
            pad_index,
        );
        self.input.players = [
            i32::try_from(first.bits()).unwrap_or(0),
            i32::try_from(second.bits()).unwrap_or(0),
        ];
        routing::core_word(first, second)
    }

    /// The tile capture, the conversion and the frame hooks of the frame the core just ran (the
    /// second half of a frame).
    fn finish_frame_lent(&mut self, scene: &mut SceneState, clock: &mut StageClock) {
        let captured = self.capture_batch(clock);
        self.convert_batch_lent(scene, clock, captured);
    }

    /// The layers of the frame the core just ran.
    fn capture_batch(&mut self, clock: &mut StageClock) -> CapturedBatch {
        let started = clock.start();
        let captured = self.core.engine_input().cloned().unwrap_or_else(EngineInput::empty);
        clock.stop(Stage::Capture, started);
        captured
    }

    /// Pauses the 3D (`paused`, `emu.pause_3d_in_2d` while the 2D picture is shown) or brings it
    /// back: while paused, the frames of a profile whose scripts leave the core alone are
    /// neither converted nor shown to the frame hooks, since nothing of the 3D scene is drawn;
    /// the last of them is converted when the 3D comes back, before it is drawn. The scripts'
    /// input hook still runs: it steers the game.
    pub fn pause_3d(
        &mut self,
        paused: bool,
        model: &mut SettingsModel,
        scene: &mut SceneState,
        clock: &mut StageClock,
    ) {
        self.paused_3d = paused;
        if paused {
            return;
        }
        if let Some(captured) = self.skipped_batch.take() {
            // The kept frame already counted when it was skipped; its conversion counts it.
            self.frame_counter -= 1;
            self.lend(model, scene, |session, scene| {
                session.convert_batch_lent(scene, clock, captured);
            });
        }
    }

    /// Whether the frames finished from now on are ones a catch-up runs before the frame it shows
    /// (`emu.catch_up_last_3d`): those of a profile whose scripts leave the core alone are
    /// neither converted nor shown to the frame hooks, as with [`GameSession::pause_3d`].
    pub const fn set_catching_up(&mut self, catching_up: bool) {
        self.catching_up = catching_up;
    }

    /// The conversion, frame hooks and camera tracking of the captured frame.
    fn convert_batch_lent(
        &mut self,
        scene: &mut SceneState,
        clock: &mut StageClock,
        captured: CapturedBatch,
    ) {
        if (self.paused_3d || self.catching_up) && self.pipeline.ahead_allowed {
            // Kept for when the 3D comes back; the scripts' clock goes on.
            self.skipped_batch = Some(captured);
            self.frame_counter += 1;
            return;
        }
        self.skipped_batch = None;
        let counter = self.frame_counter;
        // The layers line up for the camera's resting distance (it follows the field of view).
        self.layer_distance = scene.camera.reset_distance();
        // The conversion is synchronous, right after the frame, as REQ-LOOP-060 (c) requires.
        // Layer 1 (with the sprites) goes to the engine that runs the profile's scripts; an
        // empty layer when the frame has none, so its frames still follow the game's.
        let first = captured.layers.iter().position(|layer| layer.layer == 0);
        let empty = zax_snes::LayerInput {
            layer: 0,
            records: Vec::new(),
        };
        let batch = captured.batch(first.map_or(&empty, |index| &captured.layers[index]));
        let started = clock.start();
        self.engine.settings_mut().flat_layer = false;
        self.engine.settings_mut().clipping = layers::SNES_CLIPPING;
        self.scripted(scene, |engine| {
            engine.convert(&batch, counter);
        });
        if let Err(error) = self.layers.convert(&captured, counter, &[0]) {
            crate::app_log::error(&format!("XSnes3D+: a layer engine failed: {error}"));
        }
        if let Some(started) = started {
            // The frame hooks are timed inside the conversion and counted apart.
            let hooks = self.engine.last_hook_time().unwrap_or_default();
            clock.add(Stage::Conversion, started.elapsed().saturating_sub(hooks));
            clock.add(Stage::Lua, hooks);
        }
        // The subjective camera follows the shape a script picked, through the tracker of
        // SPEC-04 (REQ-LUA-088, REQ-SCN-021).
        let started = clock.start();
        self.with_runtime(scene, ProfileScriptRuntime::advance_camera_tracking);
        clock.stop(Stage::Lua, started);
        self.engine.publish();
        self.converted = Some(captured);
        self.frame_counter += 1;
    }

    /// The Zapper aim offset the scripts set (kept for the script API; the SNES has none).
    #[must_use]
    pub fn zapper_offset(&self) -> Vec3 {
        self.input.zapper_offset
    }

    /// The list of tiles to draw for the published frame: every layer's, put together.
    pub fn build_draw_list(&mut self) -> &FrameDrawList {
        let own = self.engine.build_draw_list().clone();
        self.merged = match &self.converted {
            Some(input) => self.layers.draw_list(input, self.layer_distance, &[(0, &own)]),
            None => own,
        };
        &self.merged
    }

    /// The 3D objects that have been undrawable for the whole window of AMD-051 REQ-E3D-646,
    /// each once. Empty on every frame of a healthy profile.
    pub fn take_late_objects(&mut self) -> Vec<zax_engine3d::instance::InstanceRef> {
        self.engine.take_late_objects()
    }

    /// Whether the game is played flat.
    #[must_use]
    pub fn two_dimensional_mode(&self) -> bool {
        self.engine.two_dimensional_mode
    }

    /// Copies the core's indexed picture (256 x 240, larger with widescreen margins) into
    /// `destination`; returns its rows.
    pub fn copy_nes_picture(&self, destination: &mut Vec<u8>) -> usize {
        destination.clear();
        if self.pipeline.in_flight || self.pipeline.pending.is_some() {
            destination.extend_from_slice(&self.pipeline.picture);
            self.pipeline.picture_height
        } else {
            self.core.wide_picture_into(destination)
        }
    }

    /// [`GameSession::copy_nes_picture`] with the sprites the experimental smooth scrolling draws
    /// apart moved to `overlay` ([`SnesCore::split_steady_sprites`]; empty without them).
    pub fn copy_nes_picture_layers(
        &self,
        destination: &mut Vec<u8>,
        overlay: &mut Vec<u8>,
    ) -> usize {
        if self.pipeline.in_flight || self.pipeline.pending.is_some() {
            overlay.clear();
            overlay.extend_from_slice(&self.pipeline.overlay);
            return self.copy_nes_picture(destination);
        }
        let height = self.copy_nes_picture(destination);
        self.core.split_steady_sprites(destination, overlay);
        height
    }

    /// The experimental smooth scrolling's shift of the picture [`GameSession::copy_nes_picture`]
    /// copies ([`SnesCore::picture_shift`]).
    #[must_use]
    pub fn nes_picture_shift(&self) -> Option<(f32, i32, i32)> {
        if self.pipeline.in_flight || self.pipeline.pending.is_some() {
            self.pipeline.picture_shift
        } else {
            self.core.picture_shift()
        }
    }

    /// The camera motion to the next frame of the picture [`GameSession::copy_nes_picture`]
    /// copies ([`SnesCore::picture_motion`]).
    #[must_use]
    pub fn nes_picture_motion(&self) -> Option<f32> {
        if self.pipeline.in_flight || self.pipeline.pending.is_some() {
            self.pipeline.picture_motion
        } else {
            self.core.picture_motion()
        }
    }

    /// The camera the core's last picture shows ([`SnesCore::picture_camera`]), for a debug log.
    #[must_use]
    pub fn nes_picture_camera(&self) -> Option<f64> {
        self.core.picture_camera()
    }

    /// The shape the subjective camera follows.
    #[must_use]
    pub fn followed_shape(&self) -> Option<zax_engine3d::frame::ShapeId> {
        self.runtime().and_then(ProfileScriptRuntime::fpv_shape)
    }

    /// The colour of the first palette entry of the newest frame, behind the NES picture.
    #[must_use]
    pub fn nes_background_color(&self) -> Rgb24 {
        // The sky of the core's last picture: games often paint it with colour maths, so
        // the finished picture, not palette entry 0, is where it is.
        self.core.background_color()
    }

    /// Emulated time of the newest frame, in seconds.
    #[must_use]
    pub fn emulated_time(&self) -> f32 {
        let state = self.engine.state();
        state
            .published_frame()
            .unwrap_or_else(|| state.converting_frame())
            .time()
    }

    /// Switches the meshing algorithm; the geometry is rebuilt (REQ-LOOP-024).
    pub fn set_render_algorithm(
        &mut self,
        model: &mut SettingsModel,
        scene: &mut SceneState,
        algorithm: RenderAlgorithm,
    ) {
        self.lend(model, scene, |session, scene| {
            session.scripted(scene, |engine| engine.set_render_algorithm(algorithm));
        });
    }

    /// Mirrors the settings changed since the last call into the engine, and reports what the
    /// scripts and the model changed.
    pub fn sync_settings(
        &mut self,
        model: &mut SettingsModel,
        scene: &mut SceneState,
    ) -> Vec<SessionEvent> {
        self.lend(model, scene, |_, _| ());
        self.events.try_iter().collect()
    }

    /// The events the scripts raised since the last call.
    pub fn take_events(&mut self) -> Vec<SessionEvent> {
        self.events.try_iter().collect()
    }

    /// Ends the game and drops the session.
    pub fn dispose(mut self, model: &mut SettingsModel, scene: &mut SceneState) {
        self.end(model, scene);
    }
}

// The pipelined frames (`emu.pipeline`) lend the session, the settings and the scene to the
// conversion thread for the length of a frame.
const _: fn() = || {
    fn send<T: Send>() {}
    send::<GameSession>();
    send::<SettingsModel>();
    send::<SceneState>();
};

#[cfg(test)]
mod tests {
    use zax_profiles::script::ProfileScript;

    use super::*;

    fn profile(sources: &[&str]) -> GameProfile {
        GameProfile {
            scripts: sources
                .iter()
                .map(|source| ProfileScript {
                    source: Some((*source).to_owned()),
                    ..ProfileScript::default()
                })
                .collect(),
            ..GameProfile::default()
        }
    }

    #[test]
    fn only_scripts_that_leave_the_core_alone_run_ahead() {
        assert!(scripts_leave_the_core_alone(&GameProfile::default()));
        assert!(scripts_leave_the_core_alone(&profile(&[
            "function UpdateS() shape.Offset.z = 2 end"
        ])));
        // `ReadMem` reads the copy of the RAM taken when the frame came back.
        assert!(scripts_leave_the_core_alone(&profile(&[
            "function Update() local lane = Script:ReadMem(0x70) end"
        ])));
        for member in CORE_MEMBERS {
            let source = format!("function Update() local v = Script.{member} end");
            assert!(
                !scripts_leave_the_core_alone(&profile(&["function Start() end", &source])),
                "{member}"
            );
        }
    }

    /// Q-103: of the six members of the "emulation ahead" rule, the two that **change** the core
    /// force the run-ahead to 0; the three read-only ones and the input hook do not.
    #[test]
    fn only_the_members_that_change_the_core_force_the_run_ahead_to_zero() {
        assert!(!scripts_change_the_core(&GameProfile::default()));
        assert!(!scripts_change_the_core(&profile(&[
            "function Update() local lane = Script:ReadMem(0x70) end"
        ])));
        for member in CORE_MEMBERS {
            let source = format!("function Update() local v = Script.{member} end");
            let changes = scripts_change_the_core(&profile(&["function Start() end", &source]));
            assert_eq!(CORE_CHANGING_MEMBERS.contains(&member), changes, "{member}");
        }
        // One script of the profile is enough, and `WriteMem` from a frame hook is the case the
        // ticket is about.
        assert!(scripts_change_the_core(&profile(&[
            "function UpdateS() shape.Offset.z = 2 end",
            "function Update() Script:WriteMem(0x75A, 5) end",
        ])));
    }
}

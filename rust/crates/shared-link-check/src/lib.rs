//! Nothing uses this crate yet. It only makes `cargo check` build the shared 3D engine from the
//! Xnes3D+ submodule, so a broken path or a workspace setting that does not carry over shows up
//! in step 1 rather than when step 5 starts using the engine.

pub use zax_engine3d;

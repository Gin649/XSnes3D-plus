//! Builds Mesen 2's emulation core (C and C++) and the `XSnes3D+` shim into a static library.
//!
//! The submodule in `external/mesen2` is never changed. The parts we need are copied into the
//! build folder, our patches in `patches/` are applied to that copy, and the copy is compiled.
//! Mesen's user interface, SDL and platform folders are left out.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The Mesen folders the core needs.
const MESEN_FOLDERS: [&str; 4] = ["Core", "Utilities", "SevenZip", "Lua"];

/// The files of the Lua language itself in Mesen's `Lua` folder. They are not compiled: Lua
/// comes from `mlua-sys` (the same Lua 5.4 the app's scripts use), because two copies of Lua in
/// one program clash when it is linked. Mesen's other files there (luasocket) are compiled.
const LUA_FILES: [&str; 32] = [
    "lapi.c",
    "lauxlib.c",
    "lbaselib.c",
    "lcode.c",
    "lcorolib.c",
    "lctype.c",
    "ldblib.c",
    "ldebug.c",
    "ldo.c",
    "ldump.c",
    "lfunc.c",
    "lgc.c",
    "linit.c",
    "liolib.c",
    "llex.c",
    "lmathlib.c",
    "lmem.c",
    "loadlib.c",
    "lobject.c",
    "lopcodes.c",
    "loslib.c",
    "lparser.c",
    "lstate.c",
    "lstring.c",
    "lstrlib.c",
    "ltable.c",
    "ltablib.c",
    "ltm.c",
    "lundump.c",
    "lutf8lib.c",
    "lvm.c",
    "lzio.c",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let crate_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let mesen = crate_dir.join("../../../external/mesen2");
    if !mesen.join("Core").is_dir() {
        return Err(format!(
            "Mesen 2 not found at {}. Run: git submodule update --init external/mesen2",
            mesen.display()
        )
        .into());
    }

    let out = PathBuf::from(env::var("OUT_DIR")?);
    let copy = out.join("mesen2");
    if copy.exists() {
        fs::remove_dir_all(&copy)?;
    }
    for folder in MESEN_FOLDERS {
        copy_tree(&mesen.join(folder), &copy.join(folder))?;
    }

    let mut patches: Vec<PathBuf> = fs::read_dir(crate_dir.join("patches"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "patch"))
        .collect();
    patches.sort();
    for patch in &patches {
        // The copy is inside this repository's target folder. Inside a repository, git apply
        // quietly skips files outside the current folder, so git is stopped from finding the
        // repository: it then patches the copy as plain files.
        let status = Command::new("git")
            .arg("apply")
            .arg(patch)
            .current_dir(&copy)
            .env("GIT_CEILING_DIRECTORIES", &out)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .status()?;
        if !status.success() {
            return Err(format!("could not apply {}", patch.display()).into());
        }
        println!("cargo:rerun-if-changed={}", patch.display());
    }
    // Make sure the host frame-step patch really landed.
    let emulator = fs::read_to_string(copy.join("Core/Shared/Emulator.h"))?;
    if !emulator.contains("HostRunFrame") {
        return Err("the Mesen 2 patch was not applied (no HostRunFrame in Emulator.h)".into());
    }

    let mut c_files = Vec::new();
    let mut cpp_files = Vec::new();
    for folder in MESEN_FOLDERS {
        collect_sources(&copy.join(folder), &mut c_files, &mut cpp_files)?;
    }
    cpp_files.push(crate_dir.join("cpp/shim.cpp"));

    // Mesen is always built optimised, even in debug builds: an unoptimised core is too slow
    // for the tests. Warnings from Mesen's own code are hidden.
    let mut c = cc::Build::new();
    c.files(&c_files)
        .include(&copy)
        .include(copy.join("Core"))
        .include(copy.join("Utilities"))
        .opt_level(2)
        .warnings(false)
        .flag_if_supported("-w");
    c.compile("mesen2_c");

    let mut cpp = cc::Build::new();
    cpp.cpp(true)
        .files(&cpp_files)
        .include(&copy)
        .include(copy.join("Core"))
        .include(copy.join("Utilities"))
        .std("c++17")
        .opt_level(2)
        .warnings(false)
        .flag_if_supported("-w");
    cpp.compile("mesen2_core");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-lib=pthread");
    }
    println!("cargo:rerun-if-changed=cpp/shim.cpp");
    println!("cargo:rerun-if-changed={}", mesen.join("Core").display());
    Ok(())
}

/// Copies a folder and everything in it.
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Finds the .c and .cpp files under a folder, in a fixed order.
fn collect_sources(folder: &Path, c: &mut Vec<PathBuf>, cpp: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut entries: Vec<PathBuf> = fs::read_dir(folder)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_sources(&path, c, cpp)?;
        } else {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            let in_lua = path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|parent| parent == "Lua");
            if in_lua && LUA_FILES.contains(&name) {
                continue;
            }
            match path.extension().and_then(|ext| ext.to_str()) {
                Some("c") => c.push(path),
                Some("cpp") => cpp.push(path),
                _ => {}
            }
        }
    }
    Ok(())
}

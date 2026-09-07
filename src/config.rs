/* config.rs - application identity and file locations. */

use std::path::PathBuf;

pub const APP_ID: &str = "org.sysmo.SysMo";
pub const APP_NAME: &str = "SysMo";
#[allow(dead_code)]
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ENGINE_BINARY: &str = "sysmo-magpie";

/// Directory holding SysMo's shared data (the engine's hardware database).
///
/// Resolution order: `$SYSMO_DATA_DIR`, the `SYSMO_PKGDATADIR` baked in by the
/// meson build, then `<prefix>/share/sysmo` relative to the running binary.
pub fn pkgdatadir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("SYSMO_DATA_DIR") {
        return Some(PathBuf::from(dir));
    }
    if let Some(dir) = option_env!("SYSMO_PKGDATADIR") {
        let path = PathBuf::from(dir);
        if path.exists() {
            return Some(path);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let prefix = exe.parent()?.parent()?;
    let path = prefix.join("share").join("sysmo");
    path.exists().then_some(path)
}

/// Hardware database consumed by the engine (PCI/USB vendor and model names).
pub fn hwdb_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("MC_MAGPIE_HW_DB") {
        return Some(PathBuf::from(path));
    }
    let path = pkgdatadir()?.join("hw.db");
    path.exists().then_some(path)
}

/// Locate the engine executable.
///
/// Order: `$SYSMO_ENGINE`, a `sysmo-magpie` next to this binary (installed
/// layout), the cargo development layout, and finally a `$PATH` lookup.
pub fn engine_path() -> PathBuf {
    if let Some(path) = std::env::var_os("SYSMO_ENGINE") {
        return PathBuf::from(path);
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join(ENGINE_BINARY);
            if sibling.exists() {
                return sibling;
            }

            // <root>/target/<profile>/sysmo  ->  <root>/subprojects/magpie/target/<profile>/magpie
            if let (Some(profile), Some(root)) =
                (dir.file_name(), dir.parent().and_then(|t| t.parent()))
            {
                let dev = root
                    .join("subprojects")
                    .join("magpie")
                    .join("target")
                    .join(profile)
                    .join("magpie");
                if dev.exists() {
                    return dev;
                }
            }
        }
    }

    PathBuf::from(ENGINE_BINARY)
}

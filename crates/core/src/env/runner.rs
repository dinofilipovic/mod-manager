//! Runs Windows programs inside an environment: CrossOver's `wine --bottle`
//! or plain `wine` with `WINEPREFIX`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::{Environment, EnvironmentKind};
use crate::detect::{GameInstance, Store};
use crate::error::{Error, Result};
use crate::fs_util::resolve_case_insensitive;
use crate::game::GameDefinition;

/// CrossOver's `wine` launcher, if CrossOver is installed.
pub fn crossover_wine() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(root) = std::env::var_os("CX_ROOT") {
        candidates.push(PathBuf::from(root).join("bin/wine"));
    }
    let home = dirs::home_dir().unwrap_or_default();
    if cfg!(target_os = "macos") {
        for apps in [PathBuf::from("/Applications"), home.join("Applications")] {
            candidates.push(apps.join("CrossOver.app/Contents/SharedSupport/CrossOver/bin/wine"));
        }
    } else {
        candidates.push(PathBuf::from("/opt/cxoffice/bin/wine"));
        candidates.push(home.join("cxoffice/bin/wine"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

fn plain_wine() -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join("wine"))
        .find(|p| p.is_file())
}

impl Environment {
    /// A `Command` that runs a Windows program in this environment.
    pub fn wine_command(&self) -> Result<Command> {
        match &self.kind {
            EnvironmentKind::CrossOverBottle { .. } => {
                let wine = crossover_wine().ok_or_else(|| {
                    Error::Wine("CrossOver isn't installed in a standard location.".into())
                })?;
                let mut cmd = Command::new(wine);
                cmd.arg("--bottle").arg(&self.name);
                Ok(cmd)
            }
            EnvironmentKind::WinePrefix => {
                let wine = plain_wine().ok_or_else(|| Error::Wine("wine isn't on PATH.".into()))?;
                let mut cmd = Command::new(wine);
                cmd.env("WINEPREFIX", &self.prefix);
                Ok(cmd)
            }
            EnvironmentKind::Proton { .. } => Err(Error::Wine(
                "Proton prefixes are managed by Steam; set Wine options in the game's launch options.".into(),
            )),
            EnvironmentKind::Native => Err(Error::Wine("Windows runs games directly, without Wine.".into())),
        }
    }

    /// Converts a path inside this prefix's C: drive to its Windows form.
    pub fn host_to_windows(&self, path: &Path) -> Option<String> {
        let c_drive = crate::fs_util::canonical_or_self(self.c_drive());
        let path = crate::fs_util::canonical_or_self(path.to_path_buf());
        let relative = path.strip_prefix(&c_drive).ok()?;
        let parts: Vec<String> = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        Some(format!("C:\\{}", parts.join("\\")))
    }

    /// Sets Wine DLL overrides for the whole prefix, e.g. `version` to
    /// `native,builtin`. Runs `reg add` inside the prefix so Wine applies it
    /// even while the bottle is running.
    pub fn set_dll_overrides(&self, overrides: &[(String, String)]) -> Result<()> {
        if !self.is_wine() {
            return Ok(());
        }
        for (dll, mode) in overrides {
            let output = self
                .wine_command()?
                .args([
                    "reg",
                    "add",
                    r"HKEY_CURRENT_USER\Software\Wine\DllOverrides",
                    "/v",
                ])
                .arg(dll)
                .args(["/t", "REG_SZ", "/d"])
                .arg(mode)
                .arg("/f")
                .stdin(Stdio::null())
                .output()
                .map_err(|e| Error::Wine(format!("couldn't run wine: {e}")))?;
            if !output.status.success() {
                return Err(Error::Wine(format!(
                    "setting the {dll} override failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
        }
        Ok(())
    }

    /// Starts the game without waiting for it, through its store's client
    /// where that matters: Steam (DRM, overlay) and Epic (ownership check).
    /// GOG games are DRM-free and start directly, which keeps the modded
    /// arguments Galaxy wouldn't pass on.
    pub fn launch_game(
        &self,
        game: &GameDefinition,
        instance: &GameInstance,
        modded: bool,
    ) -> Result<()> {
        let args: Vec<String> = if modded {
            game.launch.modded_args.clone()
        } else {
            Vec::new()
        };
        let steam_id = game
            .stores
            .steam_app_id
            .as_deref()
            .filter(|_| instance.store == Store::Steam);
        let epic_url = instance
            .launch_id
            .as_deref()
            .filter(|_| instance.store == Store::Epic)
            .map(|app| format!("com.epicgames.launcher://apps/{app}?action=launch&silent=true"));

        let mut cmd = match &self.kind {
            EnvironmentKind::Proton { app_id, .. } => {
                // Steam starts the game with the launch options set in Steam.
                return spawn(steam_url_command(&format!("steam://rungameid/{app_id}")));
            }
            EnvironmentKind::Native => match (steam_id, &epic_url) {
                (Some(id), _) => {
                    return spawn(steam_url_command(&format!(
                        "steam://run/{id}//{}/",
                        args.join(" ")
                    )));
                }
                (None, Some(url)) => return spawn(steam_url_command(url)),
                _ => {
                    let exe = self.game_exe(game, instance)?;
                    let mut cmd = Command::new(&exe);
                    if let Some(dir) = exe.parent() {
                        cmd.current_dir(dir);
                    }
                    cmd.args(&args);
                    cmd
                }
            },
            _ => {
                let mut cmd = self.wine_command()?;
                let steam = self.windows_to_host(r"C:\Program Files (x86)\Steam\steam.exe");
                let epic = self.windows_to_host(
                    r"C:\Program Files (x86)\Epic Games\Launcher\Portal\Binaries\Win64\EpicGamesLauncher.exe",
                );
                match (steam_id, &epic_url) {
                    (Some(id), _) if steam.is_some_and(|s| s.is_file()) => {
                        cmd.arg(r"C:\Program Files (x86)\Steam\steam.exe")
                            .arg("-applaunch")
                            .arg(id)
                            .args(&args);
                    }
                    (None, Some(url)) if epic.is_some_and(|e| e.is_file()) => {
                        cmd.arg("start").arg(url);
                    }
                    _ => {
                        let exe = self.game_exe(game, instance)?;
                        let windows_exe = self.host_to_windows(&exe).ok_or_else(|| {
                            Error::Wine("the game isn't inside this bottle's C: drive".into())
                        })?;
                        if let Some(dir) = exe.parent() {
                            cmd.current_dir(dir);
                        }
                        cmd.arg(windows_exe).args(&args);
                    }
                }
                cmd
            }
        };
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        spawn(cmd)
    }

    fn game_exe(&self, game: &GameDefinition, instance: &GameInstance) -> Result<PathBuf> {
        resolve_case_insensitive(&instance.install_path, &game.executable)
            .ok_or_else(|| Error::Wine("the game executable is missing".into()))
    }
}

/// Opens a launcher URL (`steam://`, `com.epicgames.launcher://`) with the
/// host's URL handler.
fn steam_url_command(url: &str) -> Command {
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else if cfg!(target_os = "macos") {
        Command::new("open")
    } else {
        Command::new("xdg-open")
    };
    cmd.arg(url);
    cmd
}

fn spawn(mut cmd: Command) -> Result<()> {
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| Error::Wine(format!("couldn't start the game: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_host_paths_to_windows() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("drive_c/Program Files/Game");
        std::fs::create_dir_all(&dir).unwrap();
        let env = Environment {
            id: "t".into(),
            name: "t".into(),
            kind: EnvironmentKind::WinePrefix,
            prefix: tmp.path().to_path_buf(),
        };
        assert_eq!(
            env.host_to_windows(&dir).as_deref(),
            Some(r"C:\Program Files\Game")
        );
        assert!(env.host_to_windows(tmp.path()).is_none());
    }
}

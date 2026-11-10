use std::ffi::{CStr, CString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use core_foundation::url::CFURL;
use tauri::{AppHandle, Manager, Runtime, Window};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

use crate::Prompt;

const APPLICATIONS: &str = "/Applications";

/// What [`install_as_user`] does, for running as an administrator: copies the
/// bundle `$1` to `$2`, putting back the copy it replaces if the new one cannot
/// take its place.
const INSTALL_AS_ADMIN: &str = r#"set -e
src=$1; dest=$2; staging="$dest.moving"; old="$dest.replaced"
rm -rf "$staging" "$old"
ditto "$src" "$staging"
xattr -dr com.apple.quarantine "$staging" 2>/dev/null || true
if [ -e "$dest" ]; then mv "$dest" "$old"; fi
if ! mv "$staging" "$dest"; then
	if [ -e "$old" ]; then mv "$old" "$dest"; fi
	exit 1
fi
rm -rf "$old" || true
"#;

/// Waits for process `$1` to exit, ejects the disk image mounted at `$3` if
/// any, and opens the app at `$2`.
const RELAUNCH: &str = r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done
if [ -n "$3" ]; then hdiutil detach "$3" -quiet || hdiutil detach "$3" -quiet -force; fi
open "$2"
"#;

pub fn needs_move<R: Runtime>(app: &AppHandle<R>) -> bool {
    current_bundle(app).is_some_and(|bundle| is_translocated(&bundle) || is_read_only(&bundle))
}

/// Asks the user, over `window`, whether to move the app, and moves it if they
/// agree.
pub fn offer_move<R: Runtime>(window: &Window<R>, prompt: &Prompt) {
    let app = window.app_handle().clone();
    if !needs_move(&app) {
        return;
    }
    app.dialog()
        .message(&prompt.message)
        .title(&prompt.title)
        .parent(window)
        .buttons(MessageDialogButtons::OkCancelCustom(
            prompt.move_button.clone(),
            prompt.cancel_button.clone(),
        ))
        .show(move |moving| {
            if !moving {
                return;
            }
            if let Err(err) = move_to_applications_folder(&app) {
                log::error!("Could not move the app to Applications: {err}");
            }
        });
}

/// Copies the app into /Applications, asking for an administrator password if
/// that takes one, then exits and relaunches it from there. Returns only on
/// failure, or with `Ok` when the user cancels the administrator prompt.
///
/// The original stays where it is: removing it from Downloads would make macOS
/// ask the user to let the app into their Downloads folder.
pub fn move_to_applications_folder<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    if !needs_move(app) {
        return Err("the app already runs from where it can update itself".into());
    }
    let bundle = current_bundle(app).ok_or("the app is not running from a bundle")?;
    let name = bundle.file_name().ok_or("the bundle has no name")?;
    let dest = Path::new(APPLICATIONS).join(name);

    // A quarantined copy there would be translocated just the same.
    let installing =
        is_quarantined(&dest) || !has_same_or_newer_version(&dest, &app.package_info().version);
    if installing && !install(&bundle, &dest)? {
        return Ok(());
    }

    // An app opened from a disk image is translocated too.
    let location = match is_translocated(&bundle) {
        true => original_path(&bundle).unwrap_or(bundle),
        false => bundle,
    };
    let disk_image = mount_point(&location).filter(|mount| is_disk_image(mount));

    relaunch_after_exit(&dest, disk_image.as_deref())?;
    app.cleanup_before_exit();
    std::process::exit(0);
}

/// The `.app` bundle the running executable belongs to.
fn current_bundle<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
    let exe = tauri::process::current_binary(&app.env()).ok()?;
    exe.ancestors()
        .find(|path| path.extension().is_some_and(|ext| ext == "app"))
        .map(Path::to_path_buf)
}

/// macOS runs a quarantined app that was never moved from a randomized
/// read-only mount of where it was downloaded or opened from.
fn is_translocated(bundle: &Path) -> bool {
    CFURL::from_path(bundle, true)
        .is_some_and(|url| security_translocate::is_translocated_url(url).unwrap_or(false))
}

/// Where the user actually keeps the app macOS translocated.
fn original_path(bundle: &Path) -> Option<PathBuf> {
    let url = CFURL::from_path(bundle, true)?;
    security_translocate::create_original_path_for_url(url)
        .ok()?
        .to_path()
}

fn is_read_only(bundle: &Path) -> bool {
    statfs(bundle).is_some_and(|fs| fs.f_flags & libc::MNT_RDONLY as u32 != 0)
}

fn mount_point(path: &Path) -> Option<PathBuf> {
    let fs = statfs(path)?;
    let mount = unsafe { CStr::from_ptr(fs.f_mntonname.as_ptr()) };
    Some(PathBuf::from(mount.to_string_lossy().into_owned()))
}

fn statfs(path: &Path) -> Option<libc::statfs> {
    let path = c_path(path).ok()?;
    let mut fs: libc::statfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statfs(path.as_ptr(), &mut fs) } == 0).then_some(fs)
}

/// Whether `mount` is where an attached disk image is mounted, rather than
/// e.g. a network share or an external drive.
fn is_disk_image(mount: &Path) -> bool {
    let Ok(info) = Command::new("/usr/bin/hdiutil").arg("info").output() else {
        return false;
    };
    let mount = mount.to_string_lossy();
    String::from_utf8_lossy(&info.stdout)
        .lines()
        .any(|line| line.rsplit('\t').next() == Some(&mount))
}

/// Whether the app at `dest` is at least `version`, so copying over it would
/// not move the user back.
fn has_same_or_newer_version(dest: &Path, version: &semver::Version) -> bool {
    plist::from_file::<_, plist::Dictionary>(dest.join("Contents/Info.plist"))
        .ok()
        .and_then(|info| {
            let installed = info.get("CFBundleShortVersionString")?.as_string()?;
            semver::Version::parse(installed).ok()
        })
        .is_some_and(|installed| installed >= *version)
}

/// Copies the app to `dest` as the user, or as an administrator if
/// /Applications only lets administrators write to it. Returns `false` when
/// the user cancels the administrator prompt.
fn install(bundle: &Path, dest: &Path) -> Result<bool, String> {
    match install_as_user(bundle, dest) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
            log::info!("Copying to Applications needs an administrator: {err}");
            install_as_admin(bundle, dest)
        }
        Err(err) => Err(err.to_string()),
    }
}

/// Copies `bundle` next to `dest` and swaps it into place, so `dest` is never
/// missing or half copied.
fn install_as_user(bundle: &Path, dest: &Path) -> io::Result<()> {
    let staging = dest.with_extension("app.moving");
    let _ = std::fs::remove_dir_all(&staging);
    let result = copy_bundle(bundle, &staging)
        .and_then(|()| remove_quarantine(&staging))
        .and_then(|()| put_in_place(&staging, dest));
    // After a swap this is the copy that was replaced.
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// Copies the bundle as `ditto` does, keeping the symlinks, permissions and
/// attributes its code signature covers.
fn copy_bundle(from: &Path, to: &Path) -> io::Result<()> {
    let (from, to) = (c_path(from)?, c_path(to)?);
    let flags = libc::COPYFILE_DATA
        | libc::COPYFILE_METADATA
        | libc::COPYFILE_RECURSIVE
        | libc::COPYFILE_NOFOLLOW;
    match unsafe { libc::copyfile(from.as_ptr(), to.as_ptr(), std::ptr::null_mut(), flags) } {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}

fn is_quarantined(path: &Path) -> bool {
    let Ok(path) = c_path(path) else {
        return false;
    };
    let size = unsafe {
        libc::getxattr(
            path.as_ptr(),
            c"com.apple.quarantine".as_ptr(),
            std::ptr::null_mut(),
            0,
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    size >= 0
}

/// Clears the download quarantine the copy inherited, so macOS does not
/// translocate it too.
fn remove_quarantine(path: &Path) -> io::Result<()> {
    let c = c_path(path)?;
    // Most files never had the attribute, so a failure here means nothing.
    unsafe {
        libc::removexattr(
            c.as_ptr(),
            c"com.apple.quarantine".as_ptr(),
            libc::XATTR_NOFOLLOW,
        )
    };
    if std::fs::symlink_metadata(path)?.is_dir() {
        for entry in std::fs::read_dir(path)? {
            remove_quarantine(&entry?.path())?;
        }
    }
    Ok(())
}

/// Moves `staging` to `dest`, swapping the two in one step when `dest` exists.
fn put_in_place(staging: &Path, dest: &Path) -> io::Result<()> {
    if !dest.exists() {
        return std::fs::rename(staging, dest);
    }
    let (from, to) = (c_path(staging)?, c_path(dest)?);
    match unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_SWAP) } {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)
}

fn install_as_admin(bundle: &Path, dest: &Path) -> Result<bool, String> {
    let command = format!(
        "/bin/sh -c {} sh {} {}",
        shell_quote(INSTALL_AS_ADMIN),
        shell_quote(&bundle.to_string_lossy()),
        shell_quote(&dest.to_string_lossy()),
    );
    let output = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(format!(
            "do shell script \"{}\" with administrator privileges",
            applescript_escape(&command)
        ))
        .output()
        .map_err(|err| err.to_string())?;
    let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
    match output.status.success() {
        true => Ok(true),
        // AppleScript's "User canceled." error.
        false if error.ends_with("(-128)") => Ok(false),
        false => Err(error),
    }
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn applescript_escape(text: &str) -> String {
    text.replace('\\', r"\\").replace('"', "\\\"")
}

/// Opens `dest` once this process is gone, so the copy starts with no other
/// instance holding the app's databases, ports or single-instance socket.
fn relaunch_after_exit(dest: &Path, disk_image: Option<&Path>) -> Result<(), String> {
    Command::new("/bin/sh")
        .args(["-c", RELAUNCH, "sh", &std::process::id().to_string()])
        .arg(dest)
        .arg(disk_image.unwrap_or(Path::new("")))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bundle with an executable, a symlink and the quarantine a download
    /// leaves on it.
    fn downloaded_bundle(dir: &Path, marker: &str) -> PathBuf {
        let bundle = dir.join(format!("{marker}/Test.app"));
        let macos = bundle.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::write(macos.join("test"), marker).unwrap();
        std::fs::set_permissions(
            macos.join("test"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        std::os::unix::fs::symlink("MacOS/test", bundle.join("Contents/link")).unwrap();
        let value = b"0083;00000000;Safari;";
        for path in [&bundle, &macos.join("test")] {
            let path = c_path(path).unwrap();
            let set = unsafe {
                libc::setxattr(
                    path.as_ptr(),
                    c"com.apple.quarantine".as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                    0,
                )
            };
            assert_eq!(set, 0);
        }
        bundle
    }

    #[test]
    fn installs_a_copy_without_quarantine() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = downloaded_bundle(dir.path(), "new");
        let dest = dir.path().join("Applications/Test.app");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();

        install_as_user(&bundle, &dest).unwrap();

        let binary = dest.join("Contents/MacOS/test");
        assert_eq!(std::fs::read_to_string(&binary).unwrap(), "new");
        let mode =
            std::os::unix::fs::PermissionsExt::mode(&binary.metadata().unwrap().permissions());
        assert_eq!(mode & 0o777, 0o755);
        assert_eq!(
            std::fs::read_link(dest.join("Contents/link")).unwrap(),
            Path::new("MacOS/test")
        );
        assert!(!is_quarantined(&dest));
        assert!(!is_quarantined(&binary));
        assert!(!dest.with_extension("app.moving").exists());
    }

    #[test]
    fn replaces_an_installed_copy() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("Applications/Test.app");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        install_as_user(&downloaded_bundle(dir.path(), "old"), &dest).unwrap();

        install_as_user(&downloaded_bundle(dir.path(), "new"), &dest).unwrap();

        let binary = dest.join("Contents/MacOS/test");
        assert_eq!(std::fs::read_to_string(binary).unwrap(), "new");
        assert!(!dest.with_extension("app.moving").exists());
    }

    #[test]
    fn a_folder_it_cannot_write_to_asks_for_an_administrator() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = downloaded_bundle(dir.path(), "new");
        let locked = dir.path().join("Applications");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o555))
            .unwrap();

        let err = install_as_user(&bundle, &locked.join("Test.app")).unwrap_err();

        std::fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }
}

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::Result;

/// Tracks only the command link owned by this installation, including broken links.
#[derive(Deserialize, Serialize)]
pub(super) struct Launcher {
    pub path: PathBuf,
    target: PathBuf,
    existed: bool,
}

impl Launcher {
    pub fn prepare(path: &Path, root: &Path, removing: bool) -> Result<Self> {
        if cfg!(windows) {
            return Err(
                io::Error::other("standalone command symlinks are supported only on Unix").into(),
            );
        }
        let path = std::path::absolute(path)?;
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("invalid launcher path"))?;
        if !removing {
            fs::create_dir_all(parent)?;
        }
        let parent = match parent.canonicalize() {
            Ok(parent) => parent,
            Err(error) if removing && error.kind() == io::ErrorKind::NotFound => parent.to_owned(),
            Err(error) => return Err(error.into()),
        };
        let path = parent.join(
            path.file_name()
                .ok_or_else(|| io::Error::other("invalid launcher name"))?,
        );
        if path.starts_with(root) {
            return Err(io::Error::other(
                "command link must be outside the installation directory",
            )
            .into());
        }
        let mut launcher = Self {
            path,
            target: super::binary(&root.join("current")),
            existed: false,
        };
        let state = launcher.owned()?;
        if !removing && state == Some(false) {
            return Err(launcher.conflict());
        }
        launcher.existed = state == Some(true);
        Ok(launcher)
    }

    fn owned(&self) -> Result<Option<bool>> {
        match fs::symlink_metadata(&self.path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                Ok(Some(fs::read_link(&self.path)? == self.target))
            }
            Ok(_) => Ok(Some(false)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn conflict(&self) -> Box<dyn std::error::Error> {
        io::Error::other(format!("command entry already belongs to another installation: {}; move it yourself before retrying", self.path.display())).into()
    }

    pub fn install(&self) -> Result<()> {
        match self.owned()? {
            Some(true) => Ok(()),
            Some(false) => Err(self.conflict()),
            None => {
                // Creating the link directly fails if a concurrent installer won the name.
                // It never overwrites a file or follows an existing directory destination.
                #[cfg(unix)]
                {
                    std::os::unix::fs::symlink(&self.target, &self.path)?;
                    Ok(())
                }
                #[cfg(windows)]
                {
                    Err(
                        io::Error::other("standalone command symlinks are supported only on Unix")
                            .into(),
                    )
                }
            }
        }
    }

    pub fn remove(&self) -> Result<()> {
        match self.owned()? {
            Some(true) => fs::remove_file(&self.path)?,
            Some(false) => eprintln!(
                "Preserved a command entry no longer owned by this installation: {}",
                self.path.display()
            ),
            None => {}
        }
        Ok(())
    }

    pub fn restore(&self) -> Result<()> {
        if self.existed {
            if self.owned()? != Some(false) {
                self.install()?;
            }
        } else {
            self.remove()?;
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn links_are_reused_and_conflicting_entries_are_preserved() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("program");
        let path = temp.path().join("command/zg");
        let launcher = Launcher::prepare(&path, &root, false).expect("prepare");
        launcher
            .install()
            .expect("create broken link before activation");
        launcher.install().expect("idempotent link");
        assert_eq!(
            fs::read_link(&path).expect("target"),
            super::super::binary(&root.join("current"))
        );
        launcher.restore().expect("rollback a new link");
        assert!(fs::symlink_metadata(&path).is_err());
        fs::write(&path, "another installation").expect("foreign entry");
        assert!(Launcher::prepare(&path, &root, false).is_err());
        let removing = Launcher::prepare(&path, &root, true).expect("uninstall");
        removing.remove().expect("preserve foreign file");
        assert_eq!(
            fs::read_to_string(&path).expect("foreign file"),
            "another installation"
        );
        fs::remove_file(&path).expect("remove fixture");
        std::os::unix::fs::symlink(temp.path().join("foreign-missing"), &path)
            .expect("foreign broken link");
        assert!(Launcher::prepare(&path, &root, false).is_err());
        removing.remove().expect("preserve foreign link");
        assert!(
            fs::symlink_metadata(&path)
                .expect("foreign link")
                .file_type()
                .is_symlink()
        );
    }
}

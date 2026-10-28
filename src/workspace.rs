use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Clone, Debug)]
pub struct Workspace {
    root: PathBuf,
    markdown_files: Vec<PathBuf>,
    assets_dir: PathBuf,
}

#[derive(Deserialize)]
struct WorkspaceConfig {
    assets_dir: Option<PathBuf>,
}

impl Workspace {
    /// Find the nearest ancestor containing `.jian`, without creating anything.
    pub fn discover(start: &Path) -> Result<Option<Self>> {
        for directory in start.ancestors() {
            if directory.join(".jian").is_dir() {
                return Self::from_root(directory.to_path_buf()).map(Some);
            }
        }
        Ok(None)
    }

    /// Open a folder as a workspace, creating only the `.jian` marker if needed.
    pub fn open_folder(path: &Path) -> Result<Self> {
        if let Some(workspace) = Self::discover(path)? {
            return Ok(workspace);
        }
        let root = path.to_path_buf();
        fs::create_dir_all(root.join(".jian"))
            .with_context(|| format!("failed to create workspace marker in {}", root.display()))?;
        Self::from_root(root)
    }

    fn from_root(root: PathBuf) -> Result<Self> {
        let config_path = root.join(".jian/config.toml");
        let assets_dir = if config_path.exists() {
            let source = fs::read_to_string(&config_path)
                .with_context(|| format!("failed to read {}", config_path.display()))?;
            let config: WorkspaceConfig = toml::from_str(&source)
                .with_context(|| format!("failed to parse {}", config_path.display()))?;
            let directory = config.assets_dir.unwrap_or_else(|| PathBuf::from("assets"));
            if directory.is_absolute()
                || directory
                    .components()
                    .any(|component| !matches!(component, std::path::Component::Normal(_)))
            {
                bail!("assets_dir must be a safe relative path");
            }
            directory
        } else {
            PathBuf::from("assets")
        };
        let mut workspace = Self {
            root,
            markdown_files: Vec::new(),
            assets_dir,
        };
        workspace.refresh_files()?;
        Ok(workspace)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn name(&self) -> String {
        self.root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Workspace")
            .to_owned()
    }

    pub fn markdown_files(&self) -> &[PathBuf] {
        &self.markdown_files
    }

    pub fn refresh_files(&mut self) -> Result<()> {
        self.markdown_files.clear();
        collect_markdown(
            &self.root,
            &self.root,
            &self.assets_dir,
            &mut self.markdown_files,
        )?;
        self.markdown_files.sort();
        Ok(())
    }

    pub fn assets_dir(&self) -> PathBuf {
        self.root.join(&self.assets_dir)
    }
}

fn collect_markdown(
    root: &Path,
    directory: &Path,
    assets_dir: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<()> {
    if directory != root
        && directory
            .strip_prefix(root)
            .is_ok_and(|path| path == assets_dir)
    {
        return Ok(());
    }
    for entry in fs::read_dir(directory)
        .with_context(|| format!("failed to read directory {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            collect_markdown(root, &path, assets_dir, files)?;
        } else if file_type.is_file()
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
        {
            files.push(path.strip_prefix(root).unwrap_or(&path).to_path_buf());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_nearest_marker_without_creating_one() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let nested = root.join("notes/deep");
        fs::create_dir_all(&nested).unwrap();
        assert!(Workspace::discover(&nested).unwrap().is_none());

        fs::create_dir(root.join(".jian")).unwrap();
        let workspace = Workspace::discover(&nested).unwrap().unwrap();
        assert_eq!(workspace.root(), root);
    }

    #[test]
    fn reads_optional_assets_directory() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join(".jian")).unwrap();
        fs::write(
            temp.path().join(".jian/config.toml"),
            "assets_dir = \"Attachments\"\n",
        )
        .unwrap();
        fs::create_dir(temp.path().join("Attachments")).unwrap();
        fs::write(temp.path().join("Attachments/ignored.md"), "ignored").unwrap();
        let workspace = Workspace::discover(temp.path()).unwrap().unwrap();
        assert_eq!(workspace.assets_dir(), temp.path().join("Attachments"));
        assert!(workspace.markdown_files().is_empty());
    }

    #[test]
    fn opening_folder_creates_only_jian_and_lists_markdown() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("a.md"), "a").unwrap();
        fs::write(temp.path().join("b.txt"), "b").unwrap();
        fs::create_dir(temp.path().join("assets")).unwrap();
        fs::write(temp.path().join("assets/ignored.md"), "ignored").unwrap();

        let workspace = Workspace::open_folder(temp.path()).unwrap();
        assert!(temp.path().join(".jian").is_dir());
        assert!(!temp.path().join(".jian/config.toml").exists());
        assert_eq!(workspace.markdown_files(), &[PathBuf::from("a.md")]);
    }
}

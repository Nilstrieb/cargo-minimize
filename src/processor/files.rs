use anyhow::Result;
use std::{fs, path::Path};

pub(crate) use self::file::SourceFile;

mod file {
    use anyhow::{Context, Result};
    use std::{
        cell::RefCell,
        path::{Path, PathBuf},
    };

    use super::{Changes, FileChange};

    /// The representation of a source file, with the cached AST.
    /// IMPORTANT INVARIANT: All file system operations MUST go through this type.
    /// This also shouldn't be `Clone`, so the cache is always representative of the file system state.
    /// It is inteded for the "cache" to be the source of truth.
    pub(crate) struct SourceFile {
        path: PathBuf,
        content_str: RefCell<String>,
        content: RefCell<syn::File>,
    }

    impl SourceFile {
        pub(crate) fn open(path: PathBuf) -> Result<Self> {
            let string = std::fs::read_to_string(&path)
                .with_context(|| format!("reading file {}", path.display()))?;
            let content = syn::parse_file(&string)
                .with_context(|| format!("parsing file {}", path.display()))?;
            Ok(SourceFile {
                path,
                content_str: RefCell::new(string),
                content: RefCell::new(content),
            })
        }

        pub(crate) fn write(&self, new: syn::File) -> Result<()> {
            let string = crate::formatting::format(new.clone())?;
            std::fs::write(&self.path, &string)
                .with_context(|| format!("writing file {}", self.path.display()))?;
            *self.content_str.borrow_mut() = string;
            *self.content.borrow_mut() = new;
            Ok(())
        }

        pub(crate) fn path_no_fs_interact(&self) -> &Path {
            &self.path
        }
    }

    impl PartialEq for SourceFile {
        fn eq(&self, other: &Self) -> bool {
            self.path == other.path
        }
    }

    impl std::hash::Hash for SourceFile {
        fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
            self.path.hash(state);
        }
    }

    impl Eq for SourceFile {}

    impl std::fmt::Debug for SourceFile {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.path.display())
        }
    }

    impl SourceFile {
        pub(crate) fn try_change<'file, 'change>(
            &'file self,
            changes: &'change mut Changes,
        ) -> Result<FileChange<'file, 'change>> {
            let path = &self.path;
            Ok(FileChange {
                path,
                source_file: self,
                changes,
                has_written_change: false,
                before_content_str: self.content_str.borrow().clone(),
                before_content: self.content.borrow().clone(),
            })
        }
    }
}

#[derive(Default)]
pub(crate) struct Changes {
    any_change: bool,
}

pub(crate) struct FileChange<'a, 'b> {
    pub(crate) path: &'a Path,
    source_file: &'a SourceFile,
    before_content_str: String,
    before_content: syn::File,
    changes: &'b mut Changes,
    has_written_change: bool,
}

impl FileChange<'_, '_> {
    pub(crate) fn before_content(&self) -> (&str, &syn::File) {
        (&self.before_content_str, &self.before_content)
    }

    pub(crate) fn write(&mut self, new: syn::File) -> Result<()> {
        self.has_written_change = true;
        self.source_file.write(new)?;
        Ok(())
    }

    pub(crate) fn rollback(mut self) -> Result<()> {
        assert!(self.has_written_change);
        self.has_written_change = false;
        self.source_file.write(self.before_content.clone())?;
        Ok(())
    }

    pub(crate) fn commit(mut self) {
        assert!(self.has_written_change);
        self.has_written_change = false;
        self.changes.any_change = true;
    }
}

impl Drop for FileChange<'_, '_> {
    fn drop(&mut self) {
        if self.has_written_change {
            fs::write(self.path, self.before_content().0).ok();
            if !std::thread::panicking() {
                panic!("File contains unsaved changes!");
            }
        }
    }
}

impl Changes {
    pub(crate) fn had_changes(&self) -> bool {
        self.any_change
    }
}

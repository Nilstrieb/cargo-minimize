mod files;
mod reaper;

use std::{borrow::Borrow, collections::HashSet, ffi::OsStr, mem, path::Path};

use anyhow::{ensure, Context, Result};

use crate::{build::Build, processor::files::Changes};

pub use self::files::SourceFile;

pub trait Processor {
    fn refresh_state(&mut self) -> Result<()> {
        Ok(())
    }

    /// Process a file. The state of the processor might get invalidated in the process as signaled with
    /// `ProcessState::FileInvalidated`. When a file is invalidated, the minimizer will call `Processor::refersh_state`
    /// before calling the this function on the same file again.
    fn process_file(
        &mut self,
        krate: &mut syn::File,
        file: &SourceFile,
        checker: &mut PassController,
    ) -> ProcessState;

    fn name(&self) -> &'static str;
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProcessState {
    NoChange,
    Changed,
    FileInvalidated,
}

#[derive(Debug)]
pub struct Minimizer {
    files: Vec<SourceFile>,
    build: Build,
}

impl Minimizer {
    pub fn new_glob_dir(path: &Path, build: Build) -> Self {
        let walk = walkdir::WalkDir::new(path);

        let files = walk
            .into_iter()
            .filter_map(|entry| match entry {
                Ok(entry) => Some(entry),
                Err(err) => {
                    eprintln!("WARN: Error in walkdir: {err}");
                    None
                }
            })
            .filter(|entry| entry.path().extension() == Some(OsStr::new("rs")))
            .map(|entry| SourceFile {
                path: entry.into_path(),
            })
            .inspect(|file| {
                println!("- {}", file.path.display());
            })
            .collect();

        Self { files, build }
    }

    pub fn run_passes<'a>(
        &self,
        passes: impl IntoIterator<Item = Box<dyn Processor + 'a>>,
    ) -> Result<()> {
        let inital_build = self.build.build()?;
        println!("Initial build: {}", inital_build);
        ensure!(
            inital_build.reproduces_issue(),
            "Initial build must reproduce issue"
        );

        for mut pass in passes {
            self.run_pass(&mut *pass)?;
        }

        Ok(())
    }

    fn run_pass(&self, pass: &mut dyn Processor) -> Result<()> {
        let mut invalidated_files = HashSet::new();

        let mut refresh_and_try_again = false;

        loop {
            println!("Starting a round of {}", pass.name());
            let mut changes = Changes::default();

            for file in &self.files {
                if invalidated_files.contains(file) {
                    continue;
                }

                self.process_file(pass, file, &mut invalidated_files, &mut changes)?;
            }

            if !changes.had_changes() {
                if !refresh_and_try_again && invalidated_files.len() > 0 {
                    // A few files have been invalidated, let's refresh and try these again.
                    pass.refresh_state().context("refreshing state for pass")?;
                    invalidated_files.clear();
                    refresh_and_try_again = true;
                    println!("Refreshing files for {}", pass.name());
                    continue;
                }

                println!("Finished {}", pass.name());
                return Ok(());
            } else {
                refresh_and_try_again = false;
            }
        }
    }

    fn process_file<'file>(
        &self,
        pass: &mut dyn Processor,
        file: &'file SourceFile,
        invalidated_files: &mut HashSet<&'file SourceFile>,
        changes: &mut Changes,
    ) -> Result<()> {
        let mut checker = PassController::new();

        loop {
            dbg!(&checker);

            let file_display = file.path.display();

            let mut change = file.try_change(changes)?;

            let mut krate = syn::parse_file(change.before_content())
                .with_context(|| format!("parsing file {file_display}"))?;

            let has_made_change = pass.process_file(&mut krate, file, &mut checker);

            match has_made_change {
                ProcessState::Changed | ProcessState::FileInvalidated => {
                    let result = prettyplease::unparse(&krate);

                    change.write(&result)?;

                    let after = self.build.build()?;

                    println!("{file_display}: After {}: {after}", pass.name());

                    if after.reproduces_issue() {
                        change.commit();
                        checker.reproduces();
                    } else {
                        change.rollback()?;
                        checker.does_not_reproduce();
                    }

                    if has_made_change == ProcessState::FileInvalidated {
                        invalidated_files.insert(file);
                    }
                }
                ProcessState::NoChange => {
                    println!("{file_display}: After {}: no change", pass.name());
                    checker.no_change();
                }
            }

            if checker.is_finished() {
                break;
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AstPath(Vec<String>);

impl Borrow<[String]> for AstPath {
    fn borrow(&self) -> &[String] {
        &self.0
    }
}

#[derive(Debug)]
pub struct PassController {
    state: PassControllerState,
}

#[derive(Debug)]
enum PassControllerState {
    InitialCollection {
        candidates: Vec<AstPath>,
    },

    Bisecting {
        current: HashSet<AstPath>,
        worklist: Vec<Vec<AstPath>>,
    },

    Success,
}

impl PassController {
    fn new() -> Self {
        Self {
            state: PassControllerState::InitialCollection {
                candidates: Vec::new(),
            },
        }
    }

    fn reproduces(&mut self) {
        match &mut self.state {
            PassControllerState::InitialCollection { .. } => {
                self.state = PassControllerState::Success
            }
            PassControllerState::Bisecting {
                current, worklist, ..
            } => match worklist.pop() {
                Some(next) => *current = next.into_iter().collect(),
                None => {
                    self.state = PassControllerState::Success;
                }
            },
            PassControllerState::Success => unreachable!("Processed after success"),
        }
    }

    fn does_not_reproduce(&mut self) {
        match &mut self.state {
            PassControllerState::InitialCollection { candidates } => {
                let candidates = mem::take(candidates);
                let half = candidates.len() / 2;
                let (first_half, second_half) = candidates.split_at(half);

                self.state = PassControllerState::Bisecting {
                    current: first_half.iter().cloned().collect(),
                    worklist: vec![second_half.to_owned()],
                };
            }
            PassControllerState::Bisecting { current, worklist } => {
                dbg!(&current, &worklist);
                todo!();
            }
            PassControllerState::Success => unreachable!("Processed after success"),
        }
    }

    fn no_change(&mut self) {
        match &self.state {
            PassControllerState::InitialCollection { candidates } => {
                assert!(
                    candidates.is_empty(),
                    "No change but received candidates: {candidates:?}"
                );
                self.state = PassControllerState::Success;
            }
            PassControllerState::Bisecting { current, .. } => {
                unreachable!("No change while bisecting, current was empty somehow: {current:?}");
            }
            PassControllerState::Success => {}
        }
    }

    fn is_finished(&mut self) -> bool {
        match &mut self.state {
            PassControllerState::InitialCollection { .. } => false,
            PassControllerState::Bisecting { .. } => false,
            PassControllerState::Success => true,
        }
    }

    pub fn can_process(&mut self, path: &[String]) -> bool {
        match &mut self.state {
            PassControllerState::InitialCollection { candidates } => {
                candidates.push(AstPath(path.to_owned()));
                true
            }
            PassControllerState::Bisecting { current, .. } => current.contains(path),
            PassControllerState::Success => {
                unreachable!("Processed further after success");
            }
        }
    }
}

macro_rules! tracking {
    () => {
        tracking!(visit_item_fn_mut);
        tracking!(visit_impl_item_method_mut);
        tracking!(visit_item_impl_mut);
        tracking!(visit_item_mod_mut);
    };
    (visit_item_fn_mut) => {
        fn visit_item_fn_mut(&mut self, func: &mut syn::ItemFn) {
            self.current_path.push(func.sig.ident.to_string());
            syn::visit_mut::visit_item_fn_mut(self, func);
            self.current_path.pop();
        }
    };
    (visit_impl_item_method_mut) => {
        fn visit_impl_item_method_mut(&mut self, method: &mut syn::ImplItemMethod) {
            self.current_path.push(method.sig.ident.to_string());
            syn::visit_mut::visit_impl_item_method_mut(self, method);
            self.current_path.pop();
        }
    };
    (visit_item_impl_mut) => {
        fn visit_item_impl_mut(&mut self, item: &mut syn::ItemImpl) {
            self.current_path
                .push(item.self_ty.clone().into_token_stream().to_string());
            syn::visit_mut::visit_item_impl_mut(self, item);
            self.current_path.pop();
        }
    };
    (visit_item_mod_mut) => {
        fn visit_item_mod_mut(&mut self, module: &mut syn::ItemMod) {
            self.current_path.push(module.ident.to_string());
            syn::visit_mut::visit_item_mod_mut(self, module);
            self.current_path.pop();
        }
    };
}

pub(crate) use tracking;
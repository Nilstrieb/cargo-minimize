use quote::ToTokens;
use syn::{parse_quote, visit_mut::VisitMut, Visibility};

use crate::processor::{tracking, Pass, PassController, ProcessState, SourceFile};

struct Visitor<'a> {
    pub_crate: Visibility,
    process_state: ProcessState,
    current_path: Vec<String>,
    checker: &'a mut PassController,
}

impl<'a> Visitor<'a> {
    fn new(checker: &'a mut PassController) -> Self {
        Self {
            process_state: ProcessState::NoChange,
            pub_crate: parse_quote! { pub(crate) },
            current_path: Vec::new(),
            checker,
        }
    }
}

impl VisitMut for Visitor<'_> {
    fn visit_visibility_mut(&mut self, vis: &mut Visibility) {
        if let Visibility::Public(_) = vis {
            if self.checker.can_process(&self.current_path) {
                self.process_state = ProcessState::Changed;
                *vis = self.pub_crate.clone();
            }
        }
    }

    tracking!();
}

#[derive(Default)]
pub struct Privatize {}

impl Pass for Privatize {
    fn process_file(
        &mut self,
        krate: &mut syn::File,
        _: &SourceFile,
        checker: &mut PassController,
    ) -> ProcessState {
        let mut visitor = Visitor::new(checker);
        visitor.visit_file_mut(krate);
        visitor.process_state
    }

    fn name(&self) -> &'static str {
        "privatize"
    }
}

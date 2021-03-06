//! Worker used by compiler.

use crate::ast;
use crate::collections::HashMap;
use crate::const_compiler::Consts;
use crate::index::{Index as _, Indexer};
use crate::index_scopes::IndexScopes;
use crate::items::Items;
use crate::macros::MacroCompiler;
use crate::query::Query;
use crate::CompileResult;
use crate::{
    CompileError, CompileErrorKind, CompileVisitor, Errors, LoadError, MacroContext, Options,
    Resolve as _, SourceLoader, Sources, Spanned as _, Storage, UnitBuilder, Warnings,
};
use runestick::{Component, Context, Item, Source, SourceId, Span};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

/// A single task that can be fed to the worker.
#[derive(Debug)]
pub(crate) enum Task {
    /// Load a file.
    LoadFile {
        /// The kind of loaded file.
        kind: LoadFileKind,
        /// The item of the file to load.
        item: Item,
        /// The source id of the item being loaded.
        source_id: SourceId,
    },
    /// An indexing task, which will index the specified item.
    Index(Index),
    /// Task to process an import.
    Import(Import),
    /// Task to expand a macro. This might produce additional indexing tasks.
    ExpandMacro(Macro),
}

/// The kind of the loaded module.
#[derive(Debug)]
pub(crate) enum LoadFileKind {
    /// A root file, which determined a URL root.
    Root,
    /// A loaded module, which inherits its root from the file it was loaded
    /// from.
    Module { root: Option<PathBuf> },
}

#[derive(Debug)]
pub(crate) enum IndexAst {
    /// Index the root of a file with the given item.
    File(ast::File),
    /// Index an item.
    Item(ast::Item),
    /// Index a new expression.
    Expr(ast::Expr),
}

pub(crate) struct Worker<'a> {
    pub(crate) queue: VecDeque<Task>,
    context: &'a Context,
    pub(crate) sources: &'a mut Sources,
    options: &'a Options,
    pub(crate) errors: &'a mut Errors,
    pub(crate) warnings: &'a mut Warnings,
    pub(crate) visitor: &'a mut dyn CompileVisitor,
    pub(crate) source_loader: &'a mut dyn SourceLoader,
    pub(crate) query: Query,
    pub(crate) loaded: HashMap<Item, (SourceId, Span)>,
    pub(crate) expanded: HashMap<Item, Expanded>,
}

impl<'a> Worker<'a> {
    /// Construct a new worker.
    pub(crate) fn new(
        queue: VecDeque<Task>,
        context: &'a Context,
        sources: &'a mut Sources,
        options: &'a Options,
        unit: Rc<RefCell<UnitBuilder>>,
        consts: Rc<RefCell<Consts>>,
        errors: &'a mut Errors,
        warnings: &'a mut Warnings,
        visitor: &'a mut dyn CompileVisitor,
        source_loader: &'a mut dyn SourceLoader,
        storage: Storage,
    ) -> Self {
        Self {
            queue,
            context,
            sources,
            options,
            errors,
            warnings,
            visitor,
            source_loader,
            query: Query::new(storage, unit, consts),
            loaded: HashMap::new(),
            expanded: HashMap::new(),
        }
    }

    /// Run the worker until the task queue is empty.
    pub(crate) fn run(&mut self) {
        while let Some(task) = self.queue.pop_front() {
            match task {
                Task::LoadFile {
                    kind,
                    item,
                    source_id,
                } => {
                    log::trace!("load file: {}", item);

                    let source = match self.sources.get(source_id).cloned() {
                        Some(source) => source,
                        None => {
                            self.errors.push(LoadError::internal(
                                source_id,
                                "missing queued source by id",
                            ));

                            continue;
                        }
                    };

                    let file = match crate::parse_all::<ast::File>(source.as_str()) {
                        Ok(file) => file,
                        Err(error) => {
                            self.errors.push(LoadError::new(source_id, error));

                            continue;
                        }
                    };

                    let root = match kind {
                        LoadFileKind::Root => source.path().map(ToOwned::to_owned),
                        LoadFileKind::Module { root } => root,
                    };

                    let items = Items::new(item.clone().into_vec());

                    self.queue.push_back(Task::Index(Index {
                        root,
                        item,
                        items,
                        source_id,
                        source,
                        scopes: IndexScopes::new(),
                        impl_items: Default::default(),
                        ast: IndexAst::File(file),
                    }));
                }
                Task::Index(index) => {
                    let Index {
                        root,
                        item,
                        items,
                        source_id,
                        source,
                        scopes,
                        impl_items,
                        ast,
                    } = index;

                    log::trace!("index: {}", item);

                    let mut indexer = Indexer {
                        root,
                        storage: self.query.storage.clone(),
                        loaded: &mut self.loaded,
                        query: &mut self.query,
                        queue: &mut self.queue,
                        sources: self.sources,
                        source_id,
                        source,
                        warnings: self.warnings,
                        items,
                        scopes,
                        impl_items,
                        visitor: self.visitor,
                        source_loader: self.source_loader,
                    };

                    let result = match ast {
                        IndexAst::File(ast) => match indexer.index(&ast) {
                            Ok(()) => Ok(None),
                            Err(error) => Err(error),
                        },
                        IndexAst::Item(ast) => match indexer.index(&ast) {
                            Ok(()) => Ok(None),
                            Err(error) => Err(error),
                        },
                        IndexAst::Expr(ast) => match indexer.index(&ast) {
                            Ok(()) => Ok(Some(Expanded::Expr(ast))),
                            Err(error) => Err(error),
                        },
                    };

                    match result {
                        Ok(expanded) => {
                            if let Some(expanded) = expanded {
                                self.expanded.insert(item, expanded);
                            }
                        }
                        Err(error) => {
                            self.errors.push(LoadError::new(source_id, error));
                        }
                    }
                }
                Task::Import(import) => {
                    log::trace!("import: {}", import.item);

                    let source_id = import.source_id;

                    let result = import.process(
                        self.context,
                        &self.query.storage,
                        &mut *self.query.unit.borrow_mut(),
                    );

                    if let Err(error) = result {
                        self.errors.push(LoadError::new(source_id, error));
                    }
                }
                Task::ExpandMacro(m) => {
                    let Macro {
                        kind,
                        root,
                        items,
                        ast,
                        source,
                        source_id,
                        scopes,
                        impl_items,
                    } = m;

                    let item = items.item();
                    let span = ast.span();

                    log::trace!("expand macro: {} => {:?}", item, source.source(ast.span()));

                    match kind {
                        MacroKind::Expr => (),
                        MacroKind::Item => {
                            // NB: item macros are not expanded into the second
                            // compiler phase (only indexed), so we need to
                            // restore their item position so that indexing is
                            // done on the correct item.
                            match items.pop() {
                                Some(Component::Macro(..)) => (),
                                _ => {
                                    self.errors.push(
                                        LoadError::new(source_id, CompileError::internal(
                                            &span,
                                            "expected macro item as last component of macro expansion",
                                        ))
                                    );

                                    continue;
                                }
                            }
                        }
                    }

                    let mut macro_context =
                        MacroContext::new(self.query.storage.clone(), source.clone());

                    let mut compiler = MacroCompiler {
                        storage: self.query.storage.clone(),
                        item: item.clone(),
                        macro_context: &mut macro_context,
                        options: self.options,
                        context: self.context,
                        unit: self.query.unit.clone(),
                        source: source.clone(),
                    };

                    let ast = match kind {
                        MacroKind::Expr => {
                            let ast = match compiler.eval_macro::<ast::Expr>(ast) {
                                Ok(ast) => ast,
                                Err(error) => {
                                    self.errors.push(LoadError::new(source_id, error));

                                    continue;
                                }
                            };

                            IndexAst::Expr(ast)
                        }
                        MacroKind::Item => {
                            let ast = match compiler.eval_macro::<ast::Item>(ast) {
                                Ok(ast) => ast,
                                Err(error) => {
                                    self.errors.push(LoadError::new(source_id, error));

                                    continue;
                                }
                            };

                            IndexAst::Item(ast)
                        }
                    };

                    self.queue.push_back(Task::Index(Index {
                        root,
                        item,
                        items,
                        source_id,
                        source,
                        scopes,
                        impl_items,
                        ast,
                    }));
                }
            }
        }
    }
}

/// An item that has been expanded by a macro.
pub(crate) enum Expanded {
    /// The expansion resulted in an expression.
    Expr(ast::Expr),
}

/// Indexing to process.
#[derive(Debug)]
pub(crate) struct Index {
    /// The root URL of the file which caused this item to be indexed.
    root: Option<PathBuf>,
    /// Item being built.
    item: Item,
    /// Path to index.
    items: Items,
    /// The source id where the item came from.
    source_id: SourceId,
    /// The source where the item came from.
    source: Arc<Source>,
    scopes: IndexScopes,
    impl_items: Vec<Item>,
    ast: IndexAst,
}

/// Import to process.
#[derive(Debug)]
pub(crate) struct Import {
    pub(crate) item: Item,
    pub(crate) ast: ast::ItemUse,
    pub(crate) source: Arc<Source>,
    pub(crate) source_id: usize,
}

impl Import {
    /// Process the import, populating the unit.
    pub(crate) fn process(
        self,
        context: &Context,
        storage: &Storage,
        unit: &mut UnitBuilder,
    ) -> CompileResult<()> {
        let Self {
            item,
            ast: decl_use,
            source,
            source_id,
        } = self;

        let span = decl_use.span();

        let mut name = Item::new();
        let first = decl_use.first.resolve(storage, &*source)?;
        name.push(first.as_ref());

        let mut it = decl_use.rest.iter();
        let last = it.next_back();

        for (_, c) in it {
            match c {
                ast::ItemUseComponent::Wildcard(t) => {
                    return Err(CompileError::new(t, CompileErrorKind::UnsupportedWildcard));
                }
                ast::ItemUseComponent::Ident(ident) => {
                    name.push(ident.resolve(storage, &*source)?.as_ref());
                }
            }
        }

        if let Some((_, c)) = last {
            match c {
                ast::ItemUseComponent::Wildcard(..) => {
                    let mut new_names = Vec::new();

                    if !context.contains_prefix(&name) && !unit.contains_prefix(&name) {
                        return Err(CompileError::new(
                            span,
                            CompileErrorKind::MissingModule { item: name },
                        ));
                    }

                    let iter = context
                        .iter_components(&name)
                        .chain(unit.iter_components(&name));

                    for c in iter {
                        let mut name = name.clone();
                        name.push(c);
                        new_names.push(name);
                    }

                    for name in new_names {
                        unit.new_import(item.clone(), &name, span, source_id)?;
                    }
                }
                ast::ItemUseComponent::Ident(ident) => {
                    name.push(ident.resolve(storage, &*source)?.as_ref());
                    unit.new_import(item, &name, span, source_id)?;
                }
            }
        } else {
            unit.new_import(item, &name, span, source_id)?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum MacroKind {
    Expr,
    Item,
}

#[derive(Debug)]
pub(crate) struct Macro {
    /// The kind of the macro.
    pub(crate) kind: MacroKind,
    /// The URL root at which the macro is being expanded.
    pub(crate) root: Option<PathBuf>,
    /// The item path where the macro is being expanded.
    pub(crate) items: Items,
    /// The AST of the macro call causing the expansion.
    pub(crate) ast: ast::MacroCall,
    /// The source where the macro is being expanded.
    pub(crate) source: Arc<Source>,
    /// The source id where the macro is being expanded.
    pub(crate) source_id: usize,
    /// Snapshot of index scopes when the macro was being expanded.
    pub(crate) scopes: IndexScopes,
    /// Snapshot of impl_items when the macro was being expanded.
    pub(crate) impl_items: Vec<Item>,
}

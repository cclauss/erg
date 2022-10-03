//! Defines `Context`.
//!
//! `Context` is used for type inference and type checking.
#![allow(clippy::result_unit_err)]
pub mod cache;
pub mod compare;
pub mod eval;
pub mod hint;
pub mod initialize;
pub mod inquire;
pub mod instantiate;
pub mod register;
pub mod test;
pub mod tyvar;

use std::fmt;
use std::mem;
use std::option::Option; // conflicting to Type::Option
use std::path::Path;

use erg_common::astr::AtomicStr;
use erg_common::config::ErgConfig;
use erg_common::dict::Dict;
use erg_common::error::Location;
use erg_common::impl_display_from_debug;
use erg_common::traits::{Locational, Stream};
use erg_common::vis::Visibility;
use erg_common::Str;
use erg_common::{fn_name, get_hash, log};

use erg_parser::ast::DefKind;
use erg_type::typaram::TyParam;
use erg_type::value::ValueObj;
use erg_type::{Predicate, TyBound, Type};
use Type::*;

use ast::{DefId, VarName};
use erg_parser::ast;
use erg_parser::token::Token;

use crate::context::instantiate::{ConstTemplate, TyVarContext};
use crate::error::{SingleTyCheckResult, TyCheckError, TyCheckErrors, TyCheckResult};
use crate::mod_cache::SharedModuleCache;
use crate::varinfo::{Mutability, ParamIdx, VarInfo, VarKind};
use Visibility::*;

const BUILTINS: &Str = &Str::ever("<builtins>");

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TraitInstance {
    pub sub_type: Type,
    pub sup_trait: Type,
}

impl std::fmt::Display for TraitInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TraitInstancePair{{{} <: {}}}",
            self.sub_type, self.sup_trait
        )
    }
}

impl TraitInstance {
    pub const fn new(sub_type: Type, sup_trait: Type) -> Self {
        TraitInstance {
            sub_type,
            sup_trait,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ClassDefType {
    Simple(Type),
    ImplTrait { class: Type, impl_trait: Type },
}

impl std::fmt::Display for ClassDefType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClassDefType::Simple(ty) => write!(f, "{ty}"),
            ClassDefType::ImplTrait { class, impl_trait } => {
                write!(f, "{class}|<: {impl_trait}|")
            }
        }
    }
}

impl ClassDefType {
    pub const fn impl_trait(class: Type, impl_trait: Type) -> Self {
        ClassDefType::ImplTrait { class, impl_trait }
    }
}

/// ```
/// # use erg_common::ty::{Type, TyParam};
/// # use erg_compiler::context::TyParamIdx;
///
/// let r = Type::mono_q("R");
/// let o = Type::mono_q("O");
/// let search_from = Type::poly("Add", vec![TyParam::t(r.clone()), TyParam::t(o.clone())]);
/// assert_eq!(TyParamIdx::search(&search_from, &o), Some(TyParamIdx::Nth(1)));
/// let i = Type::mono_q("I");
/// let f = Type::poly("F", vec![TyParam::t(o.clone()), TyParam::t(i.clone())]);
/// let search_from = Type::poly("Add", vec![TyParam::t(r), TyParam::t(f)]);
/// assert_eq!(TyParamIdx::search(&search_from, &o), Some(TyParamIdx::nested(1, TyParamIdx::Nth(0))));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TyParamIdx {
    Nth(usize),
    Nested { idx: usize, inner: Box<TyParamIdx> },
}

impl TyParamIdx {
    pub fn search(search_from: &Type, target: &Type) -> Option<Self> {
        match search_from {
            Type::Poly { params, .. } => {
                for (i, tp) in params.iter().enumerate() {
                    match tp {
                        TyParam::Type(t) if t.as_ref() == target => return Some(Self::Nth(i)),
                        TyParam::Type(t) if t.is_monomorphic() => {}
                        TyParam::Type(inner) => {
                            if let Some(inner) = Self::search(inner, target) {
                                return Some(Self::nested(i, inner));
                            }
                        }
                        other => todo!("{other:?}"),
                    }
                }
                None
            }
            _ => todo!(),
        }
    }

    /// ```python
    /// Nested(Nth(1), 0).select(F(X, G(Y, Z))) == Y
    /// ```
    pub fn select(self, from: &Type) -> Type {
        match self {
            Self::Nth(n) => {
                let tps = from.typarams();
                let tp = tps.get(n).unwrap();
                match tp {
                    TyParam::Type(t) => *t.clone(),
                    _ => todo!(),
                }
            }
            Self::Nested { .. } => todo!(),
        }
    }

    pub fn nested(idx: usize, inner: Self) -> Self {
        Self::Nested {
            idx,
            inner: Box::new(inner),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefaultInfo {
    NonDefault,
    WithDefault,
}

impl_display_from_debug!(DefaultInfo);

impl DefaultInfo {
    pub const fn has_default(&self) -> bool {
        matches!(self, DefaultInfo::WithDefault)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Variance {
    /// Output(T)
    Covariant, // 共変
    /// Input(T)
    Contravariant, // 反変
    #[default]
    Invariant, // 不変
}

impl_display_from_debug!(Variance);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParamSpec {
    pub(crate) name: Option<&'static str>, // TODO: nested
    pub(crate) t: Type,
    pub default_info: DefaultInfo,
}

impl ParamSpec {
    pub const fn new(name: Option<&'static str>, t: Type, default: DefaultInfo) -> Self {
        Self {
            name,
            t,
            default_info: default,
        }
    }

    pub const fn named(name: &'static str, t: Type, default: DefaultInfo) -> Self {
        Self::new(Some(name), t, default)
    }

    pub const fn named_nd(name: &'static str, t: Type) -> Self {
        Self::new(Some(name), t, DefaultInfo::NonDefault)
    }

    pub const fn t(name: &'static str, default: DefaultInfo) -> Self {
        Self::new(Some(name), Type, default)
    }

    pub const fn t_nd(name: &'static str) -> Self {
        Self::new(Some(name), Type, DefaultInfo::NonDefault)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContextKind {
    Func,
    Proc,
    Class,
    MethodDefs,
    Trait,
    StructuralTrait,
    Patch(Type),
    StructuralPatch(Type),
    GluePatch(TraitInstance),
    Module,
    Instant,
    Dummy,
}

impl From<DefKind> for ContextKind {
    fn from(kind: DefKind) -> Self {
        match kind {
            DefKind::Class | DefKind::Inherit => Self::Class,
            DefKind::Trait | DefKind::Subsume => Self::Trait,
            DefKind::StructuralTrait => Self::StructuralTrait,
            DefKind::ErgImport | DefKind::PyImport => Self::Module,
            DefKind::Other => Self::Instant,
        }
    }
}

impl ContextKind {
    pub const fn is_method_def(&self) -> bool {
        matches!(self, Self::MethodDefs)
    }

    pub const fn is_type(&self) -> bool {
        matches!(self, Self::Class | Self::Trait | Self::StructuralTrait)
    }

    pub fn is_class(&self) -> bool {
        matches!(self, Self::Class)
    }

    pub fn is_trait(&self) -> bool {
        matches!(self, Self::Trait | Self::StructuralTrait)
    }
}

/// 記号表に登録されているモードを表す
/// Preregister: サブルーチンまたは定数式、前方参照できる
/// Normal: 前方参照できない
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationMode {
    PreRegister,
    Normal,
}

/// Some Erg functions require additional operation by the compiler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    Import,
    PyImport,
    Del,
}

impl OperationKind {
    pub const fn is_erg_import(&self) -> bool {
        matches!(self, Self::Import)
    }
    pub const fn is_py_import(&self) -> bool {
        matches!(self, Self::PyImport)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContextInfo {
    mod_id: usize,
}

/// Represents the context of the current scope
///
/// Recursive functions/methods are highlighted with the prefix `rec_`, as performance may be significantly degraded.
#[derive(Debug, Clone)]
pub struct Context {
    pub(crate) name: Str,
    pub(crate) cfg: ErgConfig,
    pub(crate) kind: ContextKind,
    // Type bounds & Predicates (if the context kind is Subroutine)
    // ユーザー定義APIでのみ使う
    pub(crate) bounds: Vec<TyBound>,
    pub(crate) preds: Vec<Predicate>,
    /// for looking up the parent scope
    pub(crate) outer: Option<Box<Context>>,
    // e.g. { "Add": [ConstObjTemplate::App("Self", vec![])])
    pub(crate) const_param_defaults: Dict<Str, Vec<ConstTemplate>>,
    // Superclasses/supertraits by a patch are not included here
    // patchによってsuper class/traitになったものはここに含まれない
    pub(crate) super_classes: Vec<Type>, // if self is a patch, means patch classes
    pub(crate) super_traits: Vec<Type>,  // if self is not a trait, means implemented traits
    // method definitions, if the context is a type
    // specializations are included and needs to be separated out
    pub(crate) methods_list: Vec<(ClassDefType, Context)>,
    // K: method name, V: trait defines the method
    pub(crate) method_traits: Dict<Str, Vec<Type>>,
    /// K: method name, V: impl patch
    /// Provided methods can switch implementations on a scope-by-scope basis
    /// K: メソッド名, V: それを実装するパッチたち
    /// 提供メソッドはスコープごとに実装を切り替えることができる
    pub(crate) method_impl_patches: Dict<VarName, Vec<VarName>>,
    /// K: name of a trait, V: (type, monomorphised trait that the type implements)
    /// K: トレイトの名前, V: (型, その型が実装する単相化トレイト)
    /// e.g. { "Named": [(Type, Named), (Func, Named), ...], "Add": [(Nat, Add(Nat)), (Int, Add(Int)), ...], ... }
    pub(crate) trait_impls: Dict<Str, Vec<TraitInstance>>,
    /// stores declared names (not initialized)
    pub(crate) decls: Dict<VarName, VarInfo>,
    // stores defined names
    // 型の一致はHashMapでは判定できないため、keyはVarNameとして1つずつ見ていく
    /// ```python
    /// f [x, y], z = ...
    /// ```
    /// => params: vec![(None, [T; 2]), (Some("z"), U)]
    /// => locals: {"x": T, "y": T}
    /// TODO: impl params desugaring and replace to `Dict`
    pub(crate) params: Vec<(Option<VarName>, VarInfo)>,
    pub(crate) locals: Dict<VarName, VarInfo>,
    pub(crate) consts: Dict<VarName, ValueObj>,
    // {"Nat": ctx, "Int": ctx, ...}
    pub(crate) mono_types: Dict<VarName, (Type, Context)>,
    // Implementation Contexts for Polymorphic Types
    // Vec<TyParam> are specialization parameters
    // e.g. {"Array": [(Array(Nat), ctx), (Array(Int), ctx), (Array(Str), ctx), (Array(Obj), ctx), (Array('T), ctx)], ...}
    pub(crate) poly_types: Dict<VarName, (Type, Context)>,
    // patches can be accessed like normal records
    // but when used as a fallback to a type, values are traversed instead of accessing by keys
    pub(crate) patches: Dict<VarName, Context>,
    pub(crate) mod_cache: Option<SharedModuleCache>,
    pub(crate) py_mod_cache: Option<SharedModuleCache>,
    pub(crate) tv_ctx: Option<TyVarContext>,
    pub(crate) level: usize,
}

impl Default for Context {
    #[inline]
    fn default() -> Self {
        Self::new(
            "<dummy>".into(),
            ErgConfig::default(),
            ContextKind::Dummy,
            vec![],
            None,
            None,
            None,
            Self::TOP_LEVEL,
        )
    }
}

impl fmt::Display for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Context")
            .field("name", &self.name)
            .field("bounds", &self.bounds)
            .field("preds", &self.preds)
            .field("params", &self.params)
            .field("decls", &self.decls)
            .field("locals", &self.params)
            .field("consts", &self.consts)
            .field("mono_types", &self.mono_types)
            .field("poly_types", &self.poly_types)
            .field("patches", &self.patches)
            // .field("mod_cache", &self.mod_cache)
            .finish()
    }
}

impl Context {
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub fn new(
        name: Str,
        cfg: ErgConfig,
        kind: ContextKind,
        params: Vec<ParamSpec>,
        outer: Option<Context>,
        mod_cache: Option<SharedModuleCache>,
        py_mod_cache: Option<SharedModuleCache>,
        level: usize,
    ) -> Self {
        Self::with_capacity(
            name,
            cfg,
            kind,
            params,
            outer,
            mod_cache,
            py_mod_cache,
            0,
            level,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_capacity(
        name: Str,
        cfg: ErgConfig,
        kind: ContextKind,
        params: Vec<ParamSpec>,
        outer: Option<Context>,
        mod_cache: Option<SharedModuleCache>,
        py_mod_cache: Option<SharedModuleCache>,
        capacity: usize,
        level: usize,
    ) -> Self {
        let mut params_ = Vec::new();
        for (idx, param) in params.into_iter().enumerate() {
            let id = DefId(get_hash(&(&name, &param)));
            if let Some(name) = param.name {
                let idx = ParamIdx::Nth(idx);
                let kind = VarKind::parameter(id, idx, param.default_info);
                let muty = Mutability::from(name);
                let vi = VarInfo::new(param.t, muty, Private, kind, None);
                params_.push((Some(VarName::new(Token::static_symbol(name))), vi));
            } else {
                let idx = ParamIdx::Nth(idx);
                let kind = VarKind::parameter(id, idx, param.default_info);
                let muty = Mutability::Immutable;
                let vi = VarInfo::new(param.t, muty, Private, kind, None);
                params_.push((None, vi));
            }
        }
        Self {
            name,
            cfg,
            kind,
            bounds: vec![],
            preds: vec![],
            outer: outer.map(Box::new),
            super_classes: vec![],
            super_traits: vec![],
            methods_list: vec![],
            const_param_defaults: Dict::default(),
            method_traits: Dict::default(),
            method_impl_patches: Dict::default(),
            trait_impls: Dict::default(),
            params: params_,
            decls: Dict::default(),
            locals: Dict::with_capacity(capacity),
            consts: Dict::default(),
            mono_types: Dict::default(),
            poly_types: Dict::default(),
            mod_cache,
            py_mod_cache,
            tv_ctx: None,
            patches: Dict::default(),
            level,
        }
    }

    #[inline]
    pub fn mono(
        name: Str,
        cfg: ErgConfig,
        kind: ContextKind,
        outer: Option<Context>,
        mod_cache: Option<SharedModuleCache>,
        py_mod_cache: Option<SharedModuleCache>,
        level: usize,
    ) -> Self {
        Self::new(
            name,
            cfg,
            kind,
            vec![],
            outer,
            mod_cache,
            py_mod_cache,
            level,
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub fn poly(
        name: Str,
        cfg: ErgConfig,
        kind: ContextKind,
        params: Vec<ParamSpec>,
        outer: Option<Context>,
        mod_cache: Option<SharedModuleCache>,
        py_mod_cache: Option<SharedModuleCache>,
        capacity: usize,
        level: usize,
    ) -> Self {
        Self::with_capacity(
            name,
            cfg,
            kind,
            params,
            outer,
            mod_cache,
            py_mod_cache,
            capacity,
            level,
        )
    }

    pub fn poly_trait<S: Into<Str>>(
        name: S,
        params: Vec<ParamSpec>,
        cfg: ErgConfig,
        mod_cache: Option<SharedModuleCache>,
        py_mod_cache: Option<SharedModuleCache>,
        capacity: usize,
        level: usize,
    ) -> Self {
        let name = name.into();
        Self::poly(
            name,
            cfg,
            ContextKind::Trait,
            params,
            None,
            mod_cache,
            py_mod_cache,
            capacity,
            level,
        )
    }

    #[inline]
    pub fn builtin_poly_trait<S: Into<Str>>(
        name: S,
        params: Vec<ParamSpec>,
        capacity: usize,
    ) -> Self {
        Self::poly_trait(
            name,
            params,
            ErgConfig::default(),
            None,
            None,
            capacity,
            Self::TOP_LEVEL,
        )
    }

    pub fn poly_class<S: Into<Str>>(
        name: S,
        params: Vec<ParamSpec>,
        cfg: ErgConfig,
        mod_cache: Option<SharedModuleCache>,
        py_mod_cache: Option<SharedModuleCache>,
        capacity: usize,
        level: usize,
    ) -> Self {
        let name = name.into();
        Self::poly(
            name,
            cfg,
            ContextKind::Class,
            params,
            None,
            mod_cache,
            py_mod_cache,
            capacity,
            level,
        )
    }

    #[inline]
    pub fn builtin_poly_class<S: Into<Str>>(
        name: S,
        params: Vec<ParamSpec>,
        capacity: usize,
    ) -> Self {
        Self::poly_class(
            name,
            params,
            ErgConfig::default(),
            None,
            None,
            capacity,
            Self::TOP_LEVEL,
        )
    }

    #[inline]
    pub fn mono_trait<S: Into<Str>>(
        name: S,
        cfg: ErgConfig,
        mod_cache: Option<SharedModuleCache>,
        py_mod_cache: Option<SharedModuleCache>,
        capacity: usize,
        level: usize,
    ) -> Self {
        Self::poly_trait(name, vec![], cfg, mod_cache, py_mod_cache, capacity, level)
    }

    #[inline]
    pub fn builtin_mono_trait<S: Into<Str>>(name: S, capacity: usize) -> Self {
        Self::mono_trait(
            name,
            ErgConfig::default(),
            None,
            None,
            capacity,
            Self::TOP_LEVEL,
        )
    }

    #[inline]
    pub fn mono_class<S: Into<Str>>(
        name: S,
        cfg: ErgConfig,
        mod_cache: Option<SharedModuleCache>,
        py_mod_cache: Option<SharedModuleCache>,
        capacity: usize,
        level: usize,
    ) -> Self {
        Self::poly_class(name, vec![], cfg, mod_cache, py_mod_cache, capacity, level)
    }

    #[inline]
    pub fn builtin_mono_class<S: Into<Str>>(name: S, capacity: usize) -> Self {
        Self::mono_class(
            name,
            ErgConfig::default(),
            None,
            None,
            capacity,
            Self::TOP_LEVEL,
        )
    }

    #[inline]
    pub fn methods<S: Into<Str>>(
        name: S,
        cfg: ErgConfig,
        mod_cache: Option<SharedModuleCache>,
        py_mod_cache: Option<SharedModuleCache>,
        capacity: usize,
        level: usize,
    ) -> Self {
        Self::with_capacity(
            name.into(),
            cfg,
            ContextKind::MethodDefs,
            vec![],
            None,
            mod_cache,
            py_mod_cache,
            capacity,
            level,
        )
    }

    #[inline]
    pub fn builtin_methods<S: Into<Str>>(name: S, capacity: usize) -> Self {
        Self::methods(
            name,
            ErgConfig::default(),
            None,
            None,
            capacity,
            Self::TOP_LEVEL,
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub fn poly_patch<S: Into<Str>>(
        name: S,
        base: Type,
        params: Vec<ParamSpec>,
        cfg: ErgConfig,
        mod_cache: Option<SharedModuleCache>,
        py_mod_cache: Option<SharedModuleCache>,
        capacity: usize,
        level: usize,
    ) -> Self {
        Self::poly(
            name.into(),
            cfg,
            ContextKind::Patch(base),
            params,
            None,
            mod_cache,
            py_mod_cache,
            capacity,
            level,
        )
    }

    #[inline]
    pub fn builtin_poly_patch<S: Into<Str>>(
        name: S,
        base: Type,
        params: Vec<ParamSpec>,
        capacity: usize,
    ) -> Self {
        Self::poly_patch(
            name,
            base,
            params,
            ErgConfig::default(),
            None,
            None,
            capacity,
            Self::TOP_LEVEL,
        )
    }

    #[inline]
    pub fn module(
        name: Str,
        cfg: ErgConfig,
        mod_cache: Option<SharedModuleCache>,
        py_mod_cache: Option<SharedModuleCache>,
        capacity: usize,
    ) -> Self {
        Self::with_capacity(
            name,
            cfg,
            ContextKind::Module,
            vec![],
            None,
            mod_cache,
            py_mod_cache,
            capacity,
            Self::TOP_LEVEL,
        )
    }

    #[inline]
    pub fn builtin_module<S: Into<Str>>(name: S, capacity: usize) -> Self {
        Self::module(name.into(), ErgConfig::default(), None, None, capacity)
    }

    #[inline]
    pub fn instant(
        name: Str,
        cfg: ErgConfig,
        capacity: usize,
        mod_cache: Option<SharedModuleCache>,
        py_mod_cache: Option<SharedModuleCache>,
        outer: Context,
    ) -> Self {
        Self::with_capacity(
            name,
            cfg,
            ContextKind::Instant,
            vec![],
            Some(outer),
            mod_cache,
            py_mod_cache,
            capacity,
            Self::TOP_LEVEL,
        )
    }

    #[inline]
    pub fn caused_by(&self) -> AtomicStr {
        AtomicStr::arc(&self.name[..])
    }

    pub(crate) fn get_outer(&self) -> Option<&Context> {
        self.outer.as_ref().map(|x| x.as_ref())
    }

    pub(crate) fn path(&self) -> &Path {
        if let Some(outer) = self.get_outer() {
            outer.path()
        } else if self.kind == ContextKind::Module {
            Path::new(&self.name[..])
        } else {
            Path::new(&BUILTINS[..])
        }
    }

    /// Returns None if self is `<builtins>`.
    /// This avoids infinite loops.
    pub(crate) fn get_builtins(&self) -> Option<&Context> {
        // builtins中で定義した型等はmod_cacheがNoneになっている
        if self.path().to_string_lossy() != "<builtins>" {
            self.mod_cache
                .as_ref()
                .map(|cache| cache.ref_ctx(Path::new("<builtins>")).unwrap())
        } else {
            None
        }
    }

    pub(crate) fn grow(
        &mut self,
        name: &str,
        kind: ContextKind,
        vis: Visibility,
        tv_ctx: Option<TyVarContext>,
    ) -> TyCheckResult<()> {
        let name = if vis.is_public() {
            format!("{parent}.{name}", parent = self.name)
        } else {
            format!("{parent}::{name}", parent = self.name)
        };
        log!(info "{}: current namespace: {name}", fn_name!());
        self.outer = Some(Box::new(mem::take(self)));
        self.cfg = self.get_outer().unwrap().cfg.clone();
        self.mod_cache = self.get_outer().unwrap().mod_cache.clone();
        self.py_mod_cache = self.get_outer().unwrap().py_mod_cache.clone();
        self.tv_ctx = tv_ctx;
        self.name = name.into();
        self.kind = kind;
        Ok(())
    }

    pub fn pop(&mut self) -> Context {
        if let Some(parent) = self.outer.as_mut() {
            let parent = mem::take(parent);
            let ctx = mem::take(self);
            *self = *parent;
            log!(info "{}: current namespace: {}", fn_name!(), self.name);
            ctx
        } else {
            // toplevel
            mem::take(self)
        }
    }

    pub(crate) fn check_decls_and_pop(&mut self) -> Result<Context, TyCheckErrors> {
        self.check_decls()?;
        Ok(self.pop())
    }

    pub(crate) fn check_decls(&mut self) -> Result<(), TyCheckErrors> {
        let mut uninited_errs = TyCheckErrors::empty();
        for (name, vi) in self.decls.iter() {
            uninited_errs.push(TyCheckError::uninitialized_error(
                self.cfg.input.clone(),
                line!() as usize,
                name.loc(),
                self.caused_by(),
                name.inspect(),
                &vi.t,
            ));
        }
        if !uninited_errs.is_empty() {
            Err(uninited_errs)
        } else {
            Ok(())
        }
    }
}

/// for language server
impl Context {
    pub fn dir(&self) -> Vec<(&VarName, &VarInfo)> {
        let mut vars: Vec<_> = self.locals.iter().collect();
        if let Some(outer) = self.get_outer() {
            vars.extend(outer.dir());
        } else {
            vars.extend(self.get_builtins().unwrap().locals.iter());
        }
        vars
    }

    pub fn get_var_info(&self, name: &str) -> SingleTyCheckResult<(&VarName, &VarInfo)> {
        if let Some(info) = self.get_local_kv(name) {
            Ok(info)
        } else {
            if let Some(parent) = self.get_outer().or_else(|| self.get_builtins()) {
                return parent.get_var_info(name);
            }
            Err(TyCheckError::no_var_error(
                self.cfg.input.clone(),
                line!() as usize,
                Location::Unknown,
                self.caused_by(),
                name,
                self.get_similar_name(name),
            ))
        }
    }
}

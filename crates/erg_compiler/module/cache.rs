use std::borrow::Borrow;
use std::fmt;
use std::hash::Hash;
use std::path::Path;
use std::sync::Arc;

use erg_common::config::ErgConfig;
use erg_common::dict::Dict;
use erg_common::levenshtein::get_similar_name;
use erg_common::pathutil::NormalizedPathBuf;
use erg_common::shared::{
    MappedRwLockReadGuard, MappedRwLockWriteGuard, RwLockReadGuard, RwLockWriteGuard, Shared,
};
use erg_common::Str;

use crate::context::ModuleContext;
use crate::hir::HIR;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModId(usize);

impl ModId {
    pub const fn new(id: usize) -> Self {
        Self(id)
    }
    pub const fn builtin() -> Self {
        Self(0)
    }
    pub const fn main() -> Self {
        Self(1)
    }
}

#[derive(Debug, Clone)]
pub struct ModuleEntry {
    pub id: ModId, // builtin == 0, __main__ == 1
    pub hir: Option<HIR>,
    pub module: Arc<ModuleContext>,
}

impl fmt::Display for ModuleEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ModuleEntry(id = {}, name = {})",
            self.id.0, self.module.context.name
        )
    }
}

impl ModuleEntry {
    pub fn new(id: ModId, hir: Option<HIR>, ctx: ModuleContext) -> Self {
        Self {
            id,
            hir,
            module: Arc::new(ctx),
        }
    }

    pub fn builtin(ctx: ModuleContext) -> Self {
        Self {
            id: ModId::builtin(),
            hir: None,
            module: Arc::new(ctx),
        }
    }

    pub fn cfg(&self) -> &ErgConfig {
        &self.module.context.cfg
    }
}

/// Caches checked modules.
/// In addition to being queried here when re-imported, it is also used when linking
/// (Erg links all scripts defined in erg and outputs them to a single pyc file).
#[derive(Debug, Default)]
pub struct ModuleCache {
    cache: Dict<NormalizedPathBuf, ModuleEntry>,
    last_id: usize,
}

impl fmt::Display for ModuleCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ModuleCache {{")?;
        for (path, entry) in self.cache.iter() {
            writeln!(f, "{}: {}, ", path.display(), entry)?;
        }
        write!(f, "}}")
    }
}

impl ModuleCache {
    pub fn new() -> Self {
        Self {
            cache: Dict::new(),
            last_id: 0,
        }
    }

    pub fn get<P: Eq + Hash + ?Sized>(&self, path: &P) -> Option<&ModuleEntry>
    where
        NormalizedPathBuf: Borrow<P>,
    {
        self.cache.get(path)
    }

    pub fn get_mut<Q: Eq + Hash + ?Sized>(&mut self, path: &Q) -> Option<&mut ModuleEntry>
    where
        NormalizedPathBuf: Borrow<Q>,
    {
        self.cache.get_mut(path)
    }

    pub fn register(&mut self, path: NormalizedPathBuf, hir: Option<HIR>, ctx: ModuleContext) {
        self.last_id += 1;
        let id = ModId::new(self.last_id);
        let entry = ModuleEntry::new(id, hir, ctx);
        self.cache.insert(path, entry);
    }

    pub fn remove<Q: Eq + Hash + ?Sized>(&mut self, path: &Q) -> Option<ModuleEntry>
    where
        NormalizedPathBuf: Borrow<Q>,
    {
        self.cache.remove(path)
    }

    pub fn remove_by_id(&mut self, id: ModId) -> Option<ModuleEntry> {
        if let Some(name) = self.cache.iter().find_map(|(name, ent)| {
            if ent.id == id {
                Some(name.clone())
            } else {
                None
            }
        }) {
            self.remove(&name)
        } else {
            None
        }
    }

    pub fn get_similar_name(&self, name: &str) -> Option<Str> {
        get_similar_name(self.cache.iter().map(|(v, _)| v.to_str().unwrap()), name).map(Str::rc)
    }

    pub fn rename_path(&mut self, old: &Path, new: NormalizedPathBuf) {
        if let Some(entry) = self.cache.remove(old) {
            self.cache.insert(new, entry);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&NormalizedPathBuf, &ModuleEntry)> {
        self.cache.iter()
    }

    pub fn clear(&mut self) {
        self.cache.clear();
    }
}

#[derive(Debug, Clone, Default)]
pub struct SharedModuleCache(Shared<ModuleCache>);

impl fmt::Display for SharedModuleCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Shared{}", self.0)
    }
}

impl SharedModuleCache {
    pub fn new() -> Self {
        Self(Shared::new(ModuleCache::new()))
    }

    pub fn is_empty(&self) -> bool {
        self.0.borrow().cache.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.borrow().cache.len()
    }

    pub fn get<Q: Eq + Hash + ?Sized>(&self, path: &Q) -> Option<MappedRwLockReadGuard<ModuleEntry>>
    where
        NormalizedPathBuf: Borrow<Q>,
    {
        if self.0.borrow().get(path).is_some() {
            Some(RwLockReadGuard::map(self.0.borrow(), |cache| {
                cache.get(path).unwrap()
            }))
        } else {
            None
        }
    }

    pub fn get_mut<Q: Eq + Hash + ?Sized>(
        &self,
        path: &Q,
    ) -> Option<MappedRwLockWriteGuard<ModuleEntry>>
    where
        NormalizedPathBuf: Borrow<Q>,
    {
        if self.0.borrow().get(path).is_some() {
            Some(RwLockWriteGuard::map(self.0.borrow_mut(), |cache| {
                cache.get_mut(path).unwrap()
            }))
        } else {
            None
        }
    }

    pub fn get_ctx<Q: Eq + Hash + ?Sized>(&self, path: &Q) -> Option<Arc<ModuleContext>>
    where
        NormalizedPathBuf: Borrow<Q>,
    {
        self.0.borrow().get(path).map(|entry| entry.module.clone())
    }

    pub fn ref_ctx<Q: Eq + Hash + ?Sized>(
        &self,
        path: &Q,
    ) -> Option<MappedRwLockReadGuard<ModuleContext>>
    where
        NormalizedPathBuf: Borrow<Q>,
    {
        if self.0.borrow().get(path).is_some() {
            Some(RwLockReadGuard::map(self.0.borrow(), |cache| {
                cache.get(path).unwrap().module.as_ref()
            }))
        } else {
            None
        }
    }

    /// FIXME: see the comment in this function
    pub fn raw_ref_ctx<Q: Eq + Hash + ?Sized>(&self, path: &Q) -> Option<&ModuleContext>
    where
        NormalizedPathBuf: Borrow<Q>,
    {
        // Check if the module can be borrowed
        // If you delete `_ref`, this function returns `None` even if the key exists in rare cases
        let _ref = self.0.borrow();
        let ref_ = unsafe { self.0.as_ptr().as_ref().unwrap() };
        ref_.get(path).map(|entry| entry.module.as_ref())
    }

    pub fn register<P: Into<NormalizedPathBuf>>(
        &self,
        path: P,
        hir: Option<HIR>,
        ctx: ModuleContext,
    ) {
        self.0.borrow_mut().register(path.into(), hir, ctx);
    }

    pub fn remove<Q: Eq + Hash + ?Sized>(&self, path: &Q) -> Option<ModuleEntry>
    where
        NormalizedPathBuf: Borrow<Q>,
    {
        self.0.borrow_mut().remove(path)
    }

    pub fn remove_by_id(&self, id: ModId) -> Option<ModuleEntry> {
        self.0.borrow_mut().remove_by_id(id)
    }

    pub fn get_similar_name(&self, name: &str) -> Option<Str> {
        self.0.borrow().get_similar_name(name)
    }

    pub fn initialize(&self) {
        let builtin_path = NormalizedPathBuf::from("<builtins>");
        let Some(builtin) = self.remove(&builtin_path) else {
            return;
        };
        self.0.borrow_mut().clear();
        self.register(builtin_path, None, Arc::try_unwrap(builtin.module).unwrap());
    }

    pub fn rename_path<P: Into<NormalizedPathBuf>>(&self, path: &Path, new: P) {
        self.0.borrow_mut().rename_path(path, new.into());
    }

    pub fn ref_inner(&self) -> MappedRwLockReadGuard<Dict<NormalizedPathBuf, ModuleEntry>> {
        RwLockReadGuard::map(self.0.borrow(), |mc| &mc.cache)
    }
}

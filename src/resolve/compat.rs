//! CKAN CompatibleModules rule (CompatibilitySorter.CheckDepends): an identifier is installable
//! if one of its compatible versions has every `depends` met by an installed module, DLL or DLC,
//! or by a compatible non-DLC provider that is itself installable. A dependency cycle counts as met.

use std::collections::{HashMap, HashSet};

use crate::meta::{Module, Relationship};

use super::Context;
use super::world::{ModSet, provider_ids, satisfied};

pub(crate) struct Compat<'c> {
    ctx: Context<'c>,
    memo: HashMap<String, bool>,
    investigating: Vec<String>,
}

impl<'c> Compat<'c> {
    pub fn new(ctx: Context<'c>) -> Self {
        Compat { ctx, memo: HashMap::new(), investigating: Vec::new() }
    }

    fn version_ok(&self, m: &Module) -> bool {
        !m.is_dlc() && m.release_status <= self.ctx.stability && m.is_compatible(self.ctx.criteria)
    }

    /// Specific module version: compatible, within stability, and all depends can be met.
    pub fn module_ok(&mut self, m: &Module, sets: &[&ModSet], dlls: &HashSet<String>) -> bool {
        self.version_ok(m) && m.depends.iter().all(|d| self.dep_met(d, sets, dlls))
    }

    /// Identifier-level check with memo (CheckDepends).
    pub fn ident_ok(&mut self, id: &str, sets: &[&ModSet], dlls: &HashSet<String>) -> bool {
        if let Some(b) = self.memo.get(id) {
            return *b;
        }
        let ctx = self.ctx;
        self.investigating.push(id.to_string());
        let mut ok = false;
        for m in ctx.index.versions(id) {
            if !self.version_ok(m) {
                continue;
            }
            if m.depends.iter().all(|d| self.dep_met(d, sets, dlls)) {
                ok = true;
                break;
            }
        }
        self.investigating.pop();
        self.memo.insert(id.to_string(), ok);
        ok
    }

    /// One provider identifier counts for `name` if some version is a compatible non-DLC
    /// module that provides it.
    fn provider_ok(&self, pid: &str, name: &str) -> bool {
        self.ctx
            .index
            .versions(pid)
            .iter()
            .any(|m| self.version_ok(m) && m.provides_list().any(|p| p == name))
    }

    pub fn dep_met(&mut self, d: &Relationship, sets: &[&ModSet], dlls: &HashSet<String>) -> bool {
        if satisfied(d, sets, dlls) {
            return true;
        }
        let ctx = self.ctx;
        let mut seen: HashSet<&str> = HashSet::new();
        for name in d.names() {
            for pid in provider_ids(&ctx, name) {
                if !seen.insert(pid) || !self.provider_ok(pid, name) {
                    continue;
                }
                if self.investigating.iter().any(|x| x == pid) {
                    return true; // circular dependency: count as met
                }
                if self.ident_ok(pid, sets, dlls) {
                    return true;
                }
            }
        }
        false
    }

    /// First depends of `m` that cannot be met (for error text).
    pub fn first_unmet(&mut self, m: &Module, sets: &[&ModSet], dlls: &HashSet<String>) -> Option<String> {
        m.depends.iter().find(|d| !self.dep_met(d, sets, dlls)).map(|d| d.to_string())
    }
}

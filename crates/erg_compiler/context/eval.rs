use std::mem;

use erg_common::consts::DEBUG_MODE;
use erg_common::dict::Dict;
use erg_common::error::Location;
#[allow(unused)]
use erg_common::log;
use erg_common::set::Set;
use erg_common::traits::{Locational, Stream};
use erg_common::{dict, fmt_vec, fn_name, option_enum_unwrap, set};
use erg_common::{ArcArray, Str};
use OpKind::*;

use erg_parser::ast::Dict as AstDict;
use erg_parser::ast::Set as AstSet;
use erg_parser::ast::*;
use erg_parser::desugar::Desugarer;
use erg_parser::token::{Token, TokenKind};

use crate::ty::constructors::{
    array_t, dict_t, mono, poly, proj, proj_call, ref_, ref_mut, refinement, set_t, subr_t,
    tp_enum, tuple_t, v_enum,
};
use crate::ty::free::{FreeTyVar, HasLevel};
use crate::ty::typaram::{OpKind, TyParam};
use crate::ty::value::{GenTypeObj, TypeObj, ValueObj};
use crate::ty::{ConstSubr, HasType, Predicate, SubrKind, Type, UserConstSubr, ValueArgs};

use crate::context::instantiate_spec::ParamKind;
use crate::context::{ClassDefType, Context, ContextKind, RegistrationMode};
use crate::error::{EvalError, EvalErrors, EvalResult, SingleEvalResult};

use super::instantiate::TyVarCache;
use Type::{Failure, Never, Subr};

macro_rules! feature_error {
    ($ctx: expr, $loc: expr, $name: expr) => {
        $crate::feature_error!(EvalErrors, EvalError, $ctx, $loc, $name)
    };
}
macro_rules! unreachable_error {
    ($ctx: expr) => {
        $crate::unreachable_error!(EvalErrors, EvalError, $ctx)
    };
}

#[inline]
pub fn type_from_token_kind(kind: TokenKind) -> Type {
    use TokenKind::*;

    match kind {
        NatLit | BinLit | OctLit | HexLit => Type::Nat,
        IntLit => Type::Int,
        RatioLit => Type::Ratio,
        StrLit | DocComment => Type::Str,
        BoolLit => Type::Bool,
        NoneLit => Type::NoneType,
        EllipsisLit => Type::Ellipsis,
        InfLit => Type::Inf,
        other => panic!("this has no type: {other}"),
    }
}

fn op_to_name(op: OpKind) -> &'static str {
    match op {
        OpKind::Add => "__add__",
        OpKind::Sub => "__sub__",
        OpKind::Mul => "__mul__",
        OpKind::Div => "__div__",
        OpKind::FloorDiv => "__floordiv__",
        OpKind::Mod => "__mod__",
        OpKind::Pow => "__pow__",
        OpKind::Pos => "__pos__",
        OpKind::Neg => "__neg__",
        OpKind::Eq => "__eq__",
        OpKind::Ne => "__ne__",
        OpKind::Lt => "__lt__",
        OpKind::Le => "__le__",
        OpKind::Gt => "__gt__",
        OpKind::Ge => "__ge__",
        OpKind::As => "__as__",
        OpKind::And => "__and__",
        OpKind::Or => "__or__",
        OpKind::Not => "__not__",
        OpKind::Invert => "__invert__",
        OpKind::BitAnd => "__bitand__",
        OpKind::BitOr => "__bitor__",
        OpKind::BitXor => "__bitxor__",
        OpKind::Shl => "__shl__",
        OpKind::Shr => "__shr__",
    }
}

impl Context {
    fn try_get_op_kind_from_token(&self, token: &Token) -> EvalResult<OpKind> {
        match token.kind {
            TokenKind::Plus => Ok(OpKind::Add),
            TokenKind::Minus => Ok(OpKind::Sub),
            TokenKind::Star => Ok(OpKind::Mul),
            TokenKind::Slash => Ok(OpKind::Div),
            TokenKind::FloorDiv => Ok(OpKind::FloorDiv),
            TokenKind::Pow => Ok(OpKind::Pow),
            TokenKind::Mod => Ok(OpKind::Mod),
            TokenKind::DblEq => Ok(OpKind::Eq),
            TokenKind::NotEq => Ok(OpKind::Ne),
            TokenKind::Less => Ok(OpKind::Lt),
            TokenKind::Gre => Ok(OpKind::Gt),
            TokenKind::LessEq => Ok(OpKind::Le),
            TokenKind::GreEq => Ok(OpKind::Ge),
            TokenKind::AndOp => Ok(OpKind::And),
            TokenKind::OrOp => Ok(OpKind::Or),
            TokenKind::BitAnd => Ok(OpKind::BitAnd),
            TokenKind::BitXor => Ok(OpKind::BitXor),
            TokenKind::BitOr => Ok(OpKind::BitOr),
            TokenKind::Shl => Ok(OpKind::Shl),
            TokenKind::Shr => Ok(OpKind::Shr),
            TokenKind::PrePlus => Ok(OpKind::Pos),
            TokenKind::PreMinus => Ok(OpKind::Neg),
            TokenKind::PreBitNot => Ok(OpKind::Invert),
            _other => Err(EvalErrors::from(EvalError::not_const_expr(
                self.cfg.input.clone(),
                line!() as usize,
                token.loc(),
                self.caused_by(),
            ))),
        }
    }

    fn get_mod_ctx_from_acc(&self, acc: &Accessor) -> Option<&Context> {
        match acc {
            Accessor::Ident(ident) => self.get_mod(ident.inspect()),
            Accessor::Attr(attr) => {
                let Expr::Accessor(acc) = attr.obj.as_ref() else { return None; };
                self.get_mod_ctx_from_acc(acc)
                    .and_then(|ctx| ctx.get_mod(attr.ident.inspect()))
            }
            _ => None,
        }
    }

    fn eval_const_acc(&self, acc: &Accessor) -> EvalResult<ValueObj> {
        match acc {
            Accessor::Ident(ident) => self.eval_const_ident(ident),
            Accessor::Attr(attr) => match self.eval_const_expr(&attr.obj) {
                Ok(obj) => Ok(self.eval_attr(obj, &attr.ident)?),
                Err(err) => {
                    if let Expr::Accessor(acc) = attr.obj.as_ref() {
                        if let Some(mod_ctx) = self.get_mod_ctx_from_acc(acc) {
                            if let Ok(obj) = mod_ctx.eval_const_ident(&attr.ident) {
                                return Ok(obj);
                            }
                        }
                    }
                    Err(err)
                }
            },
            other => {
                feature_error!(self, other.loc(), &format!("eval {other}")).map_err(Into::into)
            }
        }
    }

    fn eval_const_ident(&self, ident: &Identifier) -> EvalResult<ValueObj> {
        if let Some(val) = self.rec_get_const_obj(ident.inspect()) {
            Ok(val.clone())
        } else if self.kind.is_subr() {
            feature_error!(self, ident.loc(), "const parameters")
        } else if ident.is_const() {
            Err(EvalErrors::from(EvalError::no_var_error(
                self.cfg.input.clone(),
                line!() as usize,
                ident.loc(),
                self.caused_by(),
                ident.inspect(),
                self.get_similar_name(ident.inspect()),
            )))
        } else {
            Err(EvalErrors::from(EvalError::not_const_expr(
                self.cfg.input.clone(),
                line!() as usize,
                ident.loc(),
                self.caused_by(),
            )))
        }
    }

    fn eval_attr(&self, obj: ValueObj, ident: &Identifier) -> SingleEvalResult<ValueObj> {
        let field = self
            .instantiate_field(ident)
            .map_err(|mut errs| errs.remove(0))?;
        if let Some(val) = obj.try_get_attr(&field) {
            return Ok(val);
        }
        if let ValueObj::Type(t) = &obj {
            if let Some(sups) = self.get_nominal_super_type_ctxs(t.typ()) {
                for ctx in sups {
                    if let Some(val) = ctx.consts.get(ident.inspect()) {
                        return Ok(val.clone());
                    }
                    for (_, methods) in ctx.methods_list.iter() {
                        if let Some(v) = methods.consts.get(ident.inspect()) {
                            return Ok(v.clone());
                        }
                    }
                }
            }
        }
        Err(EvalError::no_attr_error(
            self.cfg.input.clone(),
            line!() as usize,
            ident.loc(),
            self.caused_by(),
            &obj.t(),
            ident.inspect(),
            None,
        ))
    }

    fn eval_const_bin(&self, bin: &BinOp) -> EvalResult<ValueObj> {
        let lhs = self.eval_const_expr(&bin.args[0])?;
        let rhs = self.eval_const_expr(&bin.args[1])?;
        let op = self.try_get_op_kind_from_token(&bin.op)?;
        self.eval_bin(op, lhs, rhs)
    }

    fn eval_const_unary(&self, unary: &UnaryOp) -> EvalResult<ValueObj> {
        let val = self.eval_const_expr(&unary.args[0])?;
        let op = self.try_get_op_kind_from_token(&unary.op)?;
        self.eval_unary_val(op, val)
    }

    fn eval_args(&self, args: &Args) -> EvalResult<ValueArgs> {
        let mut evaluated_pos_args = vec![];
        for arg in args.pos_args().iter() {
            let val = self.eval_const_expr(&arg.expr)?;
            evaluated_pos_args.push(val);
        }
        let mut evaluated_kw_args = dict! {};
        for arg in args.kw_args().iter() {
            let val = self.eval_const_expr(&arg.expr)?;
            evaluated_kw_args.insert(arg.keyword.inspect().clone(), val);
        }
        Ok(ValueArgs::new(evaluated_pos_args, evaluated_kw_args))
    }

    fn eval_const_call(&self, call: &Call) -> EvalResult<ValueObj> {
        if let Expr::Accessor(acc) = call.obj.as_ref() {
            match acc {
                Accessor::Ident(ident) => {
                    let obj = self.rec_get_const_obj(ident.inspect()).ok_or_else(|| {
                        EvalError::no_var_error(
                            self.cfg.input.clone(),
                            line!() as usize,
                            ident.loc(),
                            self.caused_by(),
                            ident.inspect(),
                            self.get_similar_name(ident.inspect()),
                        )
                    })?;
                    let subr = option_enum_unwrap!(obj, ValueObj::Subr)
                        .ok_or_else(|| {
                            EvalError::type_mismatch_error(
                                self.cfg.input.clone(),
                                line!() as usize,
                                ident.loc(),
                                self.caused_by(),
                                ident.inspect(),
                                None,
                                &mono("Subroutine"),
                                &obj.t(),
                                self.get_candidates(&obj.t()),
                                None,
                            )
                        })?
                        .clone();
                    let args = self.eval_args(&call.args)?;
                    self.call(subr, args, call.loc())
                }
                // TODO: eval attr
                Accessor::Attr(_attr) => Err(EvalErrors::from(EvalError::not_const_expr(
                    self.cfg.input.clone(),
                    line!() as usize,
                    call.loc(),
                    self.caused_by(),
                ))),
                // TODO: eval type app
                Accessor::TypeApp(_type_app) => Err(EvalErrors::from(EvalError::not_const_expr(
                    self.cfg.input.clone(),
                    line!() as usize,
                    call.loc(),
                    self.caused_by(),
                ))),
                _ => unreachable!(),
            }
        } else {
            Err(EvalErrors::from(EvalError::not_const_expr(
                self.cfg.input.clone(),
                line!() as usize,
                call.loc(),
                self.caused_by(),
            )))
        }
    }

    fn call(&self, subr: ConstSubr, args: ValueArgs, loc: Location) -> EvalResult<ValueObj> {
        match subr {
            ConstSubr::User(user) => {
                // HACK: should avoid cloning
                let mut subr_ctx = Context::instant(
                    user.name.clone(),
                    self.cfg.clone(),
                    2,
                    self.shared.clone(),
                    self.clone(),
                );
                // TODO: var_args
                for (arg, sig) in args
                    .pos_args
                    .into_iter()
                    .zip(user.params.non_defaults.iter())
                {
                    let name = VarName::from_str(sig.inspect().unwrap().clone());
                    subr_ctx.consts.insert(name, arg);
                }
                for (name, arg) in args.kw_args.into_iter() {
                    subr_ctx.consts.insert(VarName::from_str(name), arg);
                }
                subr_ctx.eval_const_block(&user.block())
            }
            ConstSubr::Builtin(builtin) => builtin.call(args, self).map_err(|mut e| {
                if e.0.loc.is_unknown() {
                    e.0.loc = loc;
                }
                EvalErrors::from(EvalError::new(
                    *e.0,
                    self.cfg.input.clone(),
                    self.caused_by(),
                ))
            }),
            ConstSubr::Gen(gen) => gen.call(args, self).map_err(|mut e| {
                if e.0.loc.is_unknown() {
                    e.0.loc = loc;
                }
                EvalErrors::from(EvalError::new(
                    *e.0,
                    self.cfg.input.clone(),
                    self.caused_by(),
                ))
            }),
        }
    }

    fn eval_const_def(&mut self, def: &Def) -> EvalResult<ValueObj> {
        if def.is_const() {
            let __name__ = def.sig.ident().unwrap().inspect();
            let vis = self.instantiate_vis_modifier(def.sig.vis())?;
            let tv_cache = match &def.sig {
                Signature::Subr(subr) => {
                    let ty_cache =
                        self.instantiate_ty_bounds(&subr.bounds, RegistrationMode::Normal)?;
                    Some(ty_cache)
                }
                Signature::Var(_) => None,
            };
            // TODO: set params
            let kind = ContextKind::from(def);
            self.grow(__name__, kind, vis, tv_cache);
            let obj = self.eval_const_block(&def.body.block).map_err(|errs| {
                self.pop();
                errs
            })?;
            let (_ctx, errs) = self.check_decls_and_pop();
            self.register_gen_const(def.sig.ident().unwrap(), obj, def.def_kind().is_other())?;
            if errs.is_empty() {
                Ok(ValueObj::None)
            } else {
                Err(errs)
            }
        } else {
            Err(EvalErrors::from(EvalError::not_const_expr(
                self.cfg.input.clone(),
                line!() as usize,
                def.body.block.loc(),
                self.caused_by(),
            )))
        }
    }

    fn eval_const_array(&self, arr: &Array) -> EvalResult<ValueObj> {
        let mut elems = vec![];
        match arr {
            Array::Normal(arr) => {
                for elem in arr.elems.pos_args().iter() {
                    let elem = self.eval_const_expr(&elem.expr)?;
                    elems.push(elem);
                }
                Ok(ValueObj::Array(ArcArray::from(elems)))
            }
            _ => Err(EvalErrors::from(EvalError::not_const_expr(
                self.cfg.input.clone(),
                line!() as usize,
                arr.loc(),
                self.caused_by(),
            ))),
        }
    }

    fn eval_const_set(&self, set: &AstSet) -> EvalResult<ValueObj> {
        let mut elems = vec![];
        match set {
            AstSet::Normal(arr) => {
                for elem in arr.elems.pos_args().iter() {
                    let elem = self.eval_const_expr(&elem.expr)?;
                    elems.push(elem);
                }
                Ok(ValueObj::Set(Set::from(elems)))
            }
            _ => Err(EvalErrors::from(EvalError::not_const_expr(
                self.cfg.input.clone(),
                line!() as usize,
                set.loc(),
                self.caused_by(),
            ))),
        }
    }

    fn eval_const_dict(&self, dict: &AstDict) -> EvalResult<ValueObj> {
        let mut elems = dict! {};
        match dict {
            AstDict::Normal(dic) => {
                for elem in dic.kvs.iter() {
                    let key = self.eval_const_expr(&elem.key)?;
                    let value = self.eval_const_expr(&elem.value)?;
                    elems.insert(key, value);
                }
                Ok(ValueObj::Dict(elems))
            }
            _ => Err(EvalErrors::from(EvalError::not_const_expr(
                self.cfg.input.clone(),
                line!() as usize,
                dict.loc(),
                self.caused_by(),
            ))),
        }
    }

    fn eval_const_tuple(&self, tuple: &Tuple) -> EvalResult<ValueObj> {
        let mut elems = vec![];
        match tuple {
            Tuple::Normal(arr) => {
                for elem in arr.elems.pos_args().iter() {
                    let elem = self.eval_const_expr(&elem.expr)?;
                    elems.push(elem);
                }
            }
        }
        Ok(ValueObj::Tuple(ArcArray::from(elems)))
    }

    fn eval_const_record(&self, record: &Record) -> EvalResult<ValueObj> {
        match record {
            Record::Normal(rec) => self.eval_const_normal_record(rec),
            Record::Mixed(mixed) => self.eval_const_normal_record(
                &Desugarer::desugar_shortened_record_inner(mixed.clone()),
            ),
        }
    }

    fn eval_const_normal_record(&self, record: &NormalRecord) -> EvalResult<ValueObj> {
        let mut attrs = vec![];
        // HACK: should avoid cloning
        let mut record_ctx = Context::instant(
            Str::ever("<unnamed record>"),
            self.cfg.clone(),
            2,
            self.shared.clone(),
            self.clone(),
        );
        for attr in record.attrs.iter() {
            // let name = attr.sig.ident().map(|i| i.inspect());
            let elem = record_ctx.eval_const_block(&attr.body.block)?;
            let ident = match &attr.sig {
                Signature::Var(var) => match &var.pat {
                    VarPattern::Ident(ident) => self.instantiate_field(ident)?,
                    other => {
                        return feature_error!(self, other.loc(), &format!("record field: {other}"))
                    }
                },
                other => {
                    return feature_error!(self, other.loc(), &format!("record field: {other}"))
                }
            };
            attrs.push((ident, elem));
        }
        Ok(ValueObj::Record(attrs.into_iter().collect()))
    }

    /// FIXME: grow
    fn eval_const_lambda(&self, lambda: &Lambda) -> EvalResult<ValueObj> {
        let mut tmp_tv_cache =
            self.instantiate_ty_bounds(&lambda.sig.bounds, RegistrationMode::Normal)?;
        let mut non_default_params = Vec::with_capacity(lambda.sig.params.non_defaults.len());
        for sig in lambda.sig.params.non_defaults.iter() {
            let pt = self.instantiate_param_ty(
                sig,
                None,
                &mut tmp_tv_cache,
                RegistrationMode::Normal,
                ParamKind::NonDefault,
                false,
            )?;
            non_default_params.push(pt);
        }
        let var_params = if let Some(p) = lambda.sig.params.var_params.as_ref() {
            let pt = self.instantiate_param_ty(
                p,
                None,
                &mut tmp_tv_cache,
                RegistrationMode::Normal,
                ParamKind::VarParams,
                false,
            )?;
            Some(pt)
        } else {
            None
        };
        let mut default_params = Vec::with_capacity(lambda.sig.params.defaults.len());
        for sig in lambda.sig.params.defaults.iter() {
            let expr = self.eval_const_expr(&sig.default_val)?;
            let pt = self.instantiate_param_ty(
                &sig.sig,
                None,
                &mut tmp_tv_cache,
                RegistrationMode::Normal,
                ParamKind::Default(expr.t()),
                false,
            )?;
            default_params.push(pt);
        }
        // HACK: should avoid cloning
        let mut lambda_ctx = Context::instant(
            Str::ever("<lambda>"),
            self.cfg.clone(),
            0,
            self.shared.clone(),
            self.clone(),
        );
        let return_t = v_enum(set! {lambda_ctx.eval_const_block(&lambda.body)?});
        let sig_t = subr_t(
            SubrKind::from(lambda.op.kind),
            non_default_params.clone(),
            var_params,
            default_params.clone(),
            return_t,
        );
        let block =
            erg_parser::Parser::validate_const_block(lambda.body.clone()).map_err(|_| {
                EvalErrors::from(EvalError::not_const_expr(
                    self.cfg.input.clone(),
                    line!() as usize,
                    lambda.loc(),
                    self.caused_by(),
                ))
            })?;
        let sig_t = self.generalize_t(sig_t);
        let subr = ConstSubr::User(UserConstSubr::new(
            Str::ever("<lambda>"),
            lambda.sig.params.clone(),
            block,
            sig_t,
        ));
        Ok(ValueObj::Subr(subr))
    }

    pub(crate) fn eval_lit(&self, lit: &Literal) -> EvalResult<ValueObj> {
        let t = type_from_token_kind(lit.token.kind);
        ValueObj::from_str(t, lit.token.content.clone()).ok_or_else(|| {
            EvalError::invalid_literal(
                self.cfg.input.clone(),
                line!() as usize,
                lit.token.loc(),
                self.caused_by(),
            )
            .into()
        })
    }

    pub(crate) fn eval_const_expr(&self, expr: &Expr) -> EvalResult<ValueObj> {
        match expr {
            Expr::Literal(lit) => self.eval_lit(lit),
            Expr::Accessor(acc) => self.eval_const_acc(acc),
            Expr::BinOp(bin) => self.eval_const_bin(bin),
            Expr::UnaryOp(unary) => self.eval_const_unary(unary),
            Expr::Call(call) => self.eval_const_call(call),
            Expr::Array(arr) => self.eval_const_array(arr),
            Expr::Set(set) => self.eval_const_set(set),
            Expr::Dict(dict) => self.eval_const_dict(dict),
            Expr::Tuple(tuple) => self.eval_const_tuple(tuple),
            Expr::Record(rec) => self.eval_const_record(rec),
            Expr::Lambda(lambda) => self.eval_const_lambda(lambda),
            // FIXME: type check
            Expr::TypeAscription(tasc) => self.eval_const_expr(&tasc.expr),
            other => Err(EvalErrors::from(EvalError::not_const_expr(
                self.cfg.input.clone(),
                line!() as usize,
                other.loc(),
                self.caused_by(),
            ))),
        }
    }

    // Evaluate compile-time expression (just Expr on AST) instead of evaluating ConstExpr
    // Return Err if it cannot be evaluated at compile time
    // ConstExprを評価するのではなく、コンパイル時関数の式(AST上ではただのExpr)を評価する
    // コンパイル時評価できないならNoneを返す
    pub(crate) fn eval_const_chunk(&mut self, expr: &Expr) -> EvalResult<ValueObj> {
        match expr {
            // TODO: ClassDef, PatchDef
            Expr::Def(def) => self.eval_const_def(def),
            Expr::Literal(lit) => self.eval_lit(lit),
            Expr::Accessor(acc) => self.eval_const_acc(acc),
            Expr::BinOp(bin) => self.eval_const_bin(bin),
            Expr::UnaryOp(unary) => self.eval_const_unary(unary),
            Expr::Call(call) => self.eval_const_call(call),
            Expr::Array(arr) => self.eval_const_array(arr),
            Expr::Set(set) => self.eval_const_set(set),
            Expr::Dict(dict) => self.eval_const_dict(dict),
            Expr::Tuple(tuple) => self.eval_const_tuple(tuple),
            Expr::Record(rec) => self.eval_const_record(rec),
            Expr::Lambda(lambda) => self.eval_const_lambda(lambda),
            Expr::TypeAscription(tasc) => self.eval_const_expr(&tasc.expr),
            other => Err(EvalErrors::from(EvalError::not_const_expr(
                self.cfg.input.clone(),
                line!() as usize,
                other.loc(),
                self.caused_by(),
            ))),
        }
    }

    pub(crate) fn eval_const_block(&mut self, block: &Block) -> EvalResult<ValueObj> {
        for chunk in block.iter().rev().skip(1).rev() {
            self.eval_const_chunk(chunk)?;
        }
        self.eval_const_chunk(block.last().unwrap())
    }

    fn eval_bin(&self, op: OpKind, lhs: ValueObj, rhs: ValueObj) -> EvalResult<ValueObj> {
        match op {
            Add => lhs.try_add(rhs).ok_or_else(|| {
                EvalErrors::from(EvalError::unreachable(
                    self.cfg.input.clone(),
                    fn_name!(),
                    line!(),
                ))
            }),
            Sub => lhs.try_sub(rhs).ok_or_else(|| {
                EvalErrors::from(EvalError::unreachable(
                    self.cfg.input.clone(),
                    fn_name!(),
                    line!(),
                ))
            }),
            Mul => lhs.try_mul(rhs).ok_or_else(|| {
                EvalErrors::from(EvalError::unreachable(
                    self.cfg.input.clone(),
                    fn_name!(),
                    line!(),
                ))
            }),
            Div => lhs.try_div(rhs).ok_or_else(|| {
                EvalErrors::from(EvalError::unreachable(
                    self.cfg.input.clone(),
                    fn_name!(),
                    line!(),
                ))
            }),
            FloorDiv => lhs.try_floordiv(rhs).ok_or_else(|| {
                EvalErrors::from(EvalError::unreachable(
                    self.cfg.input.clone(),
                    fn_name!(),
                    line!(),
                ))
            }),
            Gt => lhs.try_gt(rhs).ok_or_else(|| {
                EvalErrors::from(EvalError::unreachable(
                    self.cfg.input.clone(),
                    fn_name!(),
                    line!(),
                ))
            }),
            Ge => lhs.try_ge(rhs).ok_or_else(|| {
                EvalErrors::from(EvalError::unreachable(
                    self.cfg.input.clone(),
                    fn_name!(),
                    line!(),
                ))
            }),
            Lt => lhs.try_lt(rhs).ok_or_else(|| {
                EvalErrors::from(EvalError::unreachable(
                    self.cfg.input.clone(),
                    fn_name!(),
                    line!(),
                ))
            }),
            Le => lhs.try_le(rhs).ok_or_else(|| {
                EvalErrors::from(EvalError::unreachable(
                    self.cfg.input.clone(),
                    fn_name!(),
                    line!(),
                ))
            }),
            Eq => lhs.try_eq(rhs).ok_or_else(|| {
                EvalErrors::from(EvalError::unreachable(
                    self.cfg.input.clone(),
                    fn_name!(),
                    line!(),
                ))
            }),
            Ne => lhs.try_ne(rhs).ok_or_else(|| {
                EvalErrors::from(EvalError::unreachable(
                    self.cfg.input.clone(),
                    fn_name!(),
                    line!(),
                ))
            }),
            Or | BitOr => match (lhs, rhs) {
                (ValueObj::Bool(l), ValueObj::Bool(r)) => Ok(ValueObj::Bool(l || r)),
                (ValueObj::Int(l), ValueObj::Int(r)) => Ok(ValueObj::Int(l | r)),
                (ValueObj::Type(lhs), ValueObj::Type(rhs)) => Ok(self.eval_or_type(lhs, rhs)),
                _ => Err(EvalErrors::from(EvalError::unreachable(
                    self.cfg.input.clone(),
                    fn_name!(),
                    line!(),
                ))),
            },
            And | BitAnd => match (lhs, rhs) {
                (ValueObj::Bool(l), ValueObj::Bool(r)) => Ok(ValueObj::Bool(l && r)),
                (ValueObj::Int(l), ValueObj::Int(r)) => Ok(ValueObj::Int(l & r)),
                (ValueObj::Type(lhs), ValueObj::Type(rhs)) => Ok(self.eval_and_type(lhs, rhs)),
                _ => Err(EvalErrors::from(EvalError::unreachable(
                    self.cfg.input.clone(),
                    fn_name!(),
                    line!(),
                ))),
            },
            BitXor => match (lhs, rhs) {
                (ValueObj::Bool(l), ValueObj::Bool(r)) => Ok(ValueObj::Bool(l ^ r)),
                (ValueObj::Int(l), ValueObj::Int(r)) => Ok(ValueObj::Int(l ^ r)),
                _ => Err(EvalErrors::from(EvalError::unreachable(
                    self.cfg.input.clone(),
                    fn_name!(),
                    line!(),
                ))),
            },
            _other => Err(EvalErrors::from(EvalError::unreachable(
                self.cfg.input.clone(),
                fn_name!(),
                line!(),
            ))),
        }
    }

    fn eval_or_type(&self, lhs: TypeObj, rhs: TypeObj) -> ValueObj {
        match (lhs, rhs) {
            (
                TypeObj::Builtin {
                    t: l,
                    meta_t: Type::ClassType,
                },
                TypeObj::Builtin {
                    t: r,
                    meta_t: Type::ClassType,
                },
            ) => ValueObj::builtin_class(self.union(&l, &r)),
            (
                TypeObj::Builtin {
                    t: l,
                    meta_t: Type::TraitType,
                },
                TypeObj::Builtin {
                    t: r,
                    meta_t: Type::TraitType,
                },
            ) => ValueObj::builtin_trait(self.union(&l, &r)),
            (TypeObj::Builtin { t: l, meta_t: _ }, TypeObj::Builtin { t: r, meta_t: _ }) => {
                ValueObj::builtin_type(self.union(&l, &r))
            }
            (lhs, rhs) => ValueObj::gen_t(GenTypeObj::union(
                self.union(lhs.typ(), rhs.typ()),
                lhs,
                rhs,
            )),
        }
    }

    fn eval_and_type(&self, lhs: TypeObj, rhs: TypeObj) -> ValueObj {
        match (lhs, rhs) {
            (
                TypeObj::Builtin {
                    t: l,
                    meta_t: Type::ClassType,
                },
                TypeObj::Builtin {
                    t: r,
                    meta_t: Type::ClassType,
                },
            ) => ValueObj::builtin_class(self.intersection(&l, &r)),
            (
                TypeObj::Builtin {
                    t: l,
                    meta_t: Type::TraitType,
                },
                TypeObj::Builtin {
                    t: r,
                    meta_t: Type::TraitType,
                },
            ) => ValueObj::builtin_trait(self.intersection(&l, &r)),
            (TypeObj::Builtin { t: l, meta_t: _ }, TypeObj::Builtin { t: r, meta_t: _ }) => {
                ValueObj::builtin_type(self.intersection(&l, &r))
            }
            (lhs, rhs) => ValueObj::gen_t(GenTypeObj::intersection(
                self.intersection(lhs.typ(), rhs.typ()),
                lhs,
                rhs,
            )),
        }
    }

    fn eval_not_type(&self, ty: TypeObj) -> ValueObj {
        match ty {
            TypeObj::Builtin {
                t,
                meta_t: Type::ClassType,
            } => ValueObj::builtin_class(self.complement(&t)),
            TypeObj::Builtin {
                t,
                meta_t: Type::TraitType,
            } => ValueObj::builtin_trait(self.complement(&t)),
            TypeObj::Builtin { t, meta_t: _ } => ValueObj::builtin_type(self.complement(&t)),
            // FIXME:
            _ => ValueObj::Illegal,
        }
    }

    pub(crate) fn eval_bin_tp(
        &self,
        op: OpKind,
        lhs: TyParam,
        rhs: TyParam,
    ) -> EvalResult<TyParam> {
        match (lhs, rhs) {
            (TyParam::Value(lhs), TyParam::Value(rhs)) => {
                self.eval_bin(op, lhs, rhs).map(TyParam::value)
            }
            (TyParam::Dict(l), TyParam::Dict(r)) if op == OpKind::Add => {
                Ok(TyParam::Dict(l.concat(r)))
            }
            (TyParam::Array(l), TyParam::Array(r)) if op == OpKind::Add => {
                Ok(TyParam::Array([l, r].concat()))
            }
            (TyParam::FreeVar(fv), r) if fv.is_linked() => {
                self.eval_bin_tp(op, fv.crack().clone(), r)
            }
            (TyParam::FreeVar(_), _) if op.is_comparison() => Ok(TyParam::value(true)),
            // _: Nat <= 10 => true
            // TODO: maybe this is wrong, we should do the type-checking of `<=`
            (TyParam::Erased(t), rhs)
                if op.is_comparison()
                    && self.supertype_of(&t, &self.get_tp_t(&rhs).unwrap_or(Type::Obj)) =>
            {
                Ok(TyParam::value(true))
            }
            (l, TyParam::FreeVar(fv)) if fv.is_linked() => {
                self.eval_bin_tp(op, l, fv.crack().clone())
            }
            (_, TyParam::FreeVar(_)) if op.is_comparison() => Ok(TyParam::value(true)),
            // 10 <= _: Nat => true
            (lhs, TyParam::Erased(t))
                if op.is_comparison()
                    && self.supertype_of(&self.get_tp_t(&lhs).unwrap_or(Type::Obj), &t) =>
            {
                Ok(TyParam::value(true))
            }
            (e @ TyParam::Erased(_), _) | (_, e @ TyParam::Erased(_)) => Ok(e),
            (lhs @ TyParam::FreeVar(_), rhs) => Ok(TyParam::bin(op, lhs, rhs)),
            (lhs, rhs @ TyParam::FreeVar(_)) => Ok(TyParam::bin(op, lhs, rhs)),
            (l, r) => feature_error!(self, Location::Unknown, &format!("{l} {op} {r}"))
                .map_err(Into::into),
        }
    }

    fn eval_unary_val(&self, op: OpKind, val: ValueObj) -> EvalResult<ValueObj> {
        match op {
            Pos => Err(EvalErrors::from(EvalError::unreachable(
                self.cfg.input.clone(),
                fn_name!(),
                line!(),
            ))),
            Neg => Err(EvalErrors::from(EvalError::unreachable(
                self.cfg.input.clone(),
                fn_name!(),
                line!(),
            ))),
            Invert => Err(EvalErrors::from(EvalError::unreachable(
                self.cfg.input.clone(),
                fn_name!(),
                line!(),
            ))),
            Not => match val {
                ValueObj::Bool(b) => Ok(ValueObj::Bool(!b)),
                ValueObj::Type(lhs) => Ok(self.eval_not_type(lhs)),
                _ => Err(EvalErrors::from(EvalError::unreachable(
                    self.cfg.input.clone(),
                    fn_name!(),
                    line!(),
                ))),
            },
            _other => unreachable_error!(self),
        }
    }

    pub(crate) fn eval_unary_tp(&self, op: OpKind, val: TyParam) -> EvalResult<TyParam> {
        match val {
            TyParam::Value(c) => self.eval_unary_val(op, c).map(TyParam::Value),
            TyParam::FreeVar(fv) if fv.is_linked() => self.eval_unary_tp(op, fv.crack().clone()),
            e @ TyParam::Erased(_) => Ok(e),
            TyParam::FreeVar(fv) if fv.is_unbound() => {
                feature_error!(self, Location::Unknown, &format!("{op} {fv}"))
            }
            other => feature_error!(self, Location::Unknown, &format!("{op} {other}")),
        }
    }

    fn eval_succ_func(&self, val: ValueObj) -> EvalResult<ValueObj> {
        match val {
            ValueObj::Bool(b) => Ok(ValueObj::Nat(b as u64 + 1)),
            ValueObj::Nat(n) => Ok(ValueObj::Nat(n + 1)),
            ValueObj::Int(n) => Ok(ValueObj::Int(n + 1)),
            // TODO:
            ValueObj::Float(n) => Ok(ValueObj::Float(n + f64::EPSILON)),
            ValueObj::Inf | ValueObj::NegInf => Ok(val),
            _ => Err(EvalErrors::from(EvalError::unreachable(
                self.cfg.input.clone(),
                fn_name!(),
                line!(),
            ))),
        }
    }

    fn eval_pred_func(&self, val: ValueObj) -> EvalResult<ValueObj> {
        match val {
            ValueObj::Bool(_) => Ok(ValueObj::Nat(0)),
            ValueObj::Nat(n) => Ok(ValueObj::Nat(n.saturating_sub(1))),
            ValueObj::Int(n) => Ok(ValueObj::Int(n - 1)),
            // TODO:
            ValueObj::Float(n) => Ok(ValueObj::Float(n - f64::EPSILON)),
            ValueObj::Inf | ValueObj::NegInf => Ok(val),
            _ => Err(EvalErrors::from(EvalError::unreachable(
                self.cfg.input.clone(),
                fn_name!(),
                line!(),
            ))),
        }
    }

    pub(crate) fn eval_app(&self, name: Str, args: Vec<TyParam>) -> EvalResult<TyParam> {
        if let Ok(mut value_args) = args
            .iter()
            .map(|tp| self.convert_tp_into_value(tp.clone()))
            .collect::<Result<Vec<_>, _>>()
        {
            match &name[..] {
                "succ" => self
                    .eval_succ_func(value_args.remove(0))
                    .map(TyParam::Value),
                "pred" => self
                    .eval_pred_func(value_args.remove(0))
                    .map(TyParam::Value),
                _ => {
                    log!(err "eval_app({name}({}))", fmt_vec(&args));
                    Ok(TyParam::app(name, args))
                }
            }
        } else {
            log!(err "eval_app({name}({}))", fmt_vec(&args));
            Ok(TyParam::app(name, args))
        }
    }

    /// Quantified variables, etc. are returned as is.
    /// 量化変数などはそのまま返す
    pub(crate) fn eval_tp(&self, p: TyParam) -> EvalResult<TyParam> {
        match p {
            TyParam::FreeVar(fv) if fv.is_linked() => self.eval_tp(fv.crack().clone()),
            TyParam::FreeVar(_) => Ok(p),
            TyParam::Mono(name) => self
                .rec_get_const_obj(&name)
                .map(|v| TyParam::value(v.clone()))
                .ok_or_else(|| {
                    EvalErrors::from(EvalError::unreachable(
                        self.cfg.input.clone(),
                        fn_name!(),
                        line!(),
                    ))
                }),
            TyParam::App { name, args } => self.eval_app(name, args),
            TyParam::BinOp { op, lhs, rhs } => self.eval_bin_tp(op, *lhs, *rhs),
            TyParam::UnaryOp { op, val } => self.eval_unary_tp(op, *val),
            TyParam::Array(tps) => {
                let mut new_tps = Vec::with_capacity(tps.len());
                for tp in tps {
                    new_tps.push(self.eval_tp(tp)?);
                }
                Ok(TyParam::Array(new_tps))
            }
            TyParam::Tuple(tps) => {
                let mut new_tps = Vec::with_capacity(tps.len());
                for tp in tps {
                    new_tps.push(self.eval_tp(tp)?);
                }
                Ok(TyParam::Tuple(new_tps))
            }
            TyParam::Dict(dic) => {
                let mut new_dic = dict! {};
                for (k, v) in dic.into_iter() {
                    new_dic.insert(self.eval_tp(k)?, self.eval_tp(v)?);
                }
                Ok(TyParam::Dict(new_dic))
            }
            TyParam::Set(set) => {
                let mut new_set = set! {};
                for v in set.into_iter() {
                    new_set.insert(self.eval_tp(v)?);
                }
                Ok(TyParam::Set(new_set))
            }
            TyParam::Type(_) | TyParam::Erased(_) | TyParam::Value(_) => Ok(p.clone()),
            _other => feature_error!(self, Location::Unknown, "???"),
        }
    }

    /// Evaluate `substituted`.
    /// If the evaluation fails, return a harmless type (filled with `Failure`) and errors
    pub(crate) fn eval_t_params(
        &self,
        substituted: Type,
        level: usize,
        t_loc: &impl Locational,
    ) -> Result<Type, (Type, EvalErrors)> {
        match substituted {
            Type::FreeVar(fv) if fv.is_linked() => {
                self.eval_t_params(fv.crack().clone(), level, t_loc)
            }
            Type::Subr(mut subr) => {
                for pt in subr.non_default_params.iter_mut() {
                    *pt.typ_mut() = match self.eval_t_params(mem::take(pt.typ_mut()), level, t_loc)
                    {
                        Ok(t) => t,
                        Err((_, errs)) => {
                            // `mem::take` replaces the type with `Type::Failure`, so it can return as is
                            return Err((Subr(subr), errs));
                        }
                    };
                }
                if let Some(var_args) = subr.var_params.as_mut() {
                    *var_args.typ_mut() =
                        match self.eval_t_params(mem::take(var_args.typ_mut()), level, t_loc) {
                            Ok(t) => t,
                            Err((_, errs)) => return Err((Subr(subr), errs)),
                        };
                }
                for pt in subr.default_params.iter_mut() {
                    *pt.typ_mut() = match self.eval_t_params(mem::take(pt.typ_mut()), level, t_loc)
                    {
                        Ok(t) => t,
                        Err((_, errs)) => return Err((Subr(subr), errs)),
                    };
                }
                match self.eval_t_params(*subr.return_t, level, t_loc) {
                    Ok(return_t) => Ok(subr_t(
                        subr.kind,
                        subr.non_default_params,
                        subr.var_params.map(|v| *v),
                        subr.default_params,
                        return_t,
                    )),
                    Err((_, errs)) => {
                        let subr = subr_t(
                            subr.kind,
                            subr.non_default_params,
                            subr.var_params.map(|v| *v),
                            subr.default_params,
                            Failure,
                        );
                        Err((subr, errs))
                    }
                }
            }
            Type::Refinement(refine) => {
                let pred = self
                    .eval_pred(*refine.pred)
                    .map_err(|errs| (Failure, errs))?;
                Ok(refinement(refine.var, *refine.t, pred))
            }
            // [?T; 0].MutType! == [?T; !0]
            // ?T(<: Add(?R(:> Int))).Output == ?T(<: Add(?R)).Output
            // ?T(:> Int, <: Add(?R(:> Int))).Output == Int
            Type::Proj { lhs, rhs } => self
                .eval_proj(*lhs, rhs, level, t_loc)
                .map_err(|errs| (Failure, errs)),
            Type::ProjCall {
                lhs,
                attr_name,
                args,
            } => self
                .eval_proj_call(*lhs, attr_name, args, level, t_loc)
                .map_err(|errs| (Failure, errs)),
            Type::Ref(l) => match self.eval_t_params(*l, level, t_loc) {
                Ok(t) => Ok(ref_(t)),
                Err((_, errs)) => Err((ref_(Failure), errs)),
            },
            Type::RefMut { before, after } => {
                let before = match self.eval_t_params(*before, level, t_loc) {
                    Ok(before) => before,
                    Err((_, errs)) => {
                        return Err((ref_mut(Failure, after.map(|x| *x)), errs));
                    }
                };
                let after = if let Some(after) = after {
                    let aft = match self.eval_t_params(*after, level, t_loc) {
                        Ok(aft) => aft,
                        Err((_, errs)) => {
                            return Err((ref_mut(before, Some(Failure)), errs));
                        }
                    };
                    Some(aft)
                } else {
                    None
                };
                Ok(ref_mut(before, after))
            }
            Type::Poly { name, mut params } => {
                for p in params.iter_mut() {
                    *p = match self.eval_tp(mem::take(p)) {
                        Ok(p) => p,
                        Err(errs) => {
                            // TODO: detoxify `p`
                            return Err((poly(name, params), errs));
                        }
                    };
                }
                Ok(poly(name, params))
            }
            Type::And(l, r) => {
                let l = match self.eval_t_params(*l, level, t_loc) {
                    Ok(l) => l,
                    Err((_, errs)) => {
                        return Err((Failure, errs));
                    }
                };
                let r = match self.eval_t_params(*r, level, t_loc) {
                    Ok(r) => r,
                    Err((_, errs)) => {
                        // L and Never == Never
                        return Err((Failure, errs));
                    }
                };
                Ok(self.intersection(&l, &r))
            }
            Type::Or(l, r) => {
                let l = match self.eval_t_params(*l, level, t_loc) {
                    Ok(l) => l,
                    Err((_, errs)) => {
                        return Err((Failure, errs));
                    }
                };
                let r = match self.eval_t_params(*r, level, t_loc) {
                    Ok(r) => r,
                    Err((_, errs)) => {
                        // L or Never == L
                        return Err((l, errs));
                    }
                };
                Ok(self.union(&l, &r))
            }
            Type::Not(ty) => match self.eval_t_params(*ty, level, t_loc) {
                Ok(ty) => Ok(self.complement(&ty)),
                Err((_, errs)) => Err((Failure, errs)),
            },
            other if other.is_monomorphic() => Ok(other),
            other => feature_error!(self, t_loc.loc(), "???").map_err(|errs| (other, errs)),
        }
    }

    /// lhs: mainly class
    pub(crate) fn eval_proj(
        &self,
        lhs: Type,
        rhs: Str,
        level: usize,
        t_loc: &impl Locational,
    ) -> EvalResult<Type> {
        if let Never | Failure = lhs {
            return Ok(lhs);
        }
        // Currently Erg does not allow projection-types to be evaluated with type variables included.
        // All type variables will be dereferenced or fail.
        let (sub, opt_sup) = match lhs.clone() {
            Type::FreeVar(fv) if fv.is_linked() => {
                return self
                    .eval_t_params(proj(fv.crack().clone(), rhs), level, t_loc)
                    .map_err(|(_, errs)| errs)
            }
            Type::FreeVar(fv) if fv.is_unbound() => {
                let (sub, sup) = fv.get_subsup().unwrap();
                (sub, Some(sup))
            }
            other => (other, None),
        };
        // cannot determine at this point
        if sub == Type::Never {
            return Ok(proj(lhs, rhs));
        }
        // in Methods
        if let Some(ctx) = self.get_same_name_context(&sub.qual_name()) {
            if let Some(t) =
                ctx.validate_and_project(&sub, opt_sup.as_ref(), &rhs, self, level, t_loc)
            {
                return Ok(t);
            }
        }
        let ty_ctxs = match self.get_nominal_super_type_ctxs(&sub) {
            Some(ty_ctxs) => ty_ctxs,
            None => {
                let errs = EvalErrors::from(EvalError::type_not_found(
                    self.cfg.input.clone(),
                    line!() as usize,
                    t_loc.loc(),
                    self.caused_by(),
                    &sub,
                ));
                return Err(errs);
            }
        };
        for ty_ctx in ty_ctxs {
            if let Some(t) =
                self.validate_and_project(&sub, opt_sup.as_ref(), &rhs, ty_ctx, level, t_loc)
            {
                return Ok(t);
            }
            for (class, methods) in ty_ctx.methods_list.iter() {
                match (&class, &opt_sup) {
                    (ClassDefType::ImplTrait { impl_trait, .. }, Some(sup)) => {
                        if !self.supertype_of(impl_trait, sup) {
                            continue;
                        }
                    }
                    (ClassDefType::ImplTrait { impl_trait, .. }, None) => {
                        if !self.supertype_of(impl_trait, &sub) {
                            continue;
                        }
                    }
                    _ => {}
                }
                if let Some(t) =
                    self.validate_and_project(&sub, opt_sup.as_ref(), &rhs, methods, level, t_loc)
                {
                    return Ok(t);
                }
            }
        }
        if let Some(fv) = lhs.as_free() {
            let (sub, sup) = fv.get_subsup().unwrap();
            if self.is_trait(&sup) && !self.trait_impl_exists(&sub, &sup) {
                // link to `Never` to prevent double errors from being reported
                lhs.link(&Never);
                let sub = if cfg!(feature = "debug") {
                    sub
                } else {
                    self.coerce(sub, t_loc)?
                };
                let sup = if cfg!(feature = "debug") {
                    sup
                } else {
                    self.coerce(sup, t_loc)?
                };
                return Err(EvalErrors::from(EvalError::no_trait_impl_error(
                    self.cfg.input.clone(),
                    line!() as usize,
                    &sub,
                    &sup,
                    t_loc.loc(),
                    self.caused_by(),
                    self.get_simple_type_mismatch_hint(&sup, &sub),
                )));
            }
        }
        // if the target can't be found in the supertype, the type will be dereferenced.
        // In many cases, it is still better to determine the type variable than if the target is not found.
        let coerced = self.coerce(lhs.clone(), t_loc)?;
        if lhs != coerced {
            let proj = proj(coerced, rhs);
            self.eval_t_params(proj, level, t_loc)
                .map_err(|(_, errs)| errs)
        } else {
            let proj = proj(lhs, rhs);
            let errs = EvalErrors::from(EvalError::no_candidate_error(
                self.cfg.input.clone(),
                line!() as usize,
                &proj,
                t_loc.loc(),
                self.caused_by(),
                self.get_no_candidate_hint(&proj),
            ));
            Err(errs)
        }
    }

    /// ```erg
    /// TyParam::Type(Int) => Int
    /// (Int, Str) => Tuple([Int, Str])
    /// {x = Int; y = Int} => Type::Record({x = Int, y = Int})
    /// {Str: Int} => Dict({Str: Int})
    /// {1, 2} => {I: Int | I == 1 or I == 2 } (== {1, 2})
    /// ```
    pub(crate) fn convert_tp_into_type(&self, tp: TyParam) -> Result<Type, TyParam> {
        match tp {
            TyParam::Tuple(tps) => {
                let mut ts = vec![];
                for elem_tp in tps {
                    ts.push(self.convert_tp_into_type(elem_tp)?);
                }
                Ok(tuple_t(ts))
            }
            TyParam::Set(tps) => {
                let mut union = Type::Never;
                for tp in tps.iter() {
                    union = self.union(&union, &self.get_tp_t(tp).unwrap_or(Type::Obj));
                }
                Ok(tp_enum(union, tps))
            }
            TyParam::Record(rec) => {
                let mut fields = dict! {};
                for (name, tp) in rec {
                    fields.insert(name, self.convert_tp_into_type(tp)?);
                }
                Ok(Type::Record(fields))
            }
            TyParam::Dict(dict) => {
                let mut kvs = dict! {};
                for (key, val) in dict {
                    kvs.insert(
                        self.convert_tp_into_type(key)?,
                        self.convert_tp_into_type(val)?,
                    );
                }
                Ok(Type::from(kvs))
            }
            TyParam::FreeVar(fv) if fv.is_linked() => self.convert_tp_into_type(fv.crack().clone()),
            TyParam::Type(t) => Ok(t.as_ref().clone()),
            TyParam::Mono(name) => Ok(Type::Mono(name)),
            TyParam::App { name, args } => Ok(Type::Poly { name, params: args }),
            TyParam::Proj { obj, attr } => {
                let lhs = self.convert_tp_into_type(*obj)?;
                Ok(lhs.proj(attr))
            }
            // TyParam::Erased(_t) => Ok(Type::Obj),
            TyParam::Value(v) => self.convert_value_into_type(v).map_err(TyParam::Value),
            // TODO: Dict, Set
            other => Err(other),
        }
    }

    pub(crate) fn convert_tp_into_value(&self, tp: TyParam) -> Result<ValueObj, TyParam> {
        match tp {
            TyParam::Value(v) => Ok(v),
            other => Err(other),
        }
    }

    pub(crate) fn convert_singular_type_into_value(&self, typ: Type) -> Result<ValueObj, Type> {
        match typ {
            Type::Refinement(ref refine) => {
                if let Predicate::Equal { rhs, .. } = refine.pred.as_ref() {
                    self.convert_tp_into_value(rhs.clone()).map_err(|_| typ)
                } else {
                    Err(typ)
                }
            }
            _ => Err(typ),
        }
    }

    pub(crate) fn convert_value_into_type(&self, val: ValueObj) -> Result<Type, ValueObj> {
        match val {
            ValueObj::Ellipsis => Ok(Type::Ellipsis),
            ValueObj::Type(t) => match t {
                TypeObj::Builtin { t, .. } => Ok(t),
                TypeObj::Generated(gen) => Ok(gen.into_typ()),
            },
            ValueObj::Record(rec) => {
                let mut fields = dict! {};
                for (name, val) in rec.into_iter() {
                    fields.insert(name, self.convert_value_into_type(val)?);
                }
                Ok(Type::Record(fields))
            }
            ValueObj::Tuple(ts) => {
                let mut new_ts = vec![];
                for v in ts.iter() {
                    new_ts.push(self.convert_value_into_type(v.clone())?);
                }
                Ok(tuple_t(new_ts))
            }
            ValueObj::Array(arr) => {
                let len = TyParam::value(arr.len());
                let mut union = Type::Never;
                for v in arr.iter().cloned() {
                    union = self.union(&union, &self.convert_value_into_type(v)?);
                }
                Ok(array_t(union, len))
            }
            ValueObj::Set(set) => Ok(v_enum(set)),
            ValueObj::Subr(subr) => subr.as_type(self).ok_or(ValueObj::Subr(subr)),
            other => Err(other),
        }
    }

    /// * Ok if `value` can be upcast to `TyParam`
    /// * Err if it is simply converted to `TyParam::Value(value)`
    pub(crate) fn convert_value_into_tp(value: ValueObj) -> Result<TyParam, TyParam> {
        match value {
            ValueObj::Type(t) => Ok(TyParam::t(t.into_typ())),
            ValueObj::Array(arr) => {
                let mut new_arr = vec![];
                for v in arr.iter().cloned() {
                    let tp = match Self::convert_value_into_tp(v) {
                        Ok(tp) => tp,
                        Err(tp) => tp,
                    };
                    new_arr.push(tp);
                }
                Ok(TyParam::Array(new_arr))
            }
            ValueObj::Tuple(vs) => {
                let mut new_ts = vec![];
                for v in vs.iter().cloned() {
                    let tp = match Self::convert_value_into_tp(v) {
                        Ok(tp) => tp,
                        Err(tp) => tp,
                    };
                    new_ts.push(tp);
                }
                Ok(TyParam::Tuple(new_ts))
            }
            ValueObj::Dict(dict) => {
                let mut new_dict = dict! {};
                for (k, v) in dict.into_iter() {
                    let k = match Self::convert_value_into_tp(k) {
                        Ok(tp) => tp,
                        Err(tp) => tp,
                    };
                    let v = match Self::convert_value_into_tp(v) {
                        Ok(tp) => tp,
                        Err(tp) => tp,
                    };
                    new_dict.insert(k, v);
                }
                Ok(TyParam::Dict(new_dict))
            }
            ValueObj::Set(set) => {
                let mut new_set = set! {};
                for v in set.into_iter() {
                    let tp = match Self::convert_value_into_tp(v) {
                        Ok(tp) => tp,
                        Err(tp) => tp,
                    };
                    new_set.insert(tp);
                }
                Ok(TyParam::Set(new_set))
            }
            ValueObj::Record(rec) => {
                let mut new_rec = dict! {};
                for (k, v) in rec.into_iter() {
                    let v = match Self::convert_value_into_tp(v) {
                        Ok(tp) => tp,
                        Err(tp) => tp,
                    };
                    new_rec.insert(k, v);
                }
                Ok(TyParam::Record(new_rec))
            }
            _ => Err(TyParam::Value(value)),
        }
    }

    fn _convert_type_to_dict_type(&self, ty: Type) -> Result<Dict<Type, Type>, ()> {
        match ty {
            Type::Poly { name, params } if &name[..] == "Dict" => {
                let dict = Dict::try_from(params[0].clone())?;
                let mut new_dict = dict! {};
                for (k, v) in dict.into_iter() {
                    let k = self.convert_tp_into_type(k).map_err(|_| ())?;
                    let v = self.convert_tp_into_type(v).map_err(|_| ())?;
                    new_dict.insert(k, v);
                }
                Ok(new_dict)
            }
            _ => Err(()),
        }
    }

    fn convert_type_to_array(&self, ty: Type) -> Result<Vec<ValueObj>, Type> {
        match ty {
            Type::Poly { name, params } if &name[..] == "Array" || &name[..] == "Array!" => {
                let Ok(t) = self.convert_tp_into_type(params[0].clone()) else {
                    return Err(poly(name, params));
                };
                let TyParam::Value(ValueObj::Nat(len)) = params[1] else { unreachable!() };
                Ok(vec![ValueObj::builtin_type(t); len as usize])
            }
            _ => Err(ty),
        }
    }

    pub(crate) fn convert_value_into_array(
        &self,
        val: ValueObj,
    ) -> Result<Vec<ValueObj>, ValueObj> {
        match val {
            ValueObj::Array(arr) => Ok(arr.to_vec()),
            ValueObj::Type(t) => self
                .convert_type_to_array(t.into_typ())
                .map_err(ValueObj::builtin_type),
            _ => Err(val),
        }
    }

    fn validate_and_project(
        &self,
        sub: &Type,
        opt_sup: Option<&Type>,
        rhs: &str,
        methods: &Context,
        level: usize,
        t_loc: &impl Locational,
    ) -> Option<Type> {
        // e.g. sub: Int, opt_sup: Add(?T), rhs: Output, methods: Int.methods
        //      sub: [Int; 4], opt_sup: Add([Int; 2]), rhs: Output, methods: [T; N].methods
        if let Ok(obj) = methods.get_const_local(&Token::symbol(rhs), &self.name) {
            #[allow(clippy::single_match)]
            // opt_sup: Add(?T), methods.impl_of(): Add(Int)
            // opt_sup: Add([Int; 2]), methods.impl_of(): Add([T; M])
            match (&opt_sup, methods.impl_of()) {
                (Some(sup), Some(trait_)) => {
                    if !self.supertype_of(&trait_, sup) {
                        return None;
                    }
                }
                _ => {}
            }
            // obj: Int|<: Add(Int)|.Output == ValueObj::Type(<type Int>)
            // obj: [T; N]|<: Add([T; M])|.Output == ValueObj::Type(<type [T; M+N]>)
            if let ValueObj::Type(quant_projected_t) = obj {
                let projected_t = quant_projected_t.into_typ();
                let (quant_sub, _) = self.get_type(&sub.qual_name()).unwrap();
                if let Some(sup) = opt_sup {
                    if let Some(quant_sup) = methods.impl_of() {
                        // T -> Int, M -> 2
                        self.substitute_typarams(&quant_sup, sup)
                            .map_err(|errs| {
                                Self::undo_substitute_typarams(&quant_sup);
                                errs
                            })
                            .ok()?;
                    }
                }
                // T -> Int, N -> 4
                self.substitute_typarams(quant_sub, sub)
                    .map_err(|errs| {
                        Self::undo_substitute_typarams(quant_sub);
                        errs
                    })
                    .ok()?;
                // [T; M+N] -> [Int; 4+2] -> [Int; 6]
                let res = self.eval_t_params(projected_t, level, t_loc).ok();
                if let Some(t) = res {
                    let mut tv_cache = TyVarCache::new(self.level, self);
                    let t = self.detach(t, &mut tv_cache);
                    // Int -> T, 2 -> M, 4 -> N
                    Self::undo_substitute_typarams(quant_sub);
                    if let Some(quant_sup) = methods.impl_of() {
                        Self::undo_substitute_typarams(&quant_sup);
                    }
                    return Some(t);
                }
                Self::undo_substitute_typarams(quant_sub);
                if let Some(quant_sup) = methods.impl_of() {
                    Self::undo_substitute_typarams(&quant_sup);
                }
            } else {
                log!(err "{obj}");
                if DEBUG_MODE {
                    todo!()
                }
            }
        }
        None
    }

    /// e.g.
    /// F((Int), 3) => F(Int, 3)
    /// F(?T, ?T) => F(?1, ?1)
    fn detach(&self, ty: Type, tv_cache: &mut TyVarCache) -> Type {
        match ty {
            Type::FreeVar(fv) if fv.is_linked() => self.detach(fv.crack().clone(), tv_cache),
            Type::FreeVar(fv) => {
                let new_fv = fv.detach();
                let name = new_fv.unbound_name().unwrap();
                if let Some(t) = tv_cache.get_tyvar(&name) {
                    t.clone()
                } else {
                    let tv = Type::FreeVar(new_fv);
                    let varname = VarName::from_str(name.clone());
                    tv_cache.push_or_init_tyvar(&varname, &tv, self);
                    tv
                }
            }
            Type::Poly { name, params } => {
                let mut new_params = vec![];
                for param in params {
                    new_params.push(self.detach_tp(param, tv_cache));
                }
                poly(name, new_params)
            }
            _ => ty,
        }
    }

    fn detach_tp(&self, tp: TyParam, tv_cache: &mut TyVarCache) -> TyParam {
        match tp {
            TyParam::FreeVar(fv) if fv.is_linked() => self.detach_tp(fv.crack().clone(), tv_cache),
            TyParam::FreeVar(fv) => {
                let new_fv = fv.detach();
                let name = new_fv.unbound_name().unwrap();
                if let Some(tp) = tv_cache.get_typaram(&name) {
                    tp.clone()
                } else {
                    let tp = TyParam::FreeVar(new_fv);
                    let varname = VarName::from_str(name.clone());
                    tv_cache.push_or_init_typaram(&varname, &tp, self);
                    tp
                }
            }
            TyParam::Type(t) => TyParam::t(self.detach(*t, tv_cache)),
            _ => tp,
        }
    }

    /// e.g.
    /// ```erg
    /// qt: Array(T, N), st: Array(Int, 3)
    /// ```
    /// invalid (no effect):
    /// ```erg
    /// qt: Iterable(T), st: Array(Int, 3)
    /// qt: Array(T, N), st: Array!(Int, 3) # TODO
    /// ```
    /// use `undo_substitute_typarams` after executing this method
    pub(crate) fn substitute_typarams(&self, qt: &Type, st: &Type) -> EvalResult<()> {
        let qtps = qt.typarams();
        let stps = st.typarams();
        if qt.qual_name() != st.qual_name() || qtps.len() != stps.len() {
            if let Some(sub) = st.get_sub() {
                return self.substitute_typarams(qt, &sub);
            }
            log!(err "{qt} / {st}");
            log!(err "[{}] [{}]", erg_common::fmt_vec(&qtps), erg_common::fmt_vec(&stps));
            return Ok(()); // TODO: e.g. Sub(Int) / Eq and Sub(?T)
        }
        for (qtp, stp) in qtps.into_iter().zip(stps.into_iter()) {
            self.substitute_typaram(qtp, stp)?;
        }
        Ok(())
    }

    pub(crate) fn overwrite_typarams(&self, qt: &Type, st: &Type) -> EvalResult<()> {
        let qtps = qt.typarams();
        let stps = st.typarams();
        if qt.qual_name() != st.qual_name() || qtps.len() != stps.len() {
            log!(err "{qt} / {st}");
            log!(err "[{}] [{}]", erg_common::fmt_vec(&qtps), erg_common::fmt_vec(&stps));
            return Ok(()); // TODO: e.g. Sub(Int) / Eq and Sub(?T)
        }
        for (qtp, stp) in qtps.into_iter().zip(stps.into_iter()) {
            self.overwrite_typaram(qtp, stp)?;
        }
        Ok(())
    }

    fn substitute_typaram(&self, qtp: TyParam, stp: TyParam) -> EvalResult<()> {
        match qtp {
            TyParam::FreeVar(ref fv) if fv.is_generalized() => {
                if !stp.is_unbound_var() || !stp.is_generalized() {
                    qtp.undoable_link(&stp);
                }
                if let Err(errs) = self.sub_unify_tp(&stp, &qtp, None, &(), false) {
                    log!(err "{errs}");
                }
                Ok(())
            }
            TyParam::Type(qt) => self.substitute_type(stp, *qt),
            TyParam::Value(ValueObj::Type(qt)) => self.substitute_type(stp, qt.into_typ()),
            _ => Ok(()),
        }
    }

    fn substitute_type(&self, stp: TyParam, qt: Type) -> EvalResult<()> {
        let st = self.convert_tp_into_type(stp).map_err(|tp| {
            EvalError::not_a_type_error(
                self.cfg.input.clone(),
                line!() as usize,
                ().loc(),
                self.caused_by(),
                &tp.to_string(),
            )
        })?;
        if qt.is_generalized() && qt.is_free_var() && (!st.is_unbound_var() || !st.is_generalized())
        {
            qt.undoable_link(&st);
        }
        if !st.is_unbound_var() || !st.is_generalized() {
            self.substitute_typarams(&qt, &st)?;
        }
        if st.has_no_unbound_var() && qt.has_no_unbound_var() {
            return Ok(());
        }
        if let Err(errs) = self.sub_unify(&st, &qt, &(), None) {
            log!(err "{errs}");
        }
        Ok(())
    }

    fn overwrite_typaram(&self, qtp: TyParam, stp: TyParam) -> EvalResult<()> {
        match qtp {
            TyParam::FreeVar(ref fv) if fv.is_undoable_linked() => {
                if !stp.is_unbound_var() || !stp.is_generalized() {
                    qtp.undoable_link(&stp);
                }
                if let Err(errs) = self.sub_unify_tp(&stp, &qtp, None, &(), false) {
                    log!(err "{errs}");
                }
                Ok(())
            }
            TyParam::Type(qt) => self.overwrite_type(stp, *qt),
            TyParam::Value(ValueObj::Type(qt)) => self.overwrite_type(stp, qt.into_typ()),
            _ => Ok(()),
        }
    }

    fn overwrite_type(&self, stp: TyParam, qt: Type) -> EvalResult<()> {
        let st = self.convert_tp_into_type(stp).map_err(|tp| {
            EvalError::not_a_type_error(
                self.cfg.input.clone(),
                line!() as usize,
                ().loc(),
                self.caused_by(),
                &tp.to_string(),
            )
        })?;
        if qt.has_undoable_linked_var() && (!st.is_unbound_var() || !st.is_generalized()) {
            qt.undoable_link(&st);
        }
        if !st.is_unbound_var() || !st.is_generalized() {
            self.overwrite_typarams(&qt, &st)?;
        }
        if let Err(errs) = self.sub_unify(&st, &qt, &(), None) {
            log!(err "{errs}");
        }
        Ok(())
    }

    pub(crate) fn undo_substitute_typarams(substituted_q: &Type) {
        for tp in substituted_q.typarams().into_iter() {
            match tp {
                TyParam::FreeVar(fv) if fv.is_undoable_linked() => fv.undo(),
                TyParam::Type(t) if t.is_free_var() => {
                    let Ok(subst) = <&FreeTyVar>::try_from(t.as_ref()) else { unreachable!() };
                    if subst.is_undoable_linked() {
                        subst.undo();
                    }
                }
                TyParam::Type(t) => {
                    Self::undo_substitute_typarams(&t);
                }
                TyParam::Value(ValueObj::Type(t)) => {
                    Self::undo_substitute_typarams(t.typ());
                }
                _ => {}
            }
        }
    }

    pub(crate) fn eval_proj_call(
        &self,
        lhs: TyParam,
        attr_name: Str,
        args: Vec<TyParam>,
        level: usize,
        t_loc: &impl Locational,
    ) -> EvalResult<Type> {
        let t = self.get_tp_t(&lhs)?;
        for ty_ctx in self.get_nominal_super_type_ctxs(&t).ok_or_else(|| {
            EvalError::type_not_found(
                self.cfg.input.clone(),
                line!() as usize,
                t_loc.loc(),
                self.caused_by(),
                &t,
            )
        })? {
            if let Ok(obj) = ty_ctx.get_const_local(&Token::symbol(&attr_name), &self.name) {
                if let ValueObj::Subr(subr) = obj {
                    let mut pos_args = vec![];
                    if subr.sig_t().is_method() {
                        match ValueObj::try_from(lhs) {
                            Ok(value) => {
                                pos_args.push(value);
                            }
                            Err(_) => {
                                return feature_error!(self, t_loc.loc(), "??");
                            }
                        }
                    }
                    for pos_arg in args.into_iter() {
                        match ValueObj::try_from(pos_arg) {
                            Ok(value) => {
                                pos_args.push(value);
                            }
                            Err(_) => {
                                return feature_error!(self, t_loc.loc(), "??");
                            }
                        }
                    }
                    let args = ValueArgs::new(pos_args, dict! {});
                    let t = self.call(subr, args, t_loc.loc())?;
                    let t = self
                        .convert_value_into_type(t)
                        .unwrap_or_else(|value| todo!("Type::try_from {value}"));
                    return Ok(t);
                } else {
                    return feature_error!(self, t_loc.loc(), "??");
                }
            }
            for (_class, methods) in ty_ctx.methods_list.iter() {
                if let Ok(obj) = methods.get_const_local(&Token::symbol(&attr_name), &self.name) {
                    if let ValueObj::Subr(subr) = obj {
                        let mut pos_args = vec![];
                        for pos_arg in args.into_iter() {
                            match ValueObj::try_from(pos_arg) {
                                Ok(value) => {
                                    pos_args.push(value);
                                }
                                Err(_) => {
                                    return feature_error!(self, t_loc.loc(), "??");
                                }
                            }
                        }
                        let args = ValueArgs::new(pos_args, dict! {});
                        let t = self.call(subr, args, t_loc.loc())?;
                        let t = self
                            .convert_value_into_type(t)
                            .unwrap_or_else(|value| todo!("Type::try_from {value}"));
                        return Ok(t);
                    } else {
                        return feature_error!(self, t_loc.loc(), "??");
                    }
                }
            }
        }
        if let TyParam::FreeVar(fv) = &lhs {
            if let Some((sub, sup)) = fv.get_subsup() {
                if self.is_trait(&sup) && !self.trait_impl_exists(&sub, &sup) {
                    // to prevent double error reporting
                    lhs.link(&TyParam::t(Never));
                    let sub = if cfg!(feature = "debug") {
                        sub
                    } else {
                        self.readable_type(sub)
                    };
                    let sup = if cfg!(feature = "debug") {
                        sup
                    } else {
                        self.readable_type(sup)
                    };
                    return Err(EvalErrors::from(EvalError::no_trait_impl_error(
                        self.cfg.input.clone(),
                        line!() as usize,
                        &sub,
                        &sup,
                        t_loc.loc(),
                        self.caused_by(),
                        self.get_simple_type_mismatch_hint(&sup, &sub),
                    )));
                }
            }
        }
        // if the target can't be found in the supertype, the type will be dereferenced.
        // In many cases, it is still better to determine the type variable than if the target is not found.
        let coerced = self.coerce_tp(lhs.clone(), t_loc)?;
        if lhs != coerced {
            let proj = proj_call(coerced, attr_name, args);
            self.eval_t_params(proj, level, t_loc)
                .map(|t| {
                    lhs.coerce();
                    t
                })
                .map_err(|(_, errs)| errs)
        } else {
            let proj = proj_call(lhs, attr_name, args);
            Err(EvalErrors::from(EvalError::no_candidate_error(
                self.cfg.input.clone(),
                line!() as usize,
                &proj,
                t_loc.loc(),
                self.caused_by(),
                self.get_no_candidate_hint(&proj),
            )))
        }
    }

    pub(crate) fn eval_pred(&self, p: Predicate) -> EvalResult<Predicate> {
        match p {
            Predicate::Value(_) | Predicate::Const(_) => Ok(p),
            Predicate::Equal { lhs, rhs } => Ok(Predicate::eq(lhs, self.eval_tp(rhs)?)),
            Predicate::NotEqual { lhs, rhs } => Ok(Predicate::ne(lhs, self.eval_tp(rhs)?)),
            Predicate::LessEqual { lhs, rhs } => Ok(Predicate::le(lhs, self.eval_tp(rhs)?)),
            Predicate::GreaterEqual { lhs, rhs } => Ok(Predicate::ge(lhs, self.eval_tp(rhs)?)),
            Predicate::And(l, r) => Ok(self.eval_pred(*l)? & self.eval_pred(*r)?),
            Predicate::Or(l, r) => Ok(self.eval_pred(*l)? | self.eval_pred(*r)?),
            Predicate::Not(pred) => Ok(!self.eval_pred(*pred)?),
        }
    }

    pub(crate) fn get_tp_t(&self, p: &TyParam) -> EvalResult<Type> {
        let p = self
            .eval_tp(p.clone())
            .map_err(|errs| {
                log!(err "{errs}");
                errs
            })
            .unwrap_or(p.clone());
        match p {
            TyParam::Value(v) => Ok(v_enum(set![v])),
            TyParam::Erased(t) => Ok((*t).clone()),
            TyParam::FreeVar(fv) if fv.is_linked() => self.get_tp_t(&fv.crack()),
            TyParam::FreeVar(fv) => {
                if let Some(t) = fv.get_type() {
                    Ok(t)
                } else {
                    feature_error!(self, Location::Unknown, "??")
                }
            }
            TyParam::Type(typ) => Ok(self.meta_type(&typ)),
            TyParam::Mono(name) => self
                .rec_get_const_obj(&name)
                .map(|v| v_enum(set![v.clone()]))
                .ok_or_else(|| {
                    EvalErrors::from(EvalError::unreachable(
                        self.cfg.input.clone(),
                        fn_name!(),
                        line!(),
                    ))
                }),
            TyParam::App { name, args } => self
                .rec_get_const_obj(&name)
                .and_then(|v| {
                    let ty = self.convert_value_into_type(v.clone()).ok()?;
                    let instance = self
                        .instantiate_def_type(&ty)
                        .map_err(|err| {
                            log!(err "{err}");
                            err
                        })
                        .ok()?;
                    for (param, arg) in instance.typarams().iter().zip(args.iter()) {
                        self.sub_unify_tp(arg, param, None, &(), false).ok()?;
                    }
                    let ty_obj = if self.is_class(&instance) {
                        ValueObj::builtin_class(instance)
                    } else if self.is_trait(&instance) {
                        ValueObj::builtin_trait(instance)
                    } else {
                        ValueObj::builtin_type(instance)
                    };
                    Some(v_enum(set![ty_obj]))
                })
                .ok_or_else(|| {
                    EvalErrors::from(EvalError::unreachable(
                        self.cfg.input.clone(),
                        fn_name!(),
                        line!(),
                    ))
                }),
            TyParam::Array(tps) => {
                let tp_t = if let Some(fst) = tps.get(0) {
                    self.get_tp_t(fst)?
                } else {
                    Never
                };
                let t = array_t(tp_t, TyParam::value(tps.len()));
                Ok(t)
            }
            TyParam::Tuple(tps) => {
                let mut tps_t = vec![];
                for tp in tps {
                    tps_t.push(self.get_tp_t(&tp)?);
                }
                Ok(tuple_t(tps_t))
            }
            TyParam::Set(tps) => {
                let len = TyParam::value(tps.len());
                let mut union = Type::Never;
                for tp in tps {
                    let tp_t = self.get_tp_t(&tp)?;
                    union = self.union(&union, &tp_t);
                }
                Ok(set_t(union, len))
            }
            dict @ TyParam::Dict(_) => Ok(dict_t(dict)),
            TyParam::BinOp { op, lhs, rhs } => match op {
                OpKind::Or | OpKind::And => {
                    let lhs = self.get_tp_t(&lhs)?;
                    let rhs = self.get_tp_t(&rhs)?;
                    if self.subtype_of(&lhs, &Type::Bool) && self.subtype_of(&rhs, &Type::Bool) {
                        Ok(Type::Bool)
                    } else if self.subtype_of(&lhs, &Type::Type)
                        && self.subtype_of(&rhs, &Type::Type)
                    {
                        Ok(Type::Type)
                    } else {
                        let op_name = op_to_name(op);
                        feature_error!(
                            self,
                            Location::Unknown,
                            &format!("get type: {op_name}({lhs}, {rhs})")
                        )
                    }
                }
                _ => {
                    let op_name = op_to_name(op);
                    feature_error!(
                        self,
                        Location::Unknown,
                        &format!("get type: {op_name}({lhs}, {rhs})")
                    )
                }
            },
            other => feature_error!(
                self,
                Location::Unknown,
                &format!("getting the type of {other}")
            ),
        }
    }

    pub(crate) fn _get_tp_class(&self, p: &TyParam) -> EvalResult<Type> {
        let p = self.eval_tp(p.clone())?;
        match p {
            TyParam::Value(v) => Ok(v.class()),
            TyParam::Erased(t) => Ok((*t).clone()),
            TyParam::FreeVar(fv) => {
                if let Some(t) = fv.get_type() {
                    Ok(t)
                } else {
                    feature_error!(self, Location::Unknown, "??")
                }
            }
            TyParam::Type(_) => Ok(Type::Type),
            TyParam::Mono(name) => {
                self.rec_get_const_obj(&name)
                    .map(|v| v.class())
                    .ok_or_else(|| {
                        EvalErrors::from(EvalError::unreachable(
                            self.cfg.input.clone(),
                            fn_name!(),
                            line!(),
                        ))
                    })
            }
            other => feature_error!(
                self,
                Location::Unknown,
                &format!("getting the class of {other}")
            ),
        }
    }

    /// NOTE: If l and r are types, the Context is used to determine the type.
    /// NOTE: lとrが型の場合はContextの方で判定する
    pub(crate) fn shallow_eq_tp(&self, lhs: &TyParam, rhs: &TyParam) -> bool {
        match (lhs, rhs) {
            (TyParam::Type(l), _) if l.is_unbound_var() => {
                self.subtype_of(&self.get_tp_t(rhs).unwrap(), &Type::Type)
            }
            (_, TyParam::Type(r)) if r.is_unbound_var() => {
                let lhs = self.get_tp_t(lhs).unwrap();
                self.subtype_of(&lhs, &Type::Type)
            }
            (TyParam::Type(l), TyParam::Type(r)) => l == r,
            (TyParam::Value(l), TyParam::Value(r)) => l == r,
            (TyParam::Erased(l), TyParam::Erased(r)) => l == r,
            (TyParam::Array(l), TyParam::Array(r)) => l == r,
            (TyParam::Tuple(l), TyParam::Tuple(r)) => l == r,
            (TyParam::Set(l), TyParam::Set(r)) => l == r, // FIXME:
            (TyParam::Dict(l), TyParam::Dict(r)) => l == r,
            (TyParam::FreeVar { .. }, TyParam::FreeVar { .. }) => true,
            (TyParam::Mono(l), TyParam::Mono(r)) => {
                if l == r {
                    true
                } else if let (Some(l), Some(r)) =
                    (self.rec_get_const_obj(l), self.rec_get_const_obj(r))
                {
                    l == r
                } else {
                    // lとrが型の場合は...
                    false
                }
            }
            (TyParam::Mono(m), TyParam::Value(l)) | (TyParam::Value(l), TyParam::Mono(m)) => {
                if let Some(o) = self.rec_get_const_obj(m) {
                    o == l
                } else {
                    true
                }
            }
            (TyParam::Erased(t), _) => t.as_ref() == &self.get_tp_t(rhs).unwrap(),
            (_, TyParam::Erased(t)) => t.as_ref() == &self.get_tp_t(lhs).unwrap(),
            (TyParam::Value(v), _) => {
                if let Ok(tp) = Self::convert_value_into_tp(v.clone()) {
                    self.shallow_eq_tp(&tp, rhs)
                } else {
                    false
                }
            }
            (_, TyParam::Value(v)) => {
                if let Ok(tp) = Self::convert_value_into_tp(v.clone()) {
                    self.shallow_eq_tp(lhs, &tp)
                } else {
                    false
                }
            }
            (l, r) => {
                log!(err "l: {l}, r: {r}");
                l == r
            }
        }
    }
}

//! Everything related to types in our intermediate representation.

use super::comp::CompInfo;
use super::enum_ty::Enum;
use super::function::FunctionSig;
use super::item::{Item, ItemId};
use super::int::IntKind;
use super::layout::Layout;
use super::context::BindgenContext;
use parse::{ClangItemParser, ParseResult, ParseError};
use clang::{self, Cursor};

/// The base representation of a type in bindgen.
///
/// A type has an optional name, which if present cannot be empty, a `layout`
/// (size, alignment and packedness) if known, a `Kind`, which determines which
/// kind of type it is, and whether the type is const.
#[derive(Debug)]
pub struct Type {
    /// The name of the type, or None if it was an unnamed struct or union.
    name: Option<String>,
    /// The layout of the type, if known.
    layout: Option<Layout>,
    /// The inner kind of the type
    kind: TypeKind,
    /// Whether this type is const-qualified.
    is_const: bool,
}

/// The maximum number of items in an array for which Rust implements common
/// traits, and so if we have a type containing an array with more than this
/// many items, we won't be able to derive common traits on that type.
///
/// We need type-level integers yesterday :'(
pub const RUST_DERIVE_IN_ARRAY_LIMIT: usize = 32;

impl Type {
    /// Get the underlying `CompInfo` for this type, or `None` if this is some
    /// other kind of type.
    pub fn as_comp(&self) -> Option<&CompInfo> {
        match self.kind {
            TypeKind::Comp(ref ci) => Some(ci),
            _ => None,
        }
    }

    /// Construct a new `Type`.
    pub fn new(name: Option<String>,
               layout: Option<Layout>,
               kind: TypeKind,
               is_const: bool) -> Self {
        Type {
            name: name,
            layout: layout,
            kind: kind,
            is_const: is_const,
        }
    }

    /// Which kind of type is this?
    pub fn kind(&self) -> &TypeKind {
        &self.kind
    }

    /// Get a mutable reference to this type's kind.
    pub fn kind_mut(&mut self) -> &mut TypeKind {
        &mut self.kind
    }

    /// Get this type's name.
    pub fn name(&self) -> Option<&str> {
        self.name.as_ref().map(|name| &**name)
    }

    /// Is this a compound type?
    pub fn is_comp(&self) -> bool {
        match self.kind {
            TypeKind::Comp(..) => true,
            _ => false,
        }
    }

    /// Is this a named type?
    pub fn is_named(&self) -> bool {
        match self.kind {
            TypeKind::Named(..) => true,
            _ => false,
        }
    }

    /// Is this a function type?
    pub fn is_function(&self) -> bool {
        match self.kind {
            TypeKind::Function(..) => true,
            _ => false,
        }
    }

    /// Is this either a builtin or named type?
    pub fn is_builtin_or_named(&self) -> bool {
        match self.kind {
            TypeKind::Void |
            TypeKind::NullPtr |
            TypeKind::Function(..) |
            TypeKind::Array(..) |
            TypeKind::Reference(..) |
            TypeKind::Pointer(..) |
            TypeKind::BlockPointer |
            TypeKind::Int(..) |
            TypeKind::Float(..) |
            TypeKind::Named(..) => true,
            _ => false,
        }
    }

    /// Creates a new named type, with name `name`.
    pub fn named(name: String, default: Option<ItemId>) -> Self {
        assert!(!name.is_empty());
        // TODO: stop duplicating the name, it's stupid.
        let kind = TypeKind::Named(name.clone(), default);
        Self::new(Some(name), None, kind, false)
    }

    /// Is this an integer type?
    pub fn is_integer(&self, ctx: &BindgenContext) -> bool {
        match self.kind {
            TypeKind::UnresolvedTypeRef(..) => false,
            _ => match self.canonical_type(ctx).kind {
                TypeKind::Int(..) => true,
                _ => false,
            }
        }
    }

    /// Is this a `const` qualified type?
    pub fn is_const(&self) -> bool {
        self.is_const
    }

    /// What is the layout of this type?
    pub fn layout(&self, ctx: &BindgenContext) -> Option<Layout> {
        use std::mem;

        self.layout.or_else(|| {
            match self.kind {
                TypeKind::Comp(ref ci)
                    => ci.layout(ctx),
                // FIXME(emilio): This is a hack for anonymous union templates.
                // Use the actual pointer size!
                TypeKind::Pointer(..) |
                TypeKind::BlockPointer
                    => Some(Layout::new(mem::size_of::<*mut ()>(), mem::align_of::<*mut ()>())),
                TypeKind::ResolvedTypeRef(inner)
                    => ctx.resolve_type(inner).layout(ctx),
                _ => None,
            }
        })
    }

    /// Wether we can derive rust's `Debug` annotation in Rust. This should
    /// ideally be a no-op that just returns `true`, but instead needs to be a
    /// recursive method that checks whether all the proper members can derive
    /// debug or not, because of the limit rust has on 32 items as max in the
    /// array.
    pub fn can_derive_debug(&self, ctx: &BindgenContext) -> bool {
        match self.kind {
            TypeKind::Array(t, len) => {
                len <= RUST_DERIVE_IN_ARRAY_LIMIT &&
                ctx.resolve_type(t).can_derive_debug(ctx)
            }
            TypeKind::ResolvedTypeRef(t) |
            TypeKind::TemplateAlias(t, _) |
            TypeKind::Alias(_, t) => {
                ctx.resolve_type(t).can_derive_debug(ctx)
            }
            TypeKind::Comp(ref info) => {
                info.can_derive_debug(ctx, self.layout(ctx))
            }
            _ => true,
        }
    }

    /// For some reason, deriving copies of an array of a type that is not known
    /// to be copy is a compile error. e.g.:
    ///
    /// ```rust
    /// #[derive(Copy, Clone)]
    /// struct A<T> {
    ///     member: T,
    /// }
    /// ```
    ///
    /// is fine, while:
    ///
    /// ```rust,ignore
    /// #[derive(Copy, Clone)]
    /// struct A<T> {
    ///     member: [T; 1],
    /// }
    /// ```
    ///
    /// is an error.
    ///
    /// That's the whole point of the existence of `can_derive_copy_in_array`.
    pub fn can_derive_copy_in_array(&self, ctx: &BindgenContext, item: &Item) -> bool {
        match self.kind {
            TypeKind::ResolvedTypeRef(t) |
            TypeKind::TemplateAlias(t, _) |
            TypeKind::Alias(_, t) |
            TypeKind::Array(t, _) => {
                ctx.resolve_item(t)
                             .can_derive_copy_in_array(ctx)
            }
            TypeKind::Named(..) => false,
            _ => self.can_derive_copy(ctx, item),
        }
    }

    /// Wether we'd be able to derive the `Copy` trait in Rust or not. Same
    /// rationale than `can_derive_debug`.
    pub fn can_derive_copy(&self, ctx: &BindgenContext, item: &Item) -> bool {
        match self.kind {
            TypeKind::Array(t, len) => {
                len <= RUST_DERIVE_IN_ARRAY_LIMIT &&
                ctx.resolve_item(t).can_derive_copy_in_array(ctx)
            }
            TypeKind::ResolvedTypeRef(t) |
            TypeKind::TemplateAlias(t, _) |
            TypeKind::TemplateRef(t, _) |
            TypeKind::Alias(_, t) => {
                ctx.resolve_item(t).can_derive_copy(ctx)
            }
            TypeKind::Comp(ref info) => {
                info.can_derive_copy(ctx, item)
            }
            _ => true,
        }
    }

    /// Whether this type has a vtable.
    pub fn has_vtable(&self, ctx: &BindgenContext) -> bool {
        // FIXME: Can we do something about template parameters? Huh...
        match self.kind {
            TypeKind::TemplateRef(t, _) |
            TypeKind::TemplateAlias(t, _) |
            TypeKind::Alias(_, t) |
            TypeKind::ResolvedTypeRef(t) => {
                ctx.resolve_type(t).has_vtable(ctx)
            }
            TypeKind::Comp(ref info) => {
                info.has_vtable(ctx)
            }
            _ => false,
        }

    }

    /// Returns whether this type has a destructor.
    pub fn has_destructor(&self, ctx: &BindgenContext) -> bool {
        match self.kind {
            TypeKind::TemplateRef(t, _) |
            TypeKind::TemplateAlias(t, _) |
            TypeKind::Alias(_, t) |
            TypeKind::ResolvedTypeRef(t) => {
                ctx.resolve_type(t).has_destructor(ctx)
            }
            TypeKind::Comp(ref info) => {
                info.has_destructor(ctx)
            }
            _ => false,
        }
    }

    /// See the comment in `Item::signature_contains_named_type`.
    pub fn signature_contains_named_type(&self,
                                         ctx: &BindgenContext,
                                         ty: &Type) -> bool {
        debug_assert!(ty.is_named());
        let name = match *ty.kind() {
            TypeKind::Named(ref name, _) => name,
            _ => unreachable!(),
        };

        match self.kind {
            TypeKind::Named(ref this_name, _)
                => this_name == name,
            TypeKind::ResolvedTypeRef(t) |
            TypeKind::Array(t, _) |
            TypeKind::Pointer(t) |
            TypeKind::Alias(_, t)
                => ctx.resolve_type(t)
                                .signature_contains_named_type(ctx, ty),
            TypeKind::Function(ref sig) => {
                sig.argument_types().iter().any(|&(_, arg)| {
                    ctx.resolve_type(arg)
                                 .signature_contains_named_type(ctx, ty)
                }) ||
                ctx.resolve_type(sig.return_type())
                             .signature_contains_named_type(ctx, ty)
            },
            TypeKind::TemplateAlias(_, ref template_args) |
            TypeKind::TemplateRef(_, ref template_args) => {
                template_args.iter().any(|arg| {
                    ctx.resolve_type(*arg)
                                 .signature_contains_named_type(ctx, ty)
                })
            }
            TypeKind::Comp(ref ci)
                => ci.signature_contains_named_type(ctx, ty),
            _   => false,
        }
    }

    /// Returns the canonical type of this type, that is, the "inner type".
    ///
    /// For example, for a `typedef`, the canonical type would be the
    /// `typedef`ed type, for a template specialization, would be the template
    /// its specializing, and so on.
    pub fn canonical_type<'tr>(&'tr self, ctx: &'tr BindgenContext) -> &'tr Type {
        match self.kind {
            TypeKind::Named(..) |
            TypeKind::Array(..) |
            TypeKind::Comp(..) |
            TypeKind::Int(..) |
            TypeKind::Float(..) |
            TypeKind::Function(..) |
            TypeKind::Enum(..) |
            TypeKind::Reference(..) |
            TypeKind::Void |
            TypeKind::NullPtr |
            TypeKind::BlockPointer |
            TypeKind::Pointer(..) => self,

            TypeKind::ResolvedTypeRef(inner) |
            TypeKind::Alias(_, inner) |
            TypeKind::TemplateAlias(inner, _) |
            TypeKind::TemplateRef(inner, _)
                => ctx.resolve_type(inner).canonical_type(ctx),

            TypeKind::UnresolvedTypeRef(..)
                => unreachable!("Should have been resolved after parsing!"),
        }
    }
}

/// The kind of float this type represents.
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum FloatKind {
    /// A `float`.
    Float,
    /// A `double`.
    Double,
    /// A `long double`.
    LongDouble,
}

/// The different kinds of types that we can parse.
#[derive(Debug)]
pub enum TypeKind {
    /// The void type.
    Void,

    /// The `nullptr_t` type.
    NullPtr,

    /// A compound type, that is, a class, struct, or union.
    Comp(CompInfo),

    /// An integer type, of a given kind. `bool` and `char` are also considered
    /// integers.
    Int(IntKind),

    /// A floating point type.
    Float(FloatKind),

    /// A type alias, with a name, that points to another type.
    Alias(String, ItemId),

    /// A templated alias, pointing to an inner `Alias` type, with template
    /// parameters.
    TemplateAlias(ItemId, Vec<ItemId>),

    /// An array of a type and a lenght.
    Array(ItemId, usize),

    /// A function type, with a given signature.
    Function(FunctionSig),

    /// An `enum` type.
    Enum(Enum),

    /// A pointer to a type. The bool field represents whether it's const or
    /// not.
    Pointer(ItemId),

    /// A pointer to an Apple block.
    BlockPointer,

    /// A reference to a type, as in: int& foo().
    Reference(ItemId),

    /// A reference to a template, with different template parameter names. To
    /// see why this is needed, check out the creation of this variant in
    /// `Type::from_clang_ty`.
    TemplateRef(ItemId, Vec<ItemId>),

    /// A reference to a yet-to-resolve type. This stores the clang cursor
    /// itself, and postpones its resolution.
    ///
    /// These are gone in a phase after parsing where these are mapped to
    /// already known types, and are converted to ResolvedTypeRef.
    ///
    /// see tests/headers/typeref.hpp to see somewhere where this is a problem.
    UnresolvedTypeRef(clang::Type, Option<clang::Cursor>, /* parent_id */ Option<ItemId>),

    /// An indirection to another type.
    ///
    /// These are generated after we resolve a forward declaration, or when we
    /// replace one type with another.
    ResolvedTypeRef(ItemId),

    /// A named type, that is, a template parameter, with an optional default
    /// type.
    Named(String, Option<ItemId>),
}

impl Type {
    /// Whether this type is unsized, that is, has no members. This is used to
    /// derive whether we should generate a dummy `_address` field for structs,
    /// to comply to the C and C++ layouts, that specify that every type needs
    /// to be addressable.
    pub fn is_unsized(&self, ctx: &BindgenContext) -> bool {
        debug_assert!(ctx.in_codegen_phase(), "Not yet");

        match self.kind {
            TypeKind::Void => true,
            TypeKind::Comp(ref ci) => ci.is_unsized(ctx),
            TypeKind::Array(inner, size) => {
                size == 0 ||
                ctx.resolve_type(inner).is_unsized(ctx)
            }
            TypeKind::ResolvedTypeRef(inner) |
            TypeKind::Alias(_, inner) |
            TypeKind::TemplateAlias(inner, _) |
            TypeKind::TemplateRef(inner, _)
                => ctx.resolve_type(inner).is_unsized(ctx),
            TypeKind::Named(..) |
            TypeKind::Int(..) |
            TypeKind::Float(..) |
            TypeKind::Function(..) |
            TypeKind::Enum(..) |
            TypeKind::Reference(..) |
            TypeKind::NullPtr |
            TypeKind::BlockPointer |
            TypeKind::Pointer(..) => false,

            TypeKind::UnresolvedTypeRef(..)
                => unreachable!("Should have been resolved after parsing!"),
        }
    }

    /// This is another of the nasty methods. This one is the one that takes
    /// care of the core logic of converting a clang type to a `Type`.
    ///
    /// It's sort of nasty and full of special-casing, but hopefully the
    /// comments in every special case justify why they're there.
    pub fn from_clang_ty(potential_id: ItemId,
                         ty: &clang::Type,
                         location: Option<Cursor>,
                         parent_id: Option<ItemId>,
                         ctx: &mut BindgenContext) -> Result<ParseResult<Self>, ParseError> {
        use clangll::*;
        if let Some(ty) = ctx.builtin_or_resolved_ty(parent_id, ty, location) {
            debug!("{:?} already resolved: {:?}", ty, location);
            return Ok(ParseResult::AlreadyResolved(ty));
        }

        let layout = ty.fallible_layout().ok();
        let cursor = ty.declaration();
        let mut name = cursor.spelling();

        debug!("from_clang_ty: {:?}, ty: {:?}, loc: {:?}", potential_id, ty, location);
        debug!("currently_parsed_types: {:?}", ctx.currently_parsed_types);

        let canonical_ty = ty.canonical_type();
        let kind = match ty.kind() {
            CXType_Unexposed if *ty != canonical_ty &&
                                canonical_ty.kind() != CXType_Invalid => {
                debug!("Looking for canonical type: {:?}", canonical_ty);
                return Self::from_clang_ty(potential_id, &canonical_ty,
                                           location, parent_id, ctx);
            }
            CXType_Unexposed |
            CXType_Invalid => {
                // For some reason Clang doesn't give us any hint
                // in some situations where we should generate a
                // function pointer (see
                // tests/headers/func_ptr_in_struct.h), so we do a
                // guess here trying to see if it has a valid return
                // type.
                if ty.ret_type().is_some() {
                    let signature =
                        try!(FunctionSig::from_ty(ty, &location.unwrap_or(cursor), ctx));
                    TypeKind::Function(signature)
                // Same here, with template specialisations we can safely assume
                // this is a Comp(..)
                } else if ty.num_template_args() > 0 {
                    debug!("Template specialization: {:?}", ty);
                    let complex =
                        CompInfo::from_ty(potential_id, ty, location, ctx)
                                .expect("C'mon");
                    TypeKind::Comp(complex)
                } else if let Some(location) = location {
                    match location.kind() {
                        CXCursor_ClassTemplatePartialSpecialization |
                        CXCursor_ClassTemplate => {
                            name = location.spelling();
                            let complex =
                                CompInfo::from_ty(potential_id, ty, Some(location), ctx)
                                        .expect("C'mon");
                            TypeKind::Comp(complex)
                        }
                        CXCursor_TypeAliasTemplateDecl => {
                            debug!("TypeAliasTemplateDecl");

                            // We need to manually unwind this one.
                            let mut inner = Err(ParseError::Continue);
                            let mut args = vec![];

                            location.visit(|cur, _| {
                                match cur.kind() {
                                    CXCursor_TypeAliasDecl => {
                                        debug_assert!(cur.cur_type().kind() == CXType_Typedef);
                                        inner = Item::from_ty(&cur.cur_type(),
                                                              Some(*cur),
                                                              Some(potential_id),
                                                              ctx);
                                    }
                                    CXCursor_TemplateTypeParameter => {
                                        // See the comment in src/ir/comp.rs
                                        // about the same situation.
                                        if cur.spelling().is_empty() {
                                            return CXChildVisit_Continue;
                                        }

                                        let default_type =
                                            Item::from_ty(&cur.cur_type(),
                                                          Some(*cur),
                                                          Some(potential_id),
                                                          ctx).ok();
                                        let param =
                                            Item::named_type(cur.spelling(),
                                                             default_type,
                                                             potential_id, ctx);
                                        args.push(param);
                                    }
                                    _ => {}
                                }
                                CXChildVisit_Continue
                            });

                            if inner.is_err() {
                                error!("Failed to parse templated type alias {:?}", location);
                                return Err(ParseError::Continue);
                            }

                            if args.is_empty() {
                                error!("Failed to get any template parameter, maybe a specialization? {:?}", location);
                                return Err(ParseError::Continue);
                            }

                            TypeKind::TemplateAlias(inner.unwrap(), args)
                        }
                        CXCursor_TemplateRef => {
                            let referenced = location.referenced();
                            return Self::from_clang_ty(potential_id,
                                                       &referenced.cur_type(),
                                                       Some(referenced.cur_type().declaration()),
                                                       parent_id,
                                                       ctx);
                        }
                        CXCursor_TypeRef => {
                            let referenced = location.referenced();
                            return Ok(ParseResult::AlreadyResolved(
                                    Item::from_ty_or_ref_with_id(potential_id,
                                                                 referenced.cur_type(),
                                                                 Some(referenced.cur_type().declaration()),
                                                                 parent_id,
                                                                 ctx)));
                        }
                        _ => {
                            if ty.kind() == CXType_Unexposed {
                                warn!("Unexposed type {:?}, recursing inside, loc: {:?}", ty, location);
                                return Err(ParseError::Recurse);
                            }

                            error!("invalid type {:?}", ty);
                            return Err(ParseError::Continue);
                        }
                    }
                } else {
                    // TODO: Don't duplicate this!
                    if ty.kind() == CXType_Unexposed {
                        warn!("Unexposed type {:?}, recursing inside", ty);
                        return Err(ParseError::Recurse);
                    }

                    error!("invalid type `{}`", ty.spelling());
                    return Err(ParseError::Continue);
                }
            }
            // NOTE: We don't resolve pointers eagerly because the pointee type
            // might not have been parsed, and if it contains templates or
            // something else we might get confused, see the comment inside
            // TypeRef.
            //
            // We might need to, though, if the context is already in the
            // process of resolving them.
            CXType_MemberPointer |
            CXType_Pointer => {
                let inner =
                    Item::from_ty_or_ref(ty.pointee_type(), location,
                                         parent_id, ctx);
                TypeKind::Pointer(inner)
            }
            CXType_BlockPointer => {
                TypeKind::BlockPointer
            }
            // XXX: RValueReference is most likely wrong, but I don't think we
            // can even add bindings for that, so huh.
            CXType_RValueReference |
            CXType_LValueReference => {
                let inner =
                    Item::from_ty_or_ref(ty.pointee_type(), location,
                                         parent_id, ctx);
                TypeKind::Reference(inner)
            }
            // XXX DependentSizedArray is wrong
            CXType_VariableArray |
            CXType_DependentSizedArray |
            CXType_IncompleteArray => {
                let inner = Item::from_ty(&ty.elem_type(), location, parent_id, ctx)
                                .expect("Not able to resolve array element?");
                TypeKind::Pointer(inner)
            }
            CXType_FunctionNoProto |
            CXType_FunctionProto => {
                let signature = try!(FunctionSig::from_ty(ty, &location.unwrap_or(cursor), ctx));
                TypeKind::Function(signature)
            }
            CXType_Typedef => {
                let inner = cursor.typedef_type();
                let inner =
                    Item::from_ty_or_ref(inner, location, parent_id, ctx);
                TypeKind::Alias(ty.spelling(), inner)
            }
            CXType_Enum => {
                let enum_ = Enum::from_ty(ty, ctx)
                                .expect("Not an enum?");
                TypeKind::Enum(enum_)
            }
            CXType_Record => {
                let complex = CompInfo::from_ty(potential_id, ty, location, ctx)
                                    .expect("Not a complex type?");
                TypeKind::Comp(complex)
            }
            // FIXME: We stub vectors as arrays since in 99% of the cases the
            // layout is going to be correct, and there's no way we can generate
            // vector types properly in Rust for now.
            //
            // That being said, that should be fixed eventually.
            CXType_Vector |
            CXType_ConstantArray => {
                let inner = Item::from_ty(&ty.elem_type(), location, parent_id, ctx)
                                .expect("Not able to resolve array element?");
                TypeKind::Array(inner, ty.num_elements())
            }
            // A complex number is always a real and an imaginary part, so
            // represent that as a two-item array.
            CXType_Complex => {
                let inner = Item::from_ty(&ty.elem_type(), location, parent_id, ctx)
                                .expect("Not able to resolve array element?");
                TypeKind::Array(inner, 2)
            }
            #[cfg(not(feature="llvm_stable"))]
            CXType_Elaborated => {
                return Self::from_clang_ty(potential_id, &ty.named(),
                                           location, parent_id, ctx);
            }
            _ => {
                error!("unsupported type {:?} at {:?}", ty, location);
                return Err(ParseError::Continue);
            }
        };

        let name = if name.is_empty() { None } else { Some(name) };
        let is_const = ty.is_const();

        let ty = Type::new(name, layout, kind, is_const);
        // TODO: maybe declaration.canonical()?
        Ok(ParseResult::New(ty, Some(cursor.canonical())))
    }
}

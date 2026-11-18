//! Attribute macros that expose Rust types and methods to Vali Scheme
//! (ADR 0016 #5). Use them through `vali` or `vali_runtime`, which
//! re-export them; the code they generate names whichever of the two the
//! crate using them depends on, `vali_runtime` first.
//!
//! - `#[object]` on a struct or enum makes it a `RustObject`, named in
//!   kebab case (`StringPort` is `string-port`) unless `name = "..."` says.
//! - `#[bridge]` on an inherent `impl` block adds an associated function,
//!   `bridge`, that adds the block's functions and methods to a
//!   `NativeLibrary`, each named in kebab case unless a `#[scheme(name =
//!   "...")]` on it says. Parameters may borrow: `&str`, `&[u8]`, and `&T`
//!   or `&mut T` for a Rust object type, lent for the call only; the rest
//!   convert through `Param`. `#[scheme(skip)]` leaves a function out, and
//!   `#[scheme(expansion_safe, version = N)]` marks it expansion-safe
//!   (ADR 0009 #15). `#[bridge(register = name)]` names the generated
//!   function, for a type with several bridged blocks.
//!
//! ```ignore
//! #[vali_runtime::object]
//! struct Counter { n: i64 }
//!
//! #[vali_runtime::bridge]
//! impl Counter {
//!     #[scheme(name = "make-counter")]
//!     fn new(start: i64) -> Counter { Counter { n: start } }
//!     #[scheme(name = "counter-add!")]
//!     fn add(&mut self, by: Option<i64>) -> i64 { self.n += by.unwrap_or(1); self.n }
//! }
//!
//! let library = Counter::bridge(NativeLibrary::new(&["counters"]));
//! ```

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as Tokens};
use quote::{format_ident, quote};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{
    Error, Expr, FnArg, ImplItem, Item, ItemImpl, Lit, Meta, Pat, ReturnType, Token, Type,
    TypeReference,
};

/// `#[object]` or `#[object(name = "...")]`: makes a struct or enum a
/// `RustObject`.
#[proc_macro_attribute]
pub fn object(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_object(attr.into(), item.into())
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// `#[bridge]` or `#[bridge(register = name)]`: exposes an inherent `impl`
/// block's functions and methods through a generated `bridge` function.
#[proc_macro_attribute]
pub fn bridge(attr: TokenStream, item: TokenStream) -> TokenStream {
    expand_bridge(attr.into(), item.into())
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

/// The path of the runtime in the crate the macro is used in: the runtime
/// the `vali` facade re-exports, if the crate depends on `vali` under
/// whatever name, or else `vali_runtime`. A crate depending on neither
/// gets `::vali_runtime`, and the compiler's error names it.
fn runtime() -> Tokens {
    use proc_macro_crate::{FoundCrate, crate_name};
    let named = |name: &str| {
        let ident = syn::Ident::new(name, Span::call_site());
        quote!(::#ident)
    };
    // A package's integration tests and examples find it by `Name`;
    // `Itself` is its library alone.
    match crate_name("vali") {
        Ok(FoundCrate::Itself) => quote!(crate::__runtime),
        Ok(FoundCrate::Name(name)) => {
            let vali = named(&name);
            quote!(#vali::__runtime)
        }
        Err(_) => match crate_name("vali-runtime") {
            Ok(FoundCrate::Itself) => quote!(crate),
            Ok(FoundCrate::Name(name)) => named(&name),
            Err(_) => named("vali_runtime"),
        },
    }
}

/// `name` in kebab case: `StringPort` and `string_port` are both
/// `string-port`.
fn kebab(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 && !out.ends_with('-') {
                out.push('-');
            }
            out.extend(c.to_lowercase());
        } else if c == '_' {
            out.push('-');
        } else {
            out.push(c);
        }
    }
    out
}

fn string_value(expr: &Expr) -> syn::Result<String> {
    match expr {
        Expr::Lit(lit) => match &lit.lit {
            Lit::Str(s) => Ok(s.value()),
            other => Err(Error::new(other.span(), "expected a string")),
        },
        other => Err(Error::new(other.span(), "expected a string")),
    }
}

fn expand_object(attr: Tokens, item: Tokens) -> syn::Result<Tokens> {
    let rt = runtime();
    let item: Item = syn::parse2(item)?;
    let (ident, generics) = match &item {
        Item::Struct(s) => (&s.ident, &s.generics),
        Item::Enum(e) => (&e.ident, &e.generics),
        other => {
            return Err(Error::new(
                other.span(),
                "#[object] goes on a struct or an enum",
            ));
        }
    };
    if !generics.params.is_empty() {
        return Err(Error::new(
            generics.span(),
            "a Rust object type cannot be generic",
        ));
    }
    let mut name = kebab(&ident.to_string());
    for meta in Punctuated::<Meta, Token![,]>::parse_terminated.parse2(attr)? {
        match meta {
            Meta::NameValue(nv) if nv.path.is_ident("name") => name = string_value(&nv.value)?,
            other => return Err(Error::new(other.span(), "expected `name = \"...\"`")),
        }
    }
    Ok(quote! {
        #item
        impl #rt::RustObject for #ident {
            const NAME: &'static str = #name;
        }
    })
}

/// How one parameter converts.
enum Param {
    /// `&str`, from a string.
    Text,
    /// `&[u8]`, from a bytevector.
    Bytes,
    /// `&T` or `&mut T`, a Rust object borrowed for the call.
    Object { ty: Type, mutable: bool },
    /// Anything else, through `Param`.
    Owned(Type),
}

fn classify(ty: &Type) -> Param {
    if let Type::Reference(TypeReference {
        elem, mutability, ..
    }) = ty
    {
        match &**elem {
            Type::Path(p) if p.path.is_ident("str") && mutability.is_none() => return Param::Text,
            Type::Slice(s) if mutability.is_none() => {
                if let Type::Path(p) = &*s.elem
                    && p.path.is_ident("u8")
                {
                    return Param::Bytes;
                }
            }
            elem => {
                return Param::Object {
                    ty: elem.clone(),
                    mutable: mutability.is_some(),
                };
            }
        }
    }
    Param::Owned(ty.clone())
}

/// A function's `#[scheme(...)]` options.
#[derive(Default)]
struct Options {
    name: Option<String>,
    skip: bool,
    expansion_safe: bool,
    version: Option<u32>,
}

fn options(attrs: &mut Vec<syn::Attribute>) -> syn::Result<Options> {
    let mut options = Options::default();
    let mut kept = Vec::new();
    for attr in attrs.drain(..) {
        if !attr.path().is_ident("scheme") {
            kept.push(attr);
            continue;
        }
        let metas = attr.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;
        for meta in metas {
            match meta {
                Meta::Path(p) if p.is_ident("skip") => options.skip = true,
                Meta::Path(p) if p.is_ident("expansion_safe") => options.expansion_safe = true,
                Meta::NameValue(nv) if nv.path.is_ident("name") => {
                    options.name = Some(string_value(&nv.value)?);
                }
                Meta::NameValue(nv) if nv.path.is_ident("version") => match &nv.value {
                    Expr::Lit(lit) => match &lit.lit {
                        Lit::Int(n) => options.version = Some(n.base10_parse()?),
                        other => return Err(Error::new(other.span(), "expected a number")),
                    },
                    other => return Err(Error::new(other.span(), "expected a number")),
                },
                other => {
                    return Err(Error::new(
                        other.span(),
                        "expected `name = \"...\"`, `skip`, `expansion_safe` or `version = N`",
                    ));
                }
            }
        }
    }
    *attrs = kept;
    if options.version.is_some() && !options.expansion_safe {
        return Err(Error::new(
            Span::call_site(),
            "`version` goes with `expansion_safe`",
        ));
    }
    Ok(options)
}

fn expand_bridge(attr: Tokens, item: Tokens) -> syn::Result<Tokens> {
    let rt = runtime();
    let mut block: ItemImpl = syn::parse2(item)?;
    if let Some((_, path, _)) = &block.trait_ {
        return Err(Error::new(
            path.span(),
            "#[bridge] goes on an inherent impl block",
        ));
    }
    let mut register = format_ident!("bridge");
    for meta in Punctuated::<Meta, Token![,]>::parse_terminated.parse2(attr)? {
        match meta {
            Meta::NameValue(nv) if nv.path.is_ident("register") => match &nv.value {
                Expr::Path(p) if p.path.get_ident().is_some() => {
                    register = p.path.get_ident().expect("checked").clone();
                }
                other => return Err(Error::new(other.span(), "expected a name")),
            },
            other => return Err(Error::new(other.span(), "expected `register = name`")),
        }
    }
    let mut additions = Vec::new();
    for item in &mut block.items {
        let ImplItem::Fn(f) = item else { continue };
        let options = options(&mut f.attrs)?;
        if options.skip {
            continue;
        }
        additions.push(function(f, options)?);
    }
    let self_ty = &block.self_ty;
    let (impl_generics, _, where_clause) = block.generics.split_for_impl();
    Ok(quote! {
        #block
        impl #impl_generics #self_ty #where_clause {
            /// Adds this block's bridged functions and methods to `library`.
            #[allow(unused_mut, unused_unsafe, clippy::all)]
            pub fn #register(library: #rt::NativeLibrary) -> #rt::NativeLibrary {
                // SAFETY: each function returns what its result converts
                // to, through IntoScheme, and keeps no raw value.
                unsafe { library #(#additions)* }
            }
        }
    })
}

/// The builder call that adds `f`.
fn function(f: &syn::ImplItemFn, options: Options) -> syn::Result<Tokens> {
    let rt = runtime();
    let ident = &f.sig.ident;
    let name = options.name.unwrap_or_else(|| kebab(&ident.to_string()));
    let mut kinds = Vec::new();
    let mut takes = Vec::new();
    let mut uses = Vec::new();
    for (i, input) in f.sig.inputs.iter().enumerate() {
        let a = format_ident!("__a{i}");
        let v = format_ident!("__v{i}");
        let param = match input {
            FnArg::Receiver(r) => {
                if r.reference.is_none() {
                    return Err(Error::new(
                        r.span(),
                        "a bridged method takes `&self` or `&mut self`",
                    ));
                }
                Param::Object {
                    ty: syn::parse_quote!(Self),
                    mutable: r.mutability.is_some(),
                }
            }
            FnArg::Typed(t) => {
                if !matches!(&*t.pat, Pat::Ident(_) | Pat::Wild(_)) {
                    return Err(Error::new(t.pat.span(), "expected a parameter name"));
                }
                classify(&t.ty)
            }
        };
        let required = quote!(#rt::bridge::ParamKind::Required);
        match param {
            Param::Text => {
                kinds.push(required);
                takes.push(quote! { let #a = unsafe { __p::text(cx, args, &mut __next) }?; });
                uses.push(quote!(&#a));
            }
            Param::Bytes => {
                kinds.push(required);
                takes.push(quote! { let #a = unsafe { __p::bytes(cx, args, &mut __next) }?; });
                uses.push(quote!(&#a[..]));
            }
            Param::Object { ty, mutable } => {
                kinds.push(required);
                takes.push(quote! {
                    let (#a, #v) = unsafe { __p::borrow::<#ty>(cx, args, &mut __next, #mutable) }?;
                    __held.push(#v);
                });
                uses.push(if mutable {
                    quote!(unsafe { &mut *#a.as_ptr() })
                } else {
                    quote!(unsafe { &*#a.as_ptr() })
                });
            }
            Param::Owned(ty) => {
                kinds.push(quote!(<#ty as #rt::bridge::Param>::KIND));
                takes
                    .push(quote! { let #a = unsafe { __p::take::<#ty>(cx, args, &mut __next) }?; });
                uses.push(quote!(#a));
            }
        }
    }
    let output = match &f.sig.output {
        ReturnType::Default => quote!(()),
        ReturnType::Type(_, ty) => quote!(#ty),
    };
    let mark = if options.expansion_safe {
        let version = options.version.unwrap_or(1);
        quote!(.expansion_safe(#version))
    } else {
        quote!()
    };
    Ok(quote! {
        .raw(
            #name,
            #rt::bridge::__private::arity(&[#(#kinds),*]),
            |cx: &mut dyn #rt::bridge::Context, args: &[#rt::Value]| {
                use #rt::bridge::__private as __p;
                let mut __next = 0usize;
                let mut __held: ::std::vec::Vec<#rt::Value> = ::std::vec::Vec::new();
                // SAFETY: the arguments are the values the bridge was called
                // with, and each borrow is released below, after the call
                // that uses it. Converting the arguments runs inside the
                // guard too, so a panic converting a later one cannot leave
                // an earlier one borrowed.
                let __outcome = __p::guarded(|| -> ::core::result::Result<#output, #rt::Error> {
                    #(#takes)*
                    Ok(Self::#ident(#(#uses),*))
                });
                unsafe { __p::release(cx, &__held) };
                match __outcome {
                    Ok(r) => __p::finish(cx, r?),
                    Err(panic) => __p::resume(panic),
                }
            },
        )
        #mark
    })
}
